//! Fetching bodies in the background, so that mail never opened is found by its text too
//! ([`MailClientBuilder::prefetch_bodies`](crate::MailClientBuilder::prefetch_bodies)).
//!
//! One task per account, started with background sync, on a connection of its own: the
//! pooled ones stay free for opening messages, which never wait for it. It starts once
//! every folder of the account was synced (and after the computer slept, once they were
//! again), fetches the newest bodies first, and waits for news of the account when none
//! is left. Messages in Trash and Junk come after all the others: least looked for, but
//! a search asks the server about any folder with bodies missing.
//!
//! A small message is fetched whole, as when it is opened; a larger one only its text,
//! its attachments left on the server until opened (see [`body`]). What is fetched counts
//! towards a daily budget per account, kept in the database, smaller for servers whose
//! limits aren't known: going over a server's limit locks the account out of IMAP for up
//! to a day, sync and opening mail included. Should the server still say the account went
//! over (Gmail and NetEase say so in words), the prefetch stops for a day, also across
//! restarts. Bodies are stored and announced a batch at a time.
//!
//! A message the server won't hand out is recorded and tried again only after a while,
//! so that it doesn't hold up the rest; several in a row stop the task for a while, as the
//! server is likely refusing more than those. Other failures (the connection, the login)
//! are the account's, and only retried.
//!
//! Like background sync, the task holds the engine weakly: dropping the last client ends
//! it, and with it its connection.
//!
//! New mail is not left to the task, which would log in for it ([`Arrivals`]): the sync
//! that announces it fetches its bodies on its own connection, within the same budget.
//! Neither sees what the other is fetching until it is stored, so together they may go a
//! batch over the budget: well within the margin to the server's limit, and only once, as
//! the next batch or sync reads what was used.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use catmail_proto::imap::{self, Session};
use catmail_store::{NewBody, Usage};
use catmail_types::{AccountId, Event, FolderId, MessageId};
use tokio::sync::broadcast::{
    self,
    error::{RecvError, TryRecvError},
};

use crate::body::{self, Limits};
use crate::content;
use crate::downloading::{Claim, Claimed};
use crate::engine::Engine;
use crate::settings::BodyPrefetch;
use crate::task::{Cancellation, backoff, blocking, now};
use crate::{Error, Result, connection};

const MAX_RETRY: Duration = Duration::from_secs(30 * 60);

/// With nothing to fetch, how long to wait without news of the account before looking
/// again: messages that failed come due again.
const IDLE: Duration = Duration::from_secs(60 * 60);

/// Messages in a row the server may refuse before the task stops for [`IDLE`].
const REFUSED_IN_A_ROW: u32 = 3;

/// How long to stop once the server said the account went over its download limit: the
/// longest Gmail and NetEase say they keep an account locked out.
const OVER_LIMIT: Duration = Duration::from_secs(24 * 60 * 60);

/// Starts fetching `account`'s bodies in the background, if the engine does. Call from
/// inside the engine runtime.
pub(crate) fn spawn(engine: &Arc<Engine>, account: AccountId) {
    if let Some(settings) = engine.settings().prefetch {
        tokio::spawn(task(engine, account, settings));
    }
}

pub(crate) fn task(
    engine: &Arc<Engine>,
    account: AccountId,
    settings: BodyPrefetch,
) -> impl Future<Output = ()> + Send + 'static {
    run(Arc::downgrade(engine), account, settings)
}

/// What to do after a look.
enum Next {
    Now,
    /// Look again once a sync of every folder finished, or on news of the account.
    Synced,
    /// Look again on news of the account, or after this long.
    News(Duration),
    Sleep(Duration),
}

/// How a batch ended.
enum Batch {
    /// There may be more to fetch.
    More,
    Done,
    /// The day's budget is used up.
    Spent,
    /// Nothing is fetched for this long: the server refused message after message, or said
    /// the account went over its download limit (now or before).
    Paused(Duration),
}

