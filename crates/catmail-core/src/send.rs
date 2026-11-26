//! The outbox: queueing drafts, sending them over SMTP, saving them to Sent.
//!
//! Queueing builds the whole message once and stores it as a blob; every attempt sends
//! exactly those bytes. An entry is worked on under the account's send permit, one
//! step at a time, each step committed before the next starts:
//!
//! - `queued` → `sending` (counted as an attempt) → MAIL / RCPT / DATA and the message
//!   body. A failure here can't have delivered anything: back to `queued` with backoff,
//!   or `failed` if the server refused for good or this was the [`MAX_ATTEMPTS`]th.
//! - `unknown` is committed *before* the final dot. The server's 250 makes it
//!   `accepted`; a refusal makes it `queued` or `failed`; anything else (lost
//!   connection, timeout, crash) leaves it `unknown`, and only the user resends it.
//! - `accepted` → APPEND to the Sent folder (at once, then retried with backoff),
//!   checking first whether an earlier APPEND arrived after all → done: the entry and
//!   its draft go. `unsaved` after [`MAX_ATTEMPTS`] failures, for the user. Gmail saves what its own SMTP server sends to Sent itself: no APPEND.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use catmail_proto::imap::Session;
use catmail_proto::smtp;
use catmail_store::{DraftSnapshot, NewOutboxEntry, Outbox, OutboxJob, Touched};
use catmail_types::{
    Account, AccountId, Address, BlobHash, Draft, DraftId, ErrorKind, Event, Flags, FlushReport,
    FolderRole, Identity, IdentityId, OutboxEntry, OutboxId, OutboxStatus, ReportedError,
};
use futures_util::future::join_all;
use tokio::sync::Notify;

use crate::connection::with_session;
use crate::connection::{self, SendAs, SmtpLogin};
use crate::engine::{Engine, Permit};
use crate::status::Activity;
use crate::task::{Closing, blocking, now};
use crate::{Error, Result, addresses, body, drafts, events, sync};

/// First retry after this long, doubling up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: i64 = 30;
const MAX_BACKOFF: i64 = 30 * 60;
/// Attempts at a step (sending, saving to Sent) before the user is told it failed,
/// instead of retrying for as long as the failure lasts: one that fails three times in
/// a row is seldom about to work, and the user can try again once it might.
const MAX_ATTEMPTS: u32 = 3;
/// After the computer woke, the first retry of a server that can't be reached is this
/// much later (see [`Failures::retry_in`](crate::status::Failures::retry_in)).
const RETRY_AFTER_WAKE: i64 = 2;

/// A draft as queueing it now would send it, for the user to confirm before someone
/// other than its editor (an agent) sends it: see
/// [`MailClient::preview_send`](crate::MailClient::preview_send).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendPreview {
    pub draft: Draft,
    pub from: Address,
    /// What [`MailClient::send_previewed`](crate::MailClient::send_previewed) takes back.
    pub revision: DraftRevision,
}

/// A version of a draft: changing what it says, its attachments or the identity it is
/// sent as makes a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DraftRevision(i64);

/// The draft as it would be sent, failing where queueing it not as its editor would.
pub(crate) async fn preview(engine: &Arc<Engine>, id: DraftId) -> Result<SendPreview> {
    let (snapshot, sender) = sending(engine, id).await?;
    sendable(id, &snapshot, &sender)?;
    drafts::not_open_elsewhere(&engine.editing(), id, None)?;
    Ok(SendPreview {
        revision: DraftRevision(snapshot.revision),
        draft: snapshot.draft,
        from: sender,
    })
}

/// The draft with what building its message needs, and the address it is sent from.
async fn sending(engine: &Arc<Engine>, id: DraftId) -> Result<(DraftSnapshot, Address)> {
    engine
        .read(move |db| connection::draft_sender(db, id))
        .await?
        .ok_or(Error::DraftNotFound(id))
}

/// The draft's envelope recipients, if it can be queued.
fn sendable(id: DraftId, snapshot: &DraftSnapshot, sender: &Address) -> Result<Vec<String>> {
    if let Some(outbox) = snapshot.draft.outbox {
        return Err(Error::Conflict(format!(
            "draft {id} is already in the outbox (entry {outbox})"
        )));
    }
    // The sender's address goes into MAIL FROM and the Message-ID.
    addresses::check_address(&sender.email)?;
    let content = &snapshot.draft.content;
    addresses::validate(content)?;
    let recipients = envelope_recipients(content.recipients());
    if recipients.is_empty() {
        return Err(Error::InvalidInput("the draft has no recipients".into()));
    }
    Ok(recipients)
}

