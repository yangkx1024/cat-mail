//! Keeping accounts up to date in the background
//! ([`MailClientBuilder::sync_in_background`](crate::MailClientBuilder::sync_in_background)).
//!
//! One task per account, with one connection, started when the engine opens or the
//! account is added. It syncs every folder now and then, and in
//! between watches INBOX: with IDLE the server says when something changes, without it
//! INBOX is checked at an interval. Queued local changes, and changes to drafts, wake
//! it to make them on the server at once; a changed draft only once it has been left
//! alone for the engine's draft upload delay, while it may still be being written. Its work takes the account's sync permit
//! like any sync, but its waiting doesn't: a sync or replay the user asks for meanwhile
//! runs on a connection of its own. The first sync of every folder is the exception: a
//! sync asked for while it runs waits for it rather than logging in to do the same.
//!
//! After the computer slept ([`Engine::resume`]) a task drops a connection made before,
//! which is likely dead, without a word, and syncs every folder unless a sync of them all
//! began since.
//!
//! The tasks hold the engine weakly, so dropping the last client ends them, and with
//! them their connections.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use catmail_proto::imap::{ChangedFlags, Session};
use catmail_types::AccountId;
use tokio::sync::Notify;

use crate::draft_sync::Uploads;
use crate::engine::{Activity, Cancellation, Engine, FirstSync, backoff};
use crate::{Error, Result, connection, ops, sync};

/// When background sync does what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncSchedule {
    /// Every folder is synced this often; in between only INBOX is watched
    /// [15 minutes].
    pub full_sync: Duration,
    /// How often INBOX is checked on a server that can't announce changes (no IDLE)
    /// [2 minutes].
    pub poll: Duration,
    /// How long one IDLE lasts before it is started over, so that a connection that died
    /// without a word (a NAT box forgetting it, say) is noticed [10 minutes].
    pub idle: Duration,
    /// How long to wait after a failure before trying again, doubling with each failure
    /// in a row, up to `full_sync` [30 seconds]. Queued local changes try again at once,
    /// except after the server rejected the login: that waits `full_sync`, whatever is
    /// queued, or until a sync asked for gets through.
    pub retry: Duration,
}

impl Default for SyncSchedule {
    fn default() -> Self {
        Self {
            full_sync: Duration::from_secs(15 * 60),
            poll: Duration::from_secs(2 * 60),
            idle: Duration::from_secs(10 * 60),
            retry: Duration::from_secs(30),
        }
    }
}

/// Starts keeping `account` in sync, if the engine syncs in the background. Call from
/// inside the engine runtime.
pub(crate) fn spawn(engine: &Arc<Engine>, account: AccountId) {
    if let Some(schedule) = engine.sync_in_background {
        tokio::spawn(task(engine, account, schedule));
    }
}

/// The task that keeps `account` in sync, its first sync of every folder marked as
/// running already.
pub(crate) fn task(
    engine: &Arc<Engine>,
    account: AccountId,
    schedule: SyncSchedule,
) -> impl Future<Output = ()> + Send + 'static {
    let first_sync = engine.start_first_sync(account);
    run(Arc::downgrade(engine), account, schedule, first_sync)
}

/// An IDLE that loses its connection sooner than this after it began counts towards
/// backing off; so does one that ends with a change this soon, past [`BUSY`].
const SHORT_IDLE: Duration = Duration::from_secs(60);

/// IDLEs in a row that may end with a change soon after they began before syncing INBOX
/// waits a little in between. Without it, a server that reports something at once every
/// time (or sends a line that can't be parsed, which counts as a change) would have
/// INBOX synced in a loop.
const BUSY: u32 = 10;

/// IDLEs in a row the server may end on its own as soon as they began, without a change,
/// before INBOX is checked at intervals instead.
const CUT_SHORT: u32 = 3;

/// How an IDLE ended.
enum Idled {
    /// The server reported a change.
    Changed,
    /// Local changes are queued, the account is removed or the engine is gone, or the
    /// next step is due (every folder to be synced, or a held-back draft to be uploaded).
    Stopped,
    /// The server ends each IDLE as soon as it begins.
    CutShort,
}

/// What the connection does between steps.
enum Wait {
    /// IDLE on INBOX, which the step left selected.
    Idle,
    /// Nothing; check again later.
    Poll,
    /// As `Poll`, but INBOX couldn't be synced: flag changes IDLE reported are still to
    /// be fetched.
    InboxFailed,
}