/// Fetches one account's bodies until it is removed or the engine is gone.
async fn run(engine: Weak<Engine>, account: AccountId, settings: BodyPrefetch) {
    let Some((mut events, synced, mut cancellation, mut closing)) = engine.upgrade().map(|e| {
        (
            e.subscribe(),
            e.synced_signal(account),
            e.cancellation(account),
            e.closing(),
        )
    }) else {
        return;
    };
    let mut session = None;
    // Folders with a new UIDVALIDITY on the server, left out until there is news of the
    // account: a sync starts them over.
    let mut skip = HashSet::new();
    // Not until a sync of every folder finished after this: the login was rejected then.
    let mut rejected_at = None;
    let mut failures = 0;
    // Messages the server refused in a row, across batches.
    let mut refused = 0;
    // Bodies were fetched since nothing was left last: the next time nothing is, it says so.
    let mut busy = false;
    tracing::info!(%account, whole_limit = settings.whole_limit,
        gmail_daily_budget = settings.gmail_daily_budget,
        daily_budget = settings.daily_budget, "prefetching bodies in the background");
    loop {
        // What happened until now, the look below sees.
        while !matches!(
            events.try_recv(),
            Err(TryRecvError::Empty | TryRecvError::Closed)
        ) {}
        let synced = synced.notified();
        tokio::pin!(synced);
        synced.as_mut().enable();
        let Some(strong) = engine.upgrade() else {
            return;
        };
        if cancellation.is_cancelled() {
            return;
        }
        let next = if !may_start(&strong, account, rejected_at) {
            tracing::debug!(%account, "prefetch waits for every folder to be synced");
            session = None;
            Next::Synced
        } else {
            let result = batch(
                &strong,
                account,
                &settings,
                &mut session,
                &mut skip,
                &mut refused,
                &mut cancellation,
            )
            .await;
            if !matches!(result, Ok(Batch::More)) {
                // Not kept open while there is nothing to do with it.
                session = None;
            }
            if result.is_ok() {
                failures = 0;
            }
            match result {
                Ok(Batch::More) => {
                    busy = true;
                    Next::Now
                }
                Ok(Batch::Done) => {
                    if std::mem::take(&mut busy) {
                        tracing::info!(%account, "every body there is to prefetch is here");
                    }
                    Next::News(IDLE)
                }
                Ok(Batch::Spent) => Next::News(until_tomorrow()),
                Ok(Batch::Paused(wait)) => Next::Sleep(wait),
                Err(Error::AccountNotFound(_) | Error::Cancelled) => return,
                // Each try is another rejected login: wait for one that works.
                Err(Error::AuthFailed(e)) => {
                    tracing::warn!(%account, error = %e,
                        "login rejected; prefetch waits for a sync that gets through");
                    rejected_at = Some(Instant::now());
                    Next::Synced
                }
                Err(e) => {
                    failures += 1;
                    let delay = backoff(settings.retry, failures, MAX_RETRY);
                    tracing::warn!(%account, error = %e, kind = ?e.kind(), failures, ?delay,
                        "prefetch failed; trying again later");
                    Next::Sleep(delay)
                }
            }
        };
        drop(strong);
        let (wait, news, waiting_for_sync) = match next {
            Next::Now => continue,
            // News too: a sync that finishes after a rejected login signals before it
            // reports the login working again, which `may_start` waits for.
            Next::Synced => (IDLE, true, true),
            Next::News(wait) => (wait, true, false),
            Next::Sleep(wait) => (wait, false, false),
        };
        tokio::select! {
            () = cancellation.cancelled() => return,
            () = closing.closed() => return,
            () = tokio::time::sleep(wait) => {}
            () = news_of(&mut events, account), if news => {}
            () = &mut synced, if waiting_for_sync => {}
        }
        // A sync may have started them over.
        skip.clear();
    }
}

