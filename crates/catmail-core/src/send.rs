//! The outbox: queueing drafts, sending them over SMTP, saving them to Sent.
//!
//! Queueing builds the whole message once and stores it as a blob; every attempt sends
//! exactly those bytes. An entry is worked on under the account's send permit, one
//! step at a time, each step committed before the next starts:
//!
//! - `queued` → `sending` (counted as an attempt) → MAIL / RCPT / DATA and the message
//!   body. A failure here can't have delivered anything: back to `queued` with backoff,
//!   or `failed` if the server refused for good.
//! - `unknown` is committed *before* the final dot. The server's 250 makes it
//!   `accepted`; a refusal makes it `queued` or `failed`; anything else (lost
//!   connection, timeout, crash) leaves it `unknown`, and only the user resends it.
//! - `accepted` → APPEND to the Sent folder (at once, then retried with backoff),
//!   checking first whether an earlier APPEND arrived after all → done: the entry and
//!   its draft go.

use std::sync::{Arc, Weak};
use std::time::Duration;

use catmail_proto::imap::Session;
use catmail_proto::smtp;
use catmail_store::{NewOutboxEntry, Outbox, OutboxJob, Touched};
use catmail_types::{
    AccountId, Address, BlobHash, DraftId, ErrorKind, Event, Flags, FolderRole, OutboxEntry,
    OutboxId, OutboxStatus,
};
use tokio::sync::Notify;

use crate::connection::{self, with_session};
use crate::engine::{Activity, Closing, Engine, Permit, blocking, now};
use crate::{Error, Result, body, drafts, ops, sync};

/// First retry after this long, doubling up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: i64 = 30;
const MAX_BACKOFF: i64 = 30 * 60;

/// Freezes the draft into an outbox entry: the message is built and stored now, with
/// its Message-ID and Date, and the draft is locked until the entry is done or
/// cancelled.
pub(crate) async fn queue_draft(engine: &Arc<Engine>, id: DraftId) -> Result<OutboxEntry> {
    let (snapshot, account) = engine
        .read(move |db| {
            let Some(snapshot) = db.drafts().snapshot(id)? else {
                return Ok(None);
            };
            let account = db.accounts().get(snapshot.draft.account_id)?;
            Ok(Some((snapshot, account)))
        })
        .await?
        .ok_or(Error::DraftNotFound(id))?;
    let account_id = snapshot.draft.account_id;
    let account = account.ok_or_else(|| Error::AccountNotFound(account_id.to_string()))?;
    if let Some(outbox) = snapshot.draft.outbox {
        return Err(Error::Conflict(format!(
            "draft {id} is already in the outbox (entry {outbox})"
        )));
    }
    // The account's own address goes into MAIL FROM and the Message-ID.
    drafts::check_address(&account.email)?;
    let content = &snapshot.draft.content;
    drafts::validate(content)?;
    let recipients = envelope_recipients(content.recipients());
    if recipients.is_empty() {
        return Err(Error::InvalidInput("the draft has no recipients".into()));
    }

    let message_id = drafts::new_message_id(&account.email);
    let (revision, subject) = (snapshot.revision, content.subject.clone());
    let envelope_from = account.email.clone();
    let engine2 = engine.clone();
    let message_id2 = message_id.clone();
    let raw = blocking(move || {
        // Bcc only in the envelope.
        let raw = drafts::build(&engine2, &account, &snapshot, &message_id2, false)?;
        Ok(engine2.blobs.put_bytes(&raw)?)
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
    engine.outbox_kick.notify_one();
    Ok(entry)
}

/// Works through the account's outbox: sends what is queued, saving each message to
/// Sent as soon as it is accepted, then saves what an earlier pass couldn't. With
/// `due_only`, only entries whose backoff has run out. Returns the entries worked on
/// (postponed ones included), as they ended up (`Done` for those that are gone).
pub(crate) async fn flush(
    engine: Arc<Engine>,
    account: AccountId,
    due_only: bool,
) -> Result<Vec<OutboxEntry>> {
    let permit = idle_outbox(&engine, account).await?;
    permit.run(run(&engine, account, due_only)).await
}

async fn run(engine: &Arc<Engine>, account: AccountId, due_only: bool) -> Result<Vec<OutboxEntry>> {
    let mut results = Vec::new();
    let outcome = pass(engine, account, due_only, &mut results).await;
    // What changed before a failure too.
    if !results.is_empty() || outcome.is_err() {
        engine.emit(Event::OutboxChanged { account });
    }
    outcome.map(|()| results)
}

/// [`run`], adding the entries worked on to `results` as it goes.
async fn pass(
    engine: &Arc<Engine>,
    account: AccountId,
    due_only: bool,
    results: &mut Vec<OutboxEntry>,
) -> Result<()> {
    let pending = |status| {
        let due = due_only.then(now);
        engine.read(move |db| db.outbox().pending(account, status, due))
    };
    let mut smtp = Smtp::None;
    for id in pending(OutboxStatus::Queued).await? {
        if let Smtp::None = smtp {
            // Before the entry counts an attempt: failing to connect or log in is the
            // account's trouble, not this message's.
            let connected = connection::connect_smtp(engine, account).await;
            let failures = engine.report(account, Activity::Send, &connected);
            smtp = match connected {
                Ok(session) => Smtp::Up(session),
                Err(e @ (Error::Cancelled | Error::AccountNotFound(_))) => return Err(e),
                Err(e) => {
                    tracing::info!(%account, error = %e, "can't reach the SMTP server");
                    Smtp::Down {
                        error: e.to_string(),
                        next: now() + backoff(failures),
                    }
                }
            };
        }
        let session = match &mut smtp {
            Smtp::Up(session) => session,
            // Everything waits as long as the account's backoff says.
            Smtp::Down { error, next } => {
                let (error, next) = (error.clone(), *next);
                let postponed = record(engine, id, move |o| o.postpone(id, next, &error, now()));
                results.extend(postponed.await?);
                continue;
            }
            Smtp::None => unreachable!("connected above"),
        };
        let Some(job) = engine
            .write(move |db| db.outbox().begin_send(id, now()))
            .await?
        else {
            continue; // Cancelled meanwhile.
        };
        let (entry, usable) = send_one(engine, session, job).await?;
        results.extend(entry);
        if !usable {
            smtp = Smtp::None;
        }
    }
    if let Smtp::Up(session) = smtp {
        session.quit().await;
    }

    // Saves that failed in an earlier pass; this pass's were tried already.
    for id in pending(OutboxStatus::Accepted).await? {
        if results.iter().any(|e: &OutboxEntry| e.id == id) {
            continue;
        }
        let Some(job) = engine
            .write_durable(move |db| db.outbox().begin_save(id, now()))
            .await?
        else {
            continue;
        };
        results.extend(save_one(engine, job, None).await?);
    }
    Ok(())
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

/// The SMTP connection of one pass over the outbox, reused from message to message.
enum Smtp {
    /// Not connected (yet, or again after a failure made the connection unusable).
    None,
    Up(smtp::Session),
    /// Connecting failed, which ends the sending part of the pass: why, and when to try
    /// again.
    Down {
        error: String,
        next: i64,
    },
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
                    Ok((db.outbox().begin_save(id, now)?, replied))
                })
                .await?;
            engine.emit(Event::MessageSent {
                account,
                outbox: id,
            });
            ops::announce(engine, &replied);
            match save {
                Some(job) => save_one(engine, job, Some(raw)).await,
                None => engine.read(move |db| db.outbox().get(id)).await,
            }
        }
        Outcome::NotSent(e) if is_permanent(&e) => {
            tracing::warn!(%id, error = %e, "message refused");
            let error = e.to_string();
            record(engine, id, move |o| o.fail(id, &error, now)).await
        }
        Outcome::NotSent(e) => {
            tracing::info!(%id, error = %e, "sending failed; will retry");
            let (next, error) = (now + backoff(attempts), e.to_string());
            record(engine, id, move |o| o.requeue(id, next, &error, now)).await
        }
        Outcome::Unknown(e) => {
            tracing::warn!(%id, error = %e, "no answer to the end of the message; outcome unknown");
            let error = e.to_string();
            record(engine, id, move |o| o.mark_unknown(id, Some(&error), now)).await
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
        .write_durable(move |db| db.outbox().mark_unknown(id, None, now()))
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
            tracing::info!(%id, error = %e, "saving to Sent failed; will retry");
            let (next, error) = (now + backoff(job.entry.attempts), e.to_string());
            record(engine, id, move |o| o.save_later(id, next, &error, now)).await
        }
    }
}