/// Keeps one account in sync until it is removed or the engine is gone.
async fn run(
    engine: Weak<Engine>,
    account: AccountId,
    schedule: SyncSchedule,
    first_sync: FirstSync,
) {
    let mut first_sync = Some(first_sync);
    // An idle connection if there is one: adding an account leaves the one that checked
    // the password.
    let Some((wake, resume, mut cancellation, mut session)) = engine.upgrade().map(|e| {
        let session = e.take_session(account);
        (
            e.wake_signal(account),
            e.resume_signal(account),
            e.cancellation(account),
            session,
        )
    }) else {
        return;
    };
    // IDLEs in a row that the connection was lost in soon after they began.
    let mut idle_losses = 0;
    // IDLEs in a row that ended with a change soon after they began.
    let mut busy = 0;
    // Cleared if the server refuses IDLE even though it offers it, or cuts it short.
    let mut idle_works = true;
    // Flags in INBOX that IDLE reported changed and no step has fetched yet.
    let mut changed_flags = ChangedFlags::default();
    loop {
        let Some(strong) = engine.upgrade() else {
            return;
        };
        if cancellation.is_cancelled() {
            return;
        }
        let first = first_sync.take();
        let result = step(&strong, &mut session, account, schedule, &changed_flags).await;
        drop(first);
        if matches!(result, Ok(Wait::Idle | Wait::Poll)) {
            changed_flags = ChangedFlags::default();
        }
        let failures = strong.report(account, Activity::Sync, &result);
        // Drafts held back while they are being written are uploaded once they settle:
        // the step runs again by then, at once for one that settled while it ran.
        let full = full_sync_in(&strong, account, &schedule);
        let due = strong
            .next_draft_due(account)
            .map_or(full, |drafts| full.min(drafts));
        drop(strong);

        let pause = match result {
            Ok(_) if due.is_zero() => continue,
            Ok(wait) => match (wait, session.as_mut()) {
                (Wait::Idle, Some(s)) if idle_works => {
                    let (start, opened) = (Instant::now(), s.opened());
                    let idled = tokio::select! {
                        idled = idle(s, &wake, &mut cancellation, &schedule, due) => Some(idled),
                        () = slept_through(&engine, &resume, opened) => None,
                    };
                    // Not ended with DONE: after a sleep the answer may never come.
                    let Some(idled) = idled else {
                        tracing::debug!(%account, "IDLE connection from before a sleep; starting over");
                        session = None;
                        (idle_losses, busy) = (0, 0);
                        continue;
                    };
                    match idled {
                        Ok(Idled::Stopped) => {
                            (idle_losses, busy) = (0, 0);
                            continue;
                        }
                        Ok(Idled::Changed) => {
                            add_changed_flags(&mut changed_flags, s.changed_flags());
                            idle_losses = 0;
                            busy = if start.elapsed() < SHORT_IDLE {
                                busy + 1
                            } else {
                                0
                            };
                            if busy <= BUSY {
                                continue;
                            }
                            let delay = backoff(schedule.retry, busy - BUSY, schedule.poll);
                            tracing::debug!(%account, ?delay, "INBOX keeps changing; slowing down");
                            delay
                        }
                        Ok(Idled::CutShort) => {
                            tracing::info!(%account,
                                "server ends IDLE at once; checking INBOX at intervals");
                            idle_works = false;
                            continue;
                        }
                        Err(catmail_proto::imap::Error::Command { text, .. }) => {
                            tracing::info!(%account, %text,
                                "server refused IDLE; checking INBOX at intervals");
                            idle_works = false;
                            continue;
                        }
                        // Servers and NAT boxes close connections that idled a while:
                        // reconnect at once. One lost again and again soon after
                        // IDLE begins backs off like a failure, or this would log
                        // in and sync in a loop.
                        Err(e) => {
                            session = None;
                            let short = start.elapsed() < SHORT_IDLE;
                            idle_losses = if short { idle_losses + 1 } else { 1 };
                            if idle_losses == 1 {
                                tracing::debug!(%account, error = %e, "IDLE connection lost");
                                continue;
                            }
                            let delay =
                                backoff(schedule.retry, idle_losses - 1, schedule.full_sync);
                            tracing::warn!(%account, error = %e, ?delay,
                                "IDLE connection keeps getting lost; trying again later");
                            delay
                        }
                    }
                }
                _ => schedule.poll.min(due),
            },
            Err(Error::AccountNotFound(_) | Error::Cancelled) => return,
            Err(e) => {
                session = None;
                let delay = match e {
                    // Each try is another rejected login, which servers count towards
                    // locking the account.
                    Error::AuthFailed(_) => schedule.full_sync,
                    _ => backoff(schedule.retry, failures, schedule.full_sync),
                };
                tracing::warn!(%account, error = %e, ?delay,
                    "background sync failed; trying again later");
                delay
            }
        };
        tokio::select! {
            () = cancellation.cancelled() => {}
            () = unless_rejected(&engine, account, &wake) => {}
            () = unless_rejected(&engine, account, &resume) => {}
            () = tokio::time::sleep(pause) => {}
        }
    }
}

/// Returns once `signal` is notified while the server doesn't reject the login, or the
/// engine is gone: neither queued changes nor waking from sleep bring on another rejected
/// login (see [`Engine::wake`]), not even when notified before the rejection was known.
async fn unless_rejected(engine: &Weak<Engine>, account: AccountId, signal: &Notify) {
    loop {
        signal.notified().await;
        if engine.upgrade().is_none_or(|e| !e.login_rejected(account)) {
            return;
        }
    }
}