/// Whether every folder of the account was synced since the computer last woke (see
/// [`Engine::synced`]), after `rejected_at` if the login was rejected then, and the
/// server neither rejects it now nor told sync the account is over its limit (prefetching
/// would only add to it; a sync getting through again is news).
fn may_start(engine: &Engine, account: AccountId, rejected_at: Option<Instant>) -> bool {
    !engine.waits_out(account)
        && engine
            .synced(account)
            .is_some_and(|at| rejected_at.is_none_or(|rejected| at > rejected))
}

/// Returns once messages of the account changed or its status did, or the engine is gone.
/// Falling behind counts as news.
async fn news_of(events: &mut broadcast::Receiver<Event>, account: AccountId) {
    loop {
        match events.recv().await {
            Ok(
                Event::MessagesChanged { account: a, .. }
                | Event::AccountStatusChanged { account: a },
            ) if a == account => return,
            Ok(_) => {}
            Err(RecvError::Lagged(_) | RecvError::Closed) => return,
        }
    }
}

/// What was fetched in a batch, to be stored together.
#[derive(Default)]
struct Fetched {
    bodies: Vec<(MessageId, FolderId, NewBody)>,
    failed: Vec<MessageId>,
    bytes: u64,
    /// The server said the account went over its download limit.
    over_limit: bool,
}

/// What came of one message.
enum One {
    Body {
        folder: FolderId,
        body: NewBody,
        bytes: u64,
    },
    /// Downloaded meanwhile, or gone.
    Skipped,
    /// Its folder has a new UIDVALIDITY on the server.
    Rebuilt(FolderId),
    /// The server won't hand it out.
    Refused(Error),
    /// The server said the account went over its download limit.
    OverLimit(Error),
}

/// What the account may fetch today.
enum Allowance {
    /// Nothing for this long: the server said the account went over its download limit.
    Paused(Duration),
    /// Until `used` reaches `budget`.
    Open { used: u64, budget: u64, gmail: bool },
}

impl Allowance {
    /// What `settings` allow as of `now`, with `usage` the day's so far, on a Gmail server
    /// or not.
    fn of(settings: &BodyPrefetch, usage: &Usage, gmail: bool, now: i64) -> Self {
        if let Some(until) = usage.paused_until.filter(|&until| until > now) {
            return Self::Paused(Duration::from_secs((until - now) as u64));
        }
        let budget = if gmail {
            settings.gmail_daily_budget
        } else {
            settings.daily_budget
        };
        Self::Open {
            used: usage.bytes,
            budget,
            gmail,
        }
    }
}

/// How much of a message the prefetch fetches: whole up to its own limit, and its text
/// up to the limit for a message the user opens.
fn limits(settings: &BodyPrefetch, engine: &Engine) -> Limits {
    Limits {
        whole: settings.whole_limit,
        text: engine.settings().tuning.whole_message_limit,
    }
}