/// APPENDs the message to the account's Sent folder, unless an earlier attempt that
/// went unanswered put it there already. Without a Sent folder there is nothing to do.
async fn attempt_save(
    engine: &Arc<Engine>,
    job: &OutboxJob,
    mut raw: Option<Vec<u8>>,
) -> Result<()> {
    let account = job.entry.account_id;
    let known = engine
        .read(move |db| db.folders().by_role(account, FolderRole::Sent))
        .await?
        .map(|(_, remote_name)| remote_name);
    // Attempts are counted before they are made, so past the first one (or after a
    // retry on a new connection within this one) an APPEND may have gone through.
    let mut may_be_there = job.entry.attempts > 1;
    let (engine2, hash, message_id) = (
        engine.clone(),
        job.raw.clone(),
        job.entry.message_id.clone(),
    );
    let saved = with_session(engine, account, async move |session| {
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

/// The Sent folder by the server's say, for an account that hasn't been synced.
async fn find_sent(session: &mut Session) -> Result<Option<String>> {
    Ok(sync::remote_folders(session.list().await?)
        .into_iter()
        .find(|f| f.role == Some(FolderRole::Sent) && f.selectable)
        .map(|f| f.remote_name))
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

/// Seconds to wait after the `attempts`-th failed attempt.
fn backoff(attempts: u32) -> i64 {
    let secs = |s: i64| Duration::from_secs(s as u64);
    crate::engine::backoff(secs(FIRST_BACKOFF), attempts, secs(MAX_BACKOFF)).as_secs() as i64
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
    engine.outbox_kick.notify_one();
    Ok(entry)
}

/// The user confirmed that a message with an unknown outcome was delivered.
pub(crate) async fn mark_sent(engine: &Arc<Engine>, id: OutboxId) -> Result<OutboxEntry> {
    let entry = user_decision(engine, id, move |o| o.mark_sent(id, now())).await?;
    engine.outbox_kick.notify_one();
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
    ops::announce(engine, &touched);
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
    let due = now();
    let accounts = engine.read(move |db| db.outbox().accounts_due(due)).await?;
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
            Err(e) => failed = Some(crate::runtime::join_error(e)),
        }
    }
    match failed {
        Some(e) => Err(e),
        None => engine.read(|db| db.outbox().next_attempt_at()).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