/// Freezes the draft into an outbox entry: the message is built and stored now, with
/// its Message-ID and Date, and the draft is locked until the entry is done or
/// cancelled. Refused while an editor other than `editor` (the draft it edits) has it
/// open, and with `previewed`, unless the draft is still that revision: what was
/// confirmed is what is sent. Both are checked in the transaction that queues it.
pub(crate) async fn queue_draft(
    engine: &Arc<Engine>,
    id: DraftId,
    editor: Option<DraftId>,
    previewed: Option<DraftRevision>,
) -> Result<OutboxEntry> {
    let (snapshot, sender) = sending(engine, id).await?;
    let account_id = snapshot.draft.account_id;
    if previewed.is_some_and(|r| r.0 != snapshot.revision) {
        return Err(Error::Conflict(format!(
            "draft {id} changed since it was shown; it has to be confirmed again"
        )));
    }
    let recipients = sendable(id, &snapshot, &sender)?;
    let editing = engine.editing();
    // Before building the message as well: it reads every attachment.
    drafts::not_open_elsewhere(&editing, id, editor)?;

    let message_id = drafts::new_message_id(&sender.email);
    let (revision, subject) = (snapshot.revision, snapshot.draft.content.subject.clone());
    let envelope_from = sender.email.clone();
    let raw = blocking({
        let (storage, message_id) = (engine.storage(), message_id.clone());
        move || {
            let raw = drafts::build(
                &storage,
                &sender,
                &snapshot,
                &message_id,
                drafts::Destination::Smtp,
            )?;
            storage.put_bytes(&raw)
        }
    })
    .await?;
    let entry = NewOutboxEntry {
        account: account_id,
        draft: id,
        revision,
        raw,
        message_id,
        subject,
        envelope_from,
        recipients,
    };
    let entry = engine
        .write(move |db| {
            drafts::not_open_elsewhere(&editing, id, editor)?;
            let id = db.outbox().queue(&entry, now())?;
            db.outbox().get(id)
        })
        .await?
        .expect("just queued");
    // Nothing more for background sync to hold back: the outbox has it now.
    engine.draft_gone(account_id, id);
    engine.emit(Event::DraftsChanged {
        account: account_id,
    });
    engine.emit(Event::OutboxChanged {
        account: account_id,
    });
    engine.kick_outbox();
    Ok(entry)
}

/// Works through the account's outbox: sends what is queued, saving each message to
/// Sent as soon as it is accepted, then saves what an earlier pass couldn't. With
/// `due_only`, only entries whose backoff has run out. Not reaching the SMTP server
/// postpones the queued entries and is in the report, not returned: the caller still
/// learns what became of them.
pub(crate) async fn flush(
    engine: Arc<Engine>,
    account: AccountId,
    due_only: bool,
) -> Result<FlushReport> {
    let permit = idle_outbox(&engine, account).await?;
    permit.run(run(&engine, account, due_only)).await
}

async fn run(engine: &Arc<Engine>, account: AccountId, due_only: bool) -> Result<FlushReport> {
    let mut report = FlushReport::default();
    let outcome = pass(engine, account, due_only, &mut report).await;
    // What changed before a failure too.
    if !report.entries.is_empty() || outcome.is_err() {
        engine.emit(Event::OutboxChanged { account });
    }
    outcome.map(|()| report)
}

/// [`run`], filling in `report` as it goes.
async fn pass(
    engine: &Arc<Engine>,
    account: AccountId,
    due_only: bool,
    report: &mut FlushReport,
) -> Result<()> {
    let pending = |status| {
        let due = due_only.then(now);
        engine.read(move |db| db.outbox().pending(account, status, due))
    };
    // Each login's messages on their own, all at once: a server that doesn't answer
    // holds up only what goes through it.
    let routes = routes(engine, account, pending(OutboxStatus::Queued).await?).await?;
    let sent = join_all(routes.into_iter().map(|route| async move {
        let mut report = FlushReport::default();
        let outcome = send_route(engine, account, route, &mut report).await;
        (report, outcome)
    }))
    .await;
    let mut outcome = Ok(());
    for (part, result) in sent {
        report.entries.extend(part.entries);
        report.errors.extend(part.errors);
        outcome = outcome.and(result);
    }
    report.entries.sort_by_key(|e| e.id);
    outcome?;

    // Saves that failed in an earlier pass; this pass's were tried already.
    for (id, _) in pending(OutboxStatus::Accepted).await? {
        if report.entries.iter().any(|e| e.id == id) {
            continue;
        }
        let Some(job) = engine
            .write_durable(move |db| db.outbox().begin_save(id))
            .await?
        else {
            continue;
        };
        report.entries.extend(save_one(engine, job, None).await?);
    }
    Ok(())
}