/// Fetches the next batch of bodies on `session` (connecting if it is empty) and stores
/// them, leaving out the folders in `skip` and adding those found rebuilt, and counting
/// the messages the server refused in a row in `refused`. Stores what it fetched also if
/// it then fails.
async fn batch(
    engine: &Arc<Engine>,
    account: AccountId,
    settings: &BodyPrefetch,
    session: &mut Option<Session>,
    skip: &mut HashSet<FolderId>,
    refused: &mut u32,
    cancellation: &mut Cancellation,
) -> Result<Batch> {
    let (now, day) = (now(), today());
    let (limit, skipped) = (settings.batch, skip.iter().copied().collect::<Vec<_>>());
    let (mut next, usage, gmail) = engine
        .read(move |db| {
            let prefetch = db.prefetch();
            let gmail = db.accounts().get(account)?.map(|a| a.gmail_server);
            Ok((
                prefetch.next(account, &skipped, now, limit)?,
                prefetch.usage(account, day)?,
                gmail,
            ))
        })
        .await?;
    let Some(gmail) = gmail else {
        return Err(Error::AccountNotFound(account.to_string()));
    };
    let (used, budget) = match Allowance::of(settings, &usage, gmail, now) {
        Allowance::Paused(wait) => {
            tracing::info!(%account, until = usage.paused_until, ?wait,
                "prefetch paused: the server said the account went over its download limit");
            return Ok(Batch::Paused(wait));
        }
        Allowance::Open { used, budget, .. } => (used, budget),
    };
    // Being fetched for the user, or as they arrived: stored as they come, they are news
    // when they are. Else a batch of only those would be fetched again and again.
    next.retain(|&(id, _)| !engine.body_claimed(id));
    if next.is_empty() {
        return Ok(Batch::Done);
    }
    // Newest first, but a folder at a time within the batch: each switch opens a folder.
    next.sort_by_key(|&(_, folder)| folder);
    if used >= budget {
        tracing::debug!(%account, used, budget, "today's budget for prefetching is used up");
        return Ok(Batch::Spent);
    }
    let limits = limits(settings, engine);
    let began = Instant::now();
    let mut fetched = Fetched::default();
    let mut result = Ok(Batch::More);
    for (id, _) in next {
        if used + fetched.bytes >= budget {
            result = Ok(Batch::Spent);
            break;
        }
        match one(engine, account, session, id, limits, cancellation).await {
            Ok(One::Body {
                folder,
                body,
                bytes,
            }) => {
                fetched.bodies.push((id, folder, body));
                fetched.bytes += bytes;
                *refused = 0;
            }
            Ok(One::Skipped) => {
                tracing::debug!(%account, message_id = %id, "downloaded meanwhile, or gone");
            }
            Ok(One::Rebuilt(folder)) => {
                tracing::info!(%account, %folder,
                    "folder rebuilt on the server; left out until synced again");
                skip.insert(folder);
            }
            Ok(One::Refused(e)) => {
                tracing::warn!(%account, message_id = %id, error = %e, refused_in_a_row = *refused + 1,
                    "can't prefetch the body; trying it again later");
                fetched.failed.push(id);
                *refused += 1;
                if *refused >= REFUSED_IN_A_ROW {
                    *refused = 0;
                    tracing::warn!(%account, refused = REFUSED_IN_A_ROW, wait = ?IDLE,
                        "the server refuses message after message; prefetch paused");
                    result = Ok(Batch::Paused(IDLE));
                    break;
                }
            }
            Ok(One::OverLimit(e)) => {
                over_limit(account, id, &e, used + fetched.bytes, budget, gmail);
                fetched.over_limit = true;
                result = Ok(Batch::Paused(OVER_LIMIT));
                break;
            }
            Err(e) => {
                tracing::debug!(%account, message_id = %id, error = %e,
                    "prefetch stopped at a failure that isn't the message's");
                result = Err(e);
                break;
            }
        }
    }
    let (count, bytes, failed) = (fetched.bodies.len(), fetched.bytes, fetched.failed.len());
    store(engine, account, now, day, fetched).await?;
    if count > 0 || failed > 0 {
        tracing::info!(%account, count, failed, bytes, used_today = used + bytes, budget,
            took = ?began.elapsed(), "prefetched bodies");
    }
    if matches!(result, Ok(Batch::Spent)) {
        tracing::info!(%account, used_today = used + bytes, budget, gmail,
            "today's budget for prefetching bodies is used up; going on tomorrow (UTC)");
    }
    result
}

fn over_limit(account: AccountId, id: MessageId, e: &Error, used: u64, budget: u64, gmail: bool) {
    tracing::warn!(%account, message_id = %id, error = %e, wait = ?OVER_LIMIT,
        used_today = used, budget, gmail,
        "the server says the account went over its download limit; prefetch paused");
}