/// Returns once the computer woke from sleep since the connection `opened` (see
/// [`Engine::resume`]), or the engine is gone.
async fn slept_through(engine: &Weak<Engine>, resume: &Notify, opened: Instant) {
    loop {
        resume.notified().await;
        if engine.upgrade().is_none_or(|e| !e.since_woke(opened)) {
            return;
        }
    }
}

/// IDLEs until the server reports a change, local changes are queued, the account is
/// removed or the engine is gone, or the next step is due (in `due`: every folder to be
/// synced, or a draft to be uploaded). An IDLE that ends quietly is started over: its
/// round trip showed the connection alive.
async fn idle(
    session: &mut Session,
    wake: &Notify,
    cancellation: &mut Cancellation,
    schedule: &SyncSchedule,
    due: Duration,
) -> catmail_proto::imap::Result<Idled> {
    let start = Instant::now();
    let mut cut_short = 0;
    loop {
        let left = due.saturating_sub(start.elapsed());
        let mut stop = false;
        let stopping = async {
            stopped(wake, cancellation).await;
            stop = true;
        };
        let (began, max) = (Instant::now(), schedule.idle.min(left));
        if session.idle(stopping, max).await? {
            return Ok(Idled::Changed);
        }
        if stop || start.elapsed() >= due {
            return Ok(Idled::Stopped);
        }
        // Ended quietly long before it was due: by the server.
        cut_short = if began.elapsed() < SHORT_IDLE.min(max / 2) {
            cut_short + 1
        } else {
            0
        };
        if cut_short >= CUT_SHORT {
            return Ok(Idled::CutShort);
        }
    }
}

/// Resolves once local changes are queued, the account is removed or the engine is gone.
async fn stopped(wake: &Notify, cancellation: &mut Cancellation) {
    tokio::select! {
        () = wake.notified() => {}
        () = cancellation.cancelled() => {}
    }
}

/// Adds flag changes IDLE `reported` to those no step has fetched yet.
fn add_changed_flags(pending: &mut ChangedFlags, reported: &ChangedFlags) {
    match (&mut *pending, reported) {
        (ChangedFlags::Uids(uids), ChangedFlags::Uids(more)) => {
            uids.extend(more);
            uids.sort_unstable();
            uids.dedup();
        }
        (_, ChangedFlags::Unknown) => *pending = ChangedFlags::Unknown,
        (ChangedFlags::Unknown, ChangedFlags::Uids(_)) => {}
    }
}

/// How long until every folder is due to be synced again; zero if it is.
fn full_sync_in(engine: &Engine, account: AccountId, schedule: &SyncSchedule) -> Duration {
    engine.synced(account).map_or(Duration::ZERO, |t| {
        schedule.full_sync.saturating_sub(t.elapsed())
    })
}

/// Syncs every folder if that is due, else makes queued local changes on the server
/// (changed drafts included); then syncs INBOX, refreshing the flags `changed_flags`
/// names. On the connection there, or a new one (see
/// [`on_session`](connection::on_session)).
async fn step(
    engine: &Arc<Engine>,
    session: &mut Option<Session>,
    account: AccountId,
    schedule: SyncSchedule,
    changed_flags: &ChangedFlags,
) -> Result<Wait> {
    // Connected before taking the permit: waiting for an unreachable server shouldn't
    // hold up syncs the user asks for.
    connection::on_session(engine, account, session, {
        let (engine, changed_flags) = (engine.clone(), changed_flags.clone());
        async move |s| work(&engine, s, account, &schedule, &changed_flags).await
    })
    .await
}

async fn work(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    schedule: &SyncSchedule,
    changed_flags: &ChangedFlags,
) -> Result<Wait> {
    // The drafts that settled by now are uploaded below; those that settle later keep
    // the next step due.
    engine.forget_settled_drafts(account);
    let full = full_sync_in(engine, account, schedule).is_zero();
    if full {
        // Takes the permit folder by folder, so syncs asked for meanwhile go in between.
        let report = sync::run(engine, session, account, Uploads::Settled).await?;
        engine.first_sync_done(account, &report);
    }
    let permit = engine.sync_permit(account).await;
    permit
        .run(async {
            // A full sync replayed them first.
            if !full {
                let mut replayed = Default::default();
                ops::replay_queued(engine, session, account, Uploads::Settled, &mut replayed)
                    .await?;
            }
            // Last, so that it is the selected mailbox to IDLE on. Only the flags the
            // server said changed are fetched: without CONDSTORE the rest would be every
            // message's, too much for each new one.
            match sync::sync_inbox(engine, session, account, changed_flags).await {
                Ok(true) if session.can_idle() => Ok(Wait::Idle),
                Ok(_) => Ok(Wait::Poll),
                Err(e) if e.is_refusal() => {
                    tracing::warn!(%account, error = %e, "can't sync INBOX");
                    Ok(Wait::InboxFailed)
                }
                Err(e) => Err(e),
            }
        })
        .await
}