/// Queued entries that go out over one SMTP login, oldest first.
struct Route {
    /// Why there is none, for the identity that has no login.
    login: std::result::Result<SmtpLogin, (SendAs, Error)>,
    entries: Vec<OutboxId>,
}

/// The queued entries, by the login they go out over.
async fn routes(
    engine: &Arc<Engine>,
    account: AccountId,
    queued: Vec<(OutboxId, IdentityId)>,
) -> Result<Vec<Route>> {
    let mut identities: Vec<_> = queued.iter().map(|&(_, identity)| identity).collect();
    identities.sort_unstable();
    identities.dedup();
    let logins = connection::smtp_logins(engine, account, identities).await?;
    let mut route_of = HashMap::new();
    for (route, login) in logins.iter().enumerate() {
        match login {
            Ok(login) => route_of.extend(login.identities().map(|i| (i.id, route))),
            Err((identity, _)) => route_of.extend([(identity.id, route)]),
        }
    }
    let mut routes: Vec<_> = logins
        .into_iter()
        .map(|login| Route {
            login,
            entries: Vec::new(),
        })
        .collect();
    for (id, identity) in queued {
        routes[route_of[&identity]].entries.push(id);
    }
    Ok(routes)
}

/// Sends a route's entries over one connection, reused from message to message.
async fn send_route(
    engine: &Arc<Engine>,
    account: AccountId,
    route: Route,
    report: &mut FlushReport,
) -> Result<()> {
    let mut login = match route.login {
        Ok(login) => login,
        Err((identity, e)) => {
            if let Smtp::Down(down) = connected(engine, account, &[identity], Err(e), report)? {
                for id in route.entries {
                    down.postpone(engine, id, report).await?;
                }
            }
            return Ok(());
        }
    };
    let identities: Vec<_> = login.identities().cloned().collect();
    let mut smtp = Smtp::None;
    for id in route.entries {
        if let Smtp::None = smtp {
            // Before the entry counts an attempt: failing to connect or log in is the
            // sender's trouble, not this message's.
            let session = login.connect(engine).await;
            smtp = connected(engine, account, &identities, session, report)?;
        }
        let session = match &mut smtp {
            Smtp::Up(session) => session,
            Smtp::Down(down) => {
                down.postpone(engine, id, report).await?;
                continue;
            }
            Smtp::None => unreachable!("connected above"),
        };
        let Some(job) = engine.write(move |db| db.outbox().begin_send(id)).await? else {
            continue; // Cancelled meanwhile.
        };
        let (entry, usable) = send_one(engine, session, job).await?;
        report.entries.extend(entry);
        if !usable {
            smtp = Smtp::None;
        }
    }
    if let Smtp::Up(session) = smtp {
        session.quit().await;
    }
    Ok(())
}

/// Records for each of `identities` how connecting for them ended, and returns the
/// connection they now have.
fn connected(
    engine: &Arc<Engine>,
    account: AccountId,
    identities: &[SendAs],
    connected: Result<smtp::Session>,
    report: &mut FlushReport,
) -> Result<Smtp> {
    // A login shared since its last failure waits as long as the most failed of its
    // identities.
    let secs = |s: i64| Duration::from_secs(s as u64);
    let retry_in = identities
        .iter()
        .map(|i| engine.report(account, Activity::Send(i.id, &i.email), &connected))
        .map(|f| {
            f.retry_in(
                secs(RETRY_AFTER_WAKE),
                secs(FIRST_BACKOFF),
                secs(MAX_BACKOFF),
            )
        })
        .max()
        .unwrap_or_default();
    match connected {
        Ok(session) => Ok(Smtp::Up(session)),
        Err(e @ (Error::Cancelled | Error::AccountNotFound(_))) => Err(e),
        Err(e) => {
            let ids: Vec<_> = identities.iter().map(|i| i.id).collect();
            tracing::info!(%account, identities = ?ids, error = %e, "can't reach the SMTP server");
            let error = ReportedError::from(&e);
            for identity in identities {
                report.errors.insert(identity.id, error.clone());
            }
            Ok(Smtp::Down(Down {
                error: error.message,
                next: now() + retry_in.as_secs() as i64,
            }))
        }
    }
}

/// The account's send permit, with nothing in the middle of being sent. An entry that
/// is still `sending` once the permit is ours belongs to an attempt that ended without
/// recording how (a failed write, a cancelled flush). It never got to the end of its
/// message, which would have made it `unknown`, so it goes back to the queue.
async fn idle_outbox(engine: &Arc<Engine>, account: AccountId) -> Result<Permit> {
    let permit = engine.send_permit(account).await;
    let requeued = engine
        .write(move |db| db.outbox().recover_interrupted(Some(account), now()))
        .await?;
    if requeued > 0 {
        tracing::info!(%account, requeued, "requeued sends that were left unfinished");
        engine.emit(Event::OutboxChanged { account });
    }
    Ok(permit)
}