/// Fetches and parses one body. A failure that isn't the message's is returned as an
/// error; a connection it leaves unusable is dropped.
async fn one(
    engine: &Arc<Engine>,
    account: AccountId,
    session: &mut Option<Session>,
    id: MessageId,
    limits: Limits,
    cancellation: &mut Cancellation,
) -> Result<One> {
    let Some(location) = engine.read(move |db| db.messages().location(id)).await? else {
        return Ok(One::Skipped);
    };
    // Claimed since the batch was read: stored as it comes, not with this batch.
    if location.downloaded || engine.body_claimed(id) {
        return Ok(One::Skipped);
    }
    let folder = location.folder;
    let result = tokio::select! {
        result = connection::on_session(engine, account, session, {
            let engine = engine.clone();
            async move |s| body::fetch_body(&engine, s, &location, limits).await
        }) => result,
        () = cancellation.cancelled() => Err(Error::Cancelled),
    };
    if let Err(e) = &result
        && e.breaks_connection()
    {
        *session = None;
    }
    outcome(engine, account, id, folder, result).await
}

/// What came of fetching message `id`'s body in `folder`: the body parsed, or the failure
/// sorted. A failure that isn't the message's is returned as an error.
async fn outcome(
    engine: &Arc<Engine>,
    account: AccountId,
    id: MessageId,
    folder: FolderId,
    fetched: Result<(content::Fetched, body::Took)>,
) -> Result<One> {
    match fetched {
        Ok((fetched, took)) => {
            let bytes = fetched.len();
            tracing::debug!(%account, message_id = %id, %folder, bytes,
                whole = matches!(fetched, content::Fetched::Whole(_)), examine = ?took.examine,
                fetch = ?took.fetch, "prefetched a body");
            let storage = engine.storage();
            let body = blocking(move || content::parse(&storage, fetched)).await?;
            Ok(One::Body {
                folder,
                body,
                bytes,
            })
        }
        Err(e @ Error::OverLimit(_)) => Ok(One::OverLimit(e)),
        Err(e) if body::folder_rebuilt(&e) => Ok(One::Rebuilt(folder)),
        Err(Error::OutOfSync(_) | Error::MessageNotFound(_)) => Ok(One::Skipped),
        // Refused, or left out of the answer while the server has it.
        Err(e) if e.is_refusal() || matches!(e.imap(), Some(imap::Error::Protocol(_))) => {
            Ok(One::Refused(e))
        }
        Err(e) => Err(e),
    }
}

/// New mail sync is announcing (`Event::NewMail`), whose bodies it fetches on its own
/// connection once the headers are stored ([`fetch`](Self::fetch)): before the
/// background task would (it logs in anew for each, as it keeps no connection while
/// idle), so that the message opens at once from its notification.
///
/// Claimed (see [`Downloading`](crate::downloading::Downloading)) as they are announced, before
/// the background task hears of them: opening one waits for this fetch rather than make
/// its own, and the background task leaves them out. At most a batch, the first
/// announced first: the rest of a burst is the background task's. None unless the engine
/// prefetches.
pub(crate) struct Arrivals {
    claimed: Vec<(MessageId, Claimed)>,
    room: usize,
}

impl Arrivals {
    pub fn new(engine: &Engine) -> Self {
        Self {
            claimed: Vec::new(),
            room: engine
                .settings()
                .prefetch
                .map_or(0, |settings| settings.batch),
        }
    }

    /// Claims those of `messages` there is room for. Call before announcing them.
    pub fn claim(&mut self, engine: &Engine, messages: &[MessageId]) {
        for &id in messages {
            if self.claimed.len() >= self.room {
                return;
            }
            if let Claim::Mine(claimed) = engine.claim_body(id) {
                self.claimed.push((id, claimed));
            }
        }
    }

