//! Fetching bodies in the background, so that mail never opened is found by its text too
//! ([`MailClientBuilder::prefetch_bodies`](crate::MailClientBuilder::prefetch_bodies)).
//!
//! One task per account, started with background sync, on a connection of its own: the
//! pooled ones stay free for opening messages, which never wait for it. It starts once
//! every folder of the account was synced (and after the computer slept, once they were
//! again), fetches the newest bodies first, and waits for news of the account when none
//! is left. Messages in Trash and Junk are left out.
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

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use catmail_proto::imap::{self, Session};
use catmail_store::NewBody;
use catmail_types::{AccountId, Event, FolderId, MessageId};
use tokio::sync::broadcast::{
    self,
    error::{RecvError, TryRecvError},
};

use crate::body::{self, Limits};
use crate::engine::{Cancellation, Engine, backoff, blocking, now};
use crate::{Error, Result, pool};

/// How the background prefetch of bodies goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyPrefetch {
    /// Bytes fetched per account and day (UTC) on a Gmail server, at most [1 GiB]. Gmail
    /// suspends IMAP for an hour up to a day past 2500 MB downloaded in one, counting
    /// other clients and messages opened, and the day it counts needn't be UTC's.
    pub gmail_daily_budget: u64,
    /// The same on other servers [200 MiB]. Most don't publish a limit; NetEase's (163,
    /// 126) is known to exist, and to be hit by syncing a few thousand messages.
    pub daily_budget: u64,
    /// Messages up to this size (RFC822.SIZE) are fetched whole, attachments and all;
    /// above it, only their text [256 KiB]. Most mail is smaller, and a message fetched
    /// whole opens offline with everything in it.
    pub whole_limit: u32,
    /// Bodies stored in one transaction and announced together [20].
    pub batch: usize,
    /// How long to wait after the connection or the login failed before trying again,
    /// doubling with each failure in a row, up to 30 minutes [30 seconds].
    pub retry: Duration,
}

impl Default for BodyPrefetch {
    fn default() -> Self {
        Self {
            gmail_daily_budget: 1024 * 1024 * 1024,
            daily_budget: 200 * 1024 * 1024,
            whole_limit: 256 * 1024,
            batch: 20,
            retry: Duration::from_secs(30),
        }
    }
}

/// The longest wait after a failure.
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
    if let Some(settings) = engine.prefetch {
        tokio::spawn(task(engine, account, settings));
    }
}

/// The task that fetches `account`'s bodies.
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
    /// Look again after this long.
    Sleep(Duration),
}

/// How a batch ended.
enum Batch {
    /// There may be more to fetch.
    More,
    /// Nothing left.
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
            e.events.subscribe(),
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
/// server doesn't reject it now.
fn may_start(engine: &Engine, account: AccountId, rejected_at: Option<Instant>) -> bool {
    !engine.login_rejected(account)
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
    let used = usage.bytes;
    if let Some(until) = usage.paused_until.filter(|&until| until > now) {
        let wait = Duration::from_secs((until - now) as u64);
        tracing::info!(%account, until, ?wait,
            "prefetch paused: the server said the account went over its download limit");
        return Ok(Batch::Paused(wait));
    }
    if next.is_empty() {
        return Ok(Batch::Done);
    }
    // Newest first, but a folder at a time within the batch: each switch opens a folder.
    next.sort_by_key(|&(_, folder)| folder);
    let budget = if gmail {
        settings.gmail_daily_budget
    } else {
        settings.daily_budget
    };
    if used >= budget {
        tracing::debug!(%account, used, budget, "today's budget for prefetching is used up");
        return Ok(Batch::Spent);
    }
    let limits = Limits {
        whole: settings.whole_limit,
        text: engine.tuning.whole_message_limit,
    };
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
                tracing::warn!(%account, message_id = %id, error = %e, wait = ?OVER_LIMIT,
                    used_today = used + fetched.bytes, budget, gmail,
                    "the server says the account went over its download limit; prefetch paused");
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
    if location.downloaded {
        return Ok(One::Skipped);
    }
    let (folder, size) = (location.folder, location.size);
    let result = tokio::select! {
        result = pool::on_session(engine, account, session, {
            let engine = engine.clone();
            async move |s| body::fetch_body(&engine, s, &location, limits).await
        }) => result,
        () = cancellation.cancelled() => Err(Error::Cancelled),
    };
    match result {
        Ok((fetched, took)) => {
            let bytes = fetched.len();
            tracing::debug!(%account, message_id = %id, %folder, size = ?size, bytes,
                whole = matches!(fetched, body::Fetched::Whole(_)), examine = ?took.examine,
                fetch = ?took.fetch, "prefetched a body");
            let engine = engine.clone();
            let body = blocking(move || body::parse(&engine, fetched)).await?;
            Ok(One::Body {
                folder,
                body,
                bytes,
            })
        }
        Err(e) => {
            // Whatever an over-limit came with (the login, opening the folder, the fetch),
            // the connection is no use until it is lifted.
            let over_limit = e.is_over_limit();
            if over_limit || e.imap().is_some_and(imap::Error::is_fatal) {
                *session = None;
            }
            if over_limit {
                return Ok(One::OverLimit(e));
            }
            if body::folder_rebuilt(&e) {
                return Ok(One::Rebuilt(folder));
            }
            match e {
                Error::OutOfSync(_) | Error::MessageNotFound(_) => Ok(One::Skipped),
                // Refused, or left out of the answer while the server has it.
                e if e.is_refusal() || matches!(e.imap(), Some(imap::Error::Protocol(_))) => {
                    Ok(One::Refused(e))
                }
                e => Err(e),
            }
        }
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
            let mut changed = BTreeSet::new();
            for (id, folder, body) in &fetched.bodies {
                // Not if it was opened meanwhile, or is gone.
                if db.messages().store_body(*id, body)? {
                    changed.insert(*folder);
                }
            }
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