/// The SMTP connection of a route in one pass over the outbox.
enum Smtp {
    /// Not connected (yet, or again after a failure made the connection unusable).
    None,
    Up(smtp::Session),
    /// Connecting failed, which ends the route's part of the pass.
    Down(Down),
}

/// Why connecting failed, and when to try again.
struct Down {
    error: String,
    next: i64,
}

impl Down {
    /// Everything the route sends waits as long as the sender's backoff says.
    async fn postpone(
        &self,
        engine: &Arc<Engine>,
        id: OutboxId,
        report: &mut FlushReport,
    ) -> Result<()> {
        let (error, next) = (self.error.clone(), self.next);
        let postponed = record(engine, id, move |o| o.postpone(id, next, &error));
        report.entries.extend(postponed.await?);
        Ok(())
    }
}

/// How an attempt at sending ended.
enum Outcome {
    /// With the message, for saving it to Sent.
    Accepted(Vec<u8>),
    /// Nothing can have been delivered: try again later, or give up for good.
    NotSent(Error),
    /// Lost after the end of the message was sent.
    Unknown(Error),
    /// The entry went away (its account is being removed); nothing to record.
    Gone,
}

/// Applies a transition and returns the entry as it is now stored, `None` if it is gone.
async fn record(
    engine: &Arc<Engine>,
    id: OutboxId,
    transition: impl FnOnce(&Outbox<'_>) -> catmail_store::Result<bool> + Send + 'static,
) -> Result<Option<OutboxEntry>> {
    engine
        .write(move |db| {
            transition(&db.outbox())?;
            db.outbox().get(id)
        })
        .await
}

/// The entry as the attempt left it; one the server accepted is saved to Sent right
/// away, with the message still at hand. Also returns whether `session` can be used
/// again.
async fn send_one(
    engine: &Arc<Engine>,
    session: &mut smtp::Session,
    job: OutboxJob,
) -> Result<(Option<OutboxEntry>, bool)> {
    let (id, account, attempts) = (job.entry.id, job.entry.account_id, job.entry.attempts);
    let now = now();
    let (outcome, usable) = attempt_send(engine, session, &job).await;
    let entry = match outcome {
        Outcome::Accepted(raw) => {
            // Durable like every count of an attempt to save: see `attempt_save`.
            let (save, replied) = engine
                .write_durable(move |db| {
                    let replied = db.outbox().mark_accepted(id, now)?.unwrap_or_default();
                    Ok((db.outbox().begin_save(id)?, replied))
                })
                .await?;
            tracing::info!(%id, %account, "message sent");
            engine.emit(Event::MessageSent {
                account,
                outbox: id,
            });
            events::announce(engine, &replied);
            match save {
                Some(job) => save_one(engine, job, Some(raw)).await,
                None => engine.read(move |db| db.outbox().get(id)).await,
            }
        }
        Outcome::NotSent(e) => {
            let error = e.to_string();
            match next_attempt(now, attempts).filter(|_| !is_permanent(&e)) {
                Some(next) => {
                    tracing::info!(%id, %error, "sending failed; will retry");
                    record(engine, id, move |o| o.requeue(id, next, &error)).await
                }
                None => {
                    tracing::warn!(%id, %attempts, %error, "message not sent; giving up");
                    record(engine, id, move |o| o.fail(id, &error)).await
                }
            }
        }
        Outcome::Unknown(e) => {
            tracing::warn!(%id, error = %e, "no answer to the end of the message; outcome unknown");
            let error = e.to_string();
            record(engine, id, move |o| o.mark_unknown(id, Some(&error))).await
        }
        Outcome::Gone => Ok(None),
    };
    Ok((entry?, usable))
}

/// One attempt on the pass's connection. Also returns whether it can be used again.
async fn attempt_send(
    engine: &Arc<Engine>,
    session: &mut smtp::Session,
    job: &OutboxJob,
) -> (Outcome, bool) {
    match read_raw(engine, job.raw.clone()).await {
        Ok(raw) => transact(engine, session, job, raw).await,
        Err(e) => (Outcome::NotSent(e), true),
    }
}

/// One SMTP transaction. Also returns whether the connection can be used again.
async fn transact(
    engine: &Arc<Engine>,
    session: &mut smtp::Session,
    job: &OutboxJob,
    raw: Vec<u8>,
) -> (Outcome, bool) {
    let sent = async {
        session
            .begin(&job.envelope_from, &job.entry.recipients, &raw)
            .await?;
        session.write_message(&raw).await
    };
    if let Err(e) = sent.await {
        let usable = !e.is_fatal();
        return (Outcome::NotSent(e.into()), usable);
    }
    // From here on the server may accept the message at any moment.
    let id = job.entry.id;
    // On disk before the end marker goes out: a commit lost to a power cut would leave
    // the entry `sending`, which is sent again.
    match engine
        .write_durable(move |db| db.outbox().mark_unknown(id, None))
        .await
    {
        Ok(true) => {}
        // The server discards the message once the connection drops without the end
        // marker.
        Ok(false) => return (Outcome::Gone, false),
        Err(e) => return (Outcome::NotSent(e), false),
    }
    match session.end_message().await {
        Ok(()) => (Outcome::Accepted(raw), true),
        // A reply, even a refusal, is a verdict: the server has dropped the message.
        Err(e @ smtp::Error::Reply { .. }) => (Outcome::NotSent(e.into()), true),
        Err(e) => (Outcome::Unknown(e.into()), false),
    }
}

/// `raw` is the message if the caller has it at hand. Returns the entry as it ended
/// up, `None` if it is gone.
async fn save_one(
    engine: &Arc<Engine>,
    job: OutboxJob,
    raw: Option<Vec<u8>>,
) -> Result<Option<OutboxEntry>> {
    let id = job.entry.id;
    let now = now();
    match attempt_save(engine, &job, raw).await {
        Ok(()) => {
            if !engine.write(move |db| db.outbox().finish(id)).await? {
                return Ok(None);
            }
            tracing::info!(%id, "saved to Sent");
            // The draft is gone, and its copy on the server goes next.
            drafts::changed(engine, job.entry.account_id);
            Ok(Some(OutboxEntry {
                status: OutboxStatus::Done,
                next_attempt_at: None,
                last_error: None,
                ..job.entry
            }))
        }
        Err(e) => {
            let (attempts, error) = (job.entry.attempts, e.to_string());
            match next_attempt(now, attempts) {
                Some(next) => {
                    tracing::info!(%id, %error, "saving to Sent failed; will retry");
                    record(engine, id, move |o| o.save_later(id, next, &error)).await
                }
                None => {
                    tracing::warn!(%id, %attempts, %error, "saving to Sent failed; giving up");
                    record(engine, id, move |o| o.give_up_saving(id, &error)).await
                }
            }
        }
    }
}

/// APPENDs the message to the account's Sent folder, unless an earlier attempt that
/// went unanswered put it there already, or the server put it there itself. Without a
/// Sent folder there is nothing to do.
async fn attempt_save(
    engine: &Arc<Engine>,
    job: &OutboxJob,
    mut raw: Option<Vec<u8>>,
) -> Result<()> {
    let (account, identity) = (job.entry.account_id, job.entry.identity);
    let (known, mut sender) = engine
        .read(move |db| {
            let known = db.folders().by_role(account, FolderRole::Sent)?;
            let sender = db
                .accounts()
                .get(account)?
                .zip(db.identities().get(identity)?);
            Ok((known.map(|(_, remote_name)| remote_name), sender))
        })
        .await?;
    if server_saves_sent(&sender) {
        return Ok(());
    }
    // Attempts are counted before they are made, so past the first one (or after a
    // retry on a new connection within this one) an APPEND may have gone through.
    let mut may_be_there = job.entry.attempts > 1;
    let (engine2, hash, message_id) = (
        engine.clone(),
        job.raw.clone(),
        job.entry.message_id.clone(),
    );
    let saved = with_session(engine, account, async move |session| {
        // As connecting recorded it: an account added without logging in to it may not
        // have been connected to before.
        if let Some((account, _)) = &mut sender {
            account.gmail_server = session.is_gmail();
        }
        if server_saves_sent(&sender) {
            return Ok(true);
        }
        let check_first = std::mem::replace(&mut may_be_there, true);
        let sent = match &known {
            Some(name) => Some(name.clone()),
            None => find_sent(session).await?,
        };
        let Some(sent) = sent else {
            return Ok(false);
        };
        if check_first {
            // Searched as it is now: a copy under an earlier UIDVALIDITY is there all the
            // same, so there is nothing to compare.
            session.examine(&sent).await?;
            if !session.uid_search_message_id(&message_id).await?.is_empty() {
                tracing::debug!(%message_id, "already in Sent");
                return Ok(true);
            }
        }
        // Read only now that it is needed, and kept for a retry on a new connection.
        let raw = match &mut raw {
            Some(raw) => raw,
            slot @ None => slot.insert(read_raw(&engine2, hash.clone()).await?),
        };
        session.append(&sent, Flags::SEEN, raw).await?;
        Ok(true)
    })
    .await?;
    if !saved {
        tracing::info!(%account, "the account has no Sent folder; not saving a copy");
    }
    Ok(())
}

/// Whether the identity's server saves what it sends to the account's Sent itself (see
/// [`Identity::server_saves_sent`]).
fn server_saves_sent(sender: &Option<(Account, Identity)>) -> bool {
    let Some((account, identity)) = sender else {
        return false;
    };
    let saves = identity.server_saves_sent(account);
    if saves {
        tracing::debug!(account = %account.id, "the server saves sent mail to Sent itself");
    }
    saves
}

/// The Sent folder by the server's say, for an account that hasn't been synced.
async fn find_sent(session: &mut Session) -> Result<Option<String>> {
    let listed = session.list().await?;
    let roles = sync::roles(&listed, sync::mode_of(session, &listed));
    Ok(listed
        .into_iter()
        .zip(roles)
        .find(|(m, role)| *role == Some(FolderRole::Sent) && m.selectable)
        .map(|(m, _)| m.raw_name))
}

/// The message as it was queued.
async fn read_raw(engine: &Arc<Engine>, hash: BlobHash) -> Result<Vec<u8>> {
    body::read_blob(engine, hash).await?.ok_or_else(|| {
        Error::Conflict(
            "the stored message is missing; cancel this entry and send the draft again".into(),
        )
    })
}

/// Trying again can't help: the server refused the message for good, or the message is
/// wrong, or the stored message is missing (a conflict). Network trouble and 4xx replies
/// are temporary, and so is trouble on this machine. (What the account's settings get
/// wrong fails logging in, before any attempt.)
fn is_permanent(e: &Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::Refused | ErrorKind::Invalid | ErrorKind::Conflict
    ) && !e.is_transient()
}