    /// Fetches and stores the bodies claimed on `session`, which has their folder open
    /// (saving a login and an EXAMINE), each stored as it comes and released then. Within
    /// the prefetch's budget, and recording refusals as it does.
    ///
    /// Fails only for what leaves the connection unusable: sync goes on with it
    /// otherwise, and the rest is the background task's.
    pub async fn fetch(
        self,
        engine: &Arc<Engine>,
        session: &mut Session,
        account: AccountId,
    ) -> Result<()> {
        let Some(settings) = engine.settings().prefetch else {
            return Ok(());
        };
        if self.claimed.is_empty() || engine.waits_out(account) {
            return Ok(());
        }
        let (now, day) = (now(), today());
        let (usage, gmail) = engine
            .read(move |db| {
                let gmail = db.accounts().get(account)?.map(|a| a.gmail_server);
                Ok((db.prefetch().usage(account, day)?, gmail))
            })
            .await?;
        let Some(gmail) = gmail else {
            return Ok(());
        };
        let Allowance::Open {
            mut used,
            budget,
            gmail,
        } = Allowance::of(&settings, &usage, gmail, now)
        else {
            return Ok(());
        };
        let limits = limits(&settings, engine);
        for (id, _claimed) in self.claimed {
            if used >= budget {
                tracing::debug!(%account, used, budget,
                    "today's budget for prefetching is used up; new mail waits to be opened");
                return Ok(());
            }
            let Some(location) = engine.read(move |db| db.messages().location(id)).await? else {
                continue;
            };
            if location.downloaded {
                continue;
            }
            let folder = location.folder;
            let result = body::fetch_body(engine, session, &location, limits).await;
            let mut fetched = Fetched::default();
            // What leaves the connection unusable, which sync is to hear of.
            let broke = match outcome(engine, account, id, folder, result).await {
                Ok(One::Body {
                    folder,
                    body,
                    bytes,
                }) => {
                    fetched.bodies.push((id, folder, body));
                    fetched.bytes = bytes;
                    None
                }
                Ok(One::Skipped | One::Rebuilt(_)) => None,
                Ok(One::Refused(e)) => {
                    tracing::warn!(%account, message_id = %id, error = %e,
                        "can't fetch new mail's body; trying it again later");
                    fetched.failed.push(id);
                    e.breaks_connection().then_some(e)
                }
                Ok(One::OverLimit(e)) => {
                    over_limit(account, id, &e, used, budget, gmail);
                    fetched.over_limit = true;
                    Some(e)
                }
                Err(e) if e.breaks_connection() => Some(e),
                Err(e) => {
                    tracing::info!(%account, message_id = %id, error = %e,
                        "couldn't fetch new mail's body; left to the prefetch");
                    None
                }
            };
            let bytes = fetched.bytes;
            let stored = !fetched.bodies.is_empty();
            store(engine, account, now, day, fetched).await?;
            used += bytes;
            if stored {
                tracing::info!(%account, message_id = %id, bytes, used_today = used, budget,
                    "fetched new mail's body as it arrived");
            }
            if let Some(e) = broke {
                return Err(e);
            }
        }
        Ok(())
    }
}

/// Stores a batch in one transaction, then announces the folders whose messages changed.
async fn store(
    engine: &Arc<Engine>,
    account: AccountId,
    now: i64,
    day: i64,
    fetched: Fetched,
) -> Result<()> {
    if fetched.bodies.is_empty() && fetched.failed.is_empty() && !fetched.over_limit {
        return Ok(());
    }
    let changed = engine
        .write(move |db| {
            let messages = db.messages();
            let mut changed = BTreeSet::new();
            for (id, folder, body) in &fetched.bodies {
                // Not if it was opened meanwhile, or is gone.
                if messages.store_body(*id, body)? {
                    changed.insert(*folder);
                }
            }
            // On Gmail, the labels (and Starred) showing them.
            changed.extend(messages.touched_labels());
            for &id in &fetched.failed {
                db.prefetch().failed(id, now)?;
            }
            db.prefetch().add_used(account, day, fetched.bytes)?;
            if fetched.over_limit {
                let until = now + OVER_LIMIT.as_secs() as i64;
                db.prefetch().pause(account, until)?;
            }
            Ok(changed)
        })
        .await?;
    for folder in changed {
        // Snippets and attachment markers changed.
        engine.emit(Event::MessagesChanged { account, folder });
    }
    Ok(())
}

const DAY: i64 = 24 * 60 * 60;

/// Today, in UTC days since the epoch.
fn today() -> i64 {
    now().div_euclid(DAY)
}

/// How long until the next UTC day.
fn until_tomorrow() -> Duration {
    Duration::from_secs((DAY - now().rem_euclid(DAY)) as u64)
}