/// When to try a step again after its `attempts`-th attempt failed for now, or `None`
/// once that was the last: the user is told instead.
fn next_attempt(now: i64, attempts: u32) -> Option<i64> {
    (attempts < MAX_ATTEMPTS).then(|| now + backoff(attempts))
}

/// Seconds to wait after the `attempts`-th failed attempt.
fn backoff(attempts: u32) -> i64 {
    let secs = |s: i64| Duration::from_secs(s as u64);
    crate::task::backoff(secs(FIRST_BACKOFF), attempts, secs(MAX_BACKOFF)).as_secs() as i64
}

/// Every address once (ignoring case), in order.
fn envelope_recipients<'a>(addresses: impl IntoIterator<Item = &'a Address>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    addresses
        .into_iter()
        .filter(|a| seen.insert(a.email.to_ascii_lowercase()))
        .map(|a| a.email.clone())
        .collect()
}

/// The user asks for another attempt now.
pub(crate) async fn retry(engine: &Arc<Engine>, id: OutboxId) -> Result<OutboxEntry> {
    let entry = user_decision(engine, id, move |o| {
        Ok(o.retry(id, now())?.map(|e| (e, Touched::new())))
    })
    .await?;
    engine.kick_outbox();
    Ok(entry)
}

/// The user confirmed that a message with an unknown outcome was delivered.
pub(crate) async fn mark_sent(engine: &Arc<Engine>, id: OutboxId) -> Result<OutboxEntry> {
    let entry = user_decision(engine, id, move |o| o.mark_sent(id, now())).await?;
    engine.kick_outbox();
    Ok(entry)
}

/// Returns the entry as it was before it was removed.
pub(crate) async fn cancel(engine: &Arc<Engine>, id: OutboxId) -> Result<OutboxEntry> {
    // Waiting its turn, it goes at once, not after whatever the pass sends before it:
    // by then the pass would have sent it too.
    let entry = match engine.write(move |db| db.outbox().withdraw(id)).await? {
        Some(entry) => {
            engine.emit(Event::OutboxChanged {
                account: entry.account_id,
            });
            entry
        }
        None => {
            user_decision(engine, id, move |o| {
                Ok(o.cancel(id)?.map(|e| (e, Touched::new())))
            })
            .await?
        }
    };
    // Editable again, and maybe changed since its copy on the server was made.
    drafts::changed(engine, entry.account_id);
    Ok(entry)
}

/// Applies what the user decided about an entry, once no attempt is working on it.
/// `decide` also returns the messages it changed (marking a reply's original answered).
async fn user_decision(
    engine: &Arc<Engine>,
    id: OutboxId,
    decide: impl FnOnce(&Outbox<'_>) -> catmail_store::Result<Option<(OutboxEntry, Touched)>>
    + Send
    + 'static,
) -> Result<OutboxEntry> {
    let _permit = decision_permit(engine, id).await?;
    let (entry, touched) = engine
        .write(move |db| decide(&db.outbox()))
        .await?
        .ok_or(Error::OutboxNotFound(id))?;
    engine.emit(Event::OutboxChanged {
        account: entry.account_id,
    });
    events::announce(engine, &touched);
    Ok(entry)
}

/// Waits for work on the entry's outbox to pause. A user's decision applies to where an
/// attempt left the entry: an `unknown` one may well be accepted the next moment, and
/// changing it meanwhile would leave the attempt unable to record that.
async fn decision_permit(engine: &Arc<Engine>, id: OutboxId) -> Result<Permit> {
    let account = engine
        .read(move |db| db.outbox().get(id))
        .await?
        .ok_or(Error::OutboxNotFound(id))?
        .account_id;
    idle_outbox(engine, account).await
}

/// Sends whatever comes due, for as long as the engine exists. Holds the engine only
/// while working, so it never keeps the data directory locked on its own.
pub(crate) async fn worker(engine: Weak<Engine>, kick: Arc<Notify>, mut closing: Closing) {
    loop {
        let Some(strong) = engine.upgrade() else {
            return;
        };
        let wait = match work_due(&strong).await {
            Ok(Some(next)) => (next - now()).max(1),
            Ok(None) => i64::MAX,
            Err(e) => {
                tracing::warn!(error = %e, "outbox worker failed; trying again in a minute");
                60
            }
        };
        drop(strong);
        let wait = Duration::from_secs(wait.min(MAX_BACKOFF) as u64);
        tokio::select! {
            () = kick.notified() => {}
            () = tokio::time::sleep(wait) => {}
            () = closing.closed() => return,
        }
    }
}

/// Works on every account with something due, all at once: one unreachable server
/// doesn't hold up the others. Returns when the next entry is due.
async fn work_due(engine: &Arc<Engine>) -> Result<Option<i64>> {
    // Every wait is a `backoff`, so none is longer than MAX_BACKOFF.
    let (brought_forward, accounts) = engine
        .write(move |db| {
            // Read holding the write connection: a wait committed before was set from
            // this time or an earlier one, so only a clock set back puts it further ahead.
            let due = now();
            let outbox = db.outbox();
            Ok((
                outbox.bring_forward(due, MAX_BACKOFF)?,
                outbox.accounts_due(due)?,
            ))
        })
        .await?;
    if brought_forward > 0 {
        tracing::info!(brought_forward, "retries set before the clock went back");
    }
    let mut flushes = tokio::task::JoinSet::new();
    for account in accounts {
        let engine = engine.clone();
        flushes.spawn(async move { (account, flush(engine, account, true).await) });
    }
    let mut failed = None;
    while let Some(joined) = flushes.join_next().await {
        match joined {
            Ok((_, Ok(_) | Err(Error::Cancelled | Error::AccountNotFound(_)))) => {}
            Ok((account, Err(e))) => {
                tracing::warn!(%account, error = %e, "working on the outbox failed");
                failed = Some(e);
            }
            Err(e) => failed = Some(crate::task::join_error(e)),
        }
    }
    match failed {
        Some(e) => Err(e),
        None => engine.read(|db| db.outbox().next_attempt_at()).await,
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::*;
    use crate::client::fixtures::{client_with_account, draft_to_b, open_client, queue_one};
    use crate::{MemorySecrets, SecretStore};

    #[test]
    fn backs_off_exponentially_up_to_a_cap() {
        let waits: Vec<_> = (1..=8).map(backoff).collect();
        assert_eq!(waits, [30, 60, 120, 240, 480, 960, 1800, 1800]);
        assert_eq!(backoff(0), 30);
        assert_eq!(backoff(u32::MAX), MAX_BACKOFF);
    }

    #[test]
    fn envelope_has_every_recipient_once() {
        let a = |email: &str| Address {
            name: None,
            email: email.into(),
        };
        let list = [a("x@example.com"), a("y@example.com"), a("X@Example.com")];
        assert_eq!(
            envelope_recipients(&list),
            ["x@example.com", "y@example.com"]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sends_interrupted_before_the_end_go_back_to_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let (client, account) =
            client_with_account(dir.path(), secrets.clone(), "a@example.com").await;
        let mut entries = Vec::new();
        for _ in 0..2 {
            entries.push(queue_one(&client, account).await.id);
        }
        // The process dies sending both: the first before the end of the message, the
        // second after.
        client
            .engine()
            .unwrap()
            .storage()
            .write(|db| {
                db.outbox().begin_send(entries[0])?;
                db.outbox().begin_send(entries[1])?;
                db.outbox().mark_unknown(entries[1], None)
            })
            .unwrap();
        drop(client);

        let client = open_client(dir.path(), secrets);
        let statuses: Vec<_> = client
            .list_outbox(None)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.status)
            .collect();
        assert_eq!(
            statuses,
            [
                catmail_types::OutboxStatus::Queued,
                catmail_types::OutboxStatus::Unknown
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sends_left_unfinished_are_requeued_without_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let (client, account) = client_with_account(dir.path(), secrets, "a@example.com").await;
        let entry = queue_one(&client, account).await;
        // An attempt that couldn't record how it ended, e.g. a failed write.
        client
            .engine()
            .unwrap()
            .storage()
            .write(|db| db.outbox().begin_send(entry.id))
            .unwrap();

        let cancelled = client.cancel_outbox(entry.id).await.unwrap();
        assert_eq!(cancelled.status, catmail_types::OutboxStatus::Queued);
        assert!(client.list_outbox(None).await.unwrap().is_empty());
    }

    /// Like a Secret Service whose collection is locked: works once unlocked.
    #[derive(Default)]
    struct LockableSecrets {
        inner: MemorySecrets,
        locked: std::sync::atomic::AtomicBool,
    }

    impl SecretStore for LockableSecrets {
        fn set(&self, key: &str, secret: &SecretString) -> Result<()> {
            self.inner.set(key, secret)
        }
        fn get(&self, key: &str) -> Result<Option<SecretString>> {
            if self.locked.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(Error::Secrets("collection is locked".into()));
            }
            self.inner.get(key)
        }
        fn delete(&self, key: &str) -> Result<()> {
            self.inner.delete(key)
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn locked_keyring_postpones_sending() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(LockableSecrets::default());
        let (client, account) =
            client_with_account(dir.path(), secrets.clone(), "a@example.com").await;
        queue_one(&client, account).await;
        secrets
            .locked
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // Fails before any connection is made, so the fake host doesn't matter.
        let flushed = client.flush_outbox(account).await.unwrap();
        assert_eq!(flushed.entries.len(), 1);
        assert_eq!(
            flushed.entries[0].status,
            catmail_types::OutboxStatus::Queued
        );
        let error = flushed.entries[0].last_error.as_deref().unwrap();
        assert!(error.contains("locked"));
        assert_eq!(flushed.errors.into_values().next().unwrap().message, error);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn account_address_is_checked_before_queueing() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let (client, account) = client_with_account(dir.path(), secrets, "张三@example.com").await;
        let draft = draft_to_b(&client, account).await;
        let queued = client.send_draft(draft.id).await;
        assert!(matches!(queued, Err(Error::InvalidInput(_))), "{queued:?}");
        assert!(client.list_outbox(None).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_previewed_send_goes_out_only_as_it_was_shown() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let (client, account) = client_with_account(dir.path(), secrets, "a@example.com").await;
        let draft = draft_to_b(&client, account).await;
        let shown = client.preview_send(draft.id).await.unwrap();
        assert_eq!(shown.from.email, "a@example.com");

        // Changed after it was shown: what was confirmed isn't what would go out.
        let changes = catmail_types::DraftChanges {
            subject: Some("changed".into()),
            ..Default::default()
        };
        client.update_draft(draft.id, changes).await.unwrap();
        let queued = client.send_previewed(draft.id, shown.revision).await;
        assert!(matches!(queued, Err(Error::Conflict(_))), "{queued:?}");

        // Open in an editor, where the user may be changing it.
        let shown = client.preview_send(draft.id).await.unwrap();
        let (_editor, mark, _) = client.edit_draft(draft.id).await.unwrap();
        let queued = client.send_previewed(draft.id, shown.revision).await;
        assert!(matches!(queued, Err(Error::Conflict(_))), "{queued:?}");
        let previewed = client.preview_send(draft.id).await;
        assert!(
            matches!(previewed, Err(Error::Conflict(_))),
            "{previewed:?}"
        );
        let sent = client.send_draft(draft.id).await;
        assert!(matches!(sent, Err(Error::Conflict(_))), "{sent:?}");
        assert!(client.list_outbox(None).await.unwrap().is_empty());

        drop(mark);
        let entry = client
            .send_previewed(draft.id, shown.revision)
            .await
            .unwrap();
        assert_eq!(entry.subject, "changed");
        let again = client.preview_send(draft.id).await;
        assert!(matches!(again, Err(Error::Conflict(_))), "{again:?}");
    }
}
