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
//! runs on a connection of its own. A sync of every account asked for
//! ([`MailClient::sync_now`](crate::MailClient::sync_now)) is made by these tasks.
//!
//! After the computer slept ([`Engine::resume`]) a task drops a connection made before,
//! which is likely dead, without a word, and syncs every folder unless a sync of them all
//! began since. Failing then, it tries again sooner than after other failures, as the
//! network may not be back yet ([`SyncSchedule::retry_after_wake`]).
//!
//! The tasks hold the engine weakly, so dropping the last client ends them, and with
//! them their connections.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use catmail_proto::imap::{ChangedFlags, Session};
use catmail_types::AccountId;
use tokio::sync::Notify;

use crate::drafts::draft_sync::Uploads;
use crate::engine::Engine;
use crate::engine::pool::{self, Pinged};
use crate::engine::settings::SyncSchedule;
use crate::engine::status::Activity;
use crate::task::{Cancellation, backoff};
use crate::{Error, Result, engine::connection, sync, sync::ops};

/// Starts keeping `account` in sync, if the engine syncs in the background. Call from
/// inside the engine runtime.
pub(crate) fn spawn(engine: &Arc<Engine>, account: AccountId) {
    if let Some(schedule) = engine.settings().sync_in_background {
        tokio::spawn(task(engine, account, schedule));
    }
}

/// The task that keeps `account` in sync, the account shown as syncing already for its
/// first sync of every folder.
pub(crate) fn task(
    engine: &Arc<Engine>,
    account: AccountId,
    schedule: SyncSchedule,
) -> impl Future<Output = ()> + Send + 'static {
    engine.begin_syncing(account);
    run(Arc::downgrade(engine), account, schedule)
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
    /// Check INBOX again later; the step left it selected.
    Poll,
    /// The account has no INBOX to watch; check again later.
    NoInbox,
    /// As `Poll`, but INBOX couldn't be synced: flag changes reported earlier are still
    /// to be fetched.
    InboxFailed,
}

impl Wait {
    /// Whether INBOX was synced and left selected, so keepalive NOOPs while polling can
    /// check it for changes.
    fn watches_inbox(&self) -> bool {
        matches!(self, Self::Idle | Self::Poll)
    }
}

enum KeepAlive {
    /// The connection answered and stays open; `changed` is whether the server reported a
    /// change to INBOX (only asked when polling it).
    Alive { changed: bool },
    /// The connection was closed (died, from before a sleep, or cancelled).
    Closed,
    /// The server said the account went over its download limit.
    OverLimit(catmail_proto::imap::Error),
}

/// Keeps one account in sync until it is removed or the engine is gone.
async fn run(engine: Weak<Engine>, account: AccountId, schedule: SyncSchedule) {
    // An idle connection if there is one: adding an account leaves the one that checked
    // the password.
    let Some((wake, resume, mut cancellation, mut session)) = engine.upgrade().map(|e| {
        let session = e.pool(account).take();
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
    // IDLEs in a row that ended with a change soon after they began, or keepalive NOOPs
    // in a row that woke polling early.
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
        let began = Instant::now();
        let result = step(&strong, &mut session, account, schedule, &changed_flags).await;
        if matches!(result, Ok(Wait::Idle | Wait::Poll | Wait::NoInbox)) {
            changed_flags = ChangedFlags::default();
        }
        let failures = strong.report(account, Activity::SyncBegun(began), &result);
        // Drafts held back while they are being written are uploaded once they settle:
        // the step runs again by then, at once for one that settled while it ran.
        let full = full_sync_in(&strong, account, &schedule);
        let due = strong
            .next_draft_due(account)
            .map_or(full, |drafts| full.min(drafts));
        drop(strong);

        // `poll_inbox`: check INBOX for changes in keepalive NOOPs only between normal
        // polls of INBOX, never during backoff, after INBOX failed, or without an INBOX.
        let (pause, poll_inbox) = match result {
            Ok(_) if due.is_zero() => continue,
            Ok(wait) => match (wait, session.as_mut()) {
                (Wait::Idle, Some(s)) if idle_works => {
                    let (start, opened) = (Instant::now(), s.opened());
                    let idled = tokio::select! {
                        idled = idle(s, &engine, account, &wake, &mut cancellation, &schedule, due) => Some(idled),
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
                            (delay, false)
                        }
                        Ok(Idled::CutShort) => {
                            tracing::info!(%account,
                                "server ends IDLE at once; checking INBOX at intervals");
                            idle_works = false;
                            continue;
                        }
                        // Neither a refused IDLE nor a lost connection: nothing works on
                        // the account until it lifts, as for a step that fails so.
                        Err(e) if e.is_over_limit() => {
                            session = None;
                            (went_over_limit(&engine, account, e, &schedule), false)
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
                                tracing::info!(%account, error = %e, "IDLE connection lost");
                                continue;
                            }
                            let delay =
                                backoff(schedule.retry, idle_losses - 1, schedule.full_sync);
                            tracing::warn!(%account, error = %e, ?delay,
                                "IDLE connection keeps getting lost; trying again later");
                            (delay, false)
                        }
                    }
                }
                (wait, _) => (schedule.poll.min(due), wait.watches_inbox()),
            },
            Err(Error::AccountNotFound(_) | Error::Cancelled) => return,
            Err(e) => {
                session = None;
                let delay = match e {
                    // Each try is another rejected login, which servers count towards
                    // locking the account.
                    Error::AuthFailed(_) => schedule.full_sync,
                    Error::OverLimit(_) => schedule.over_limit,
                    _ => failures.retry_in(
                        schedule.retry_after_wake,
                        schedule.retry,
                        schedule.full_sync,
                    ),
                };
                tracing::warn!(%account, error = %e, ?delay,
                    "background sync failed; trying again later");
                (delay, false)
            }
        };
        let mut until = tokio::time::Instant::now() + pause;
        loop {
            let next = tokio::time::Instant::now() + schedule.keepalive;
            let noop = session.is_some() && next < until;
            tokio::select! {
                () = cancellation.cancelled() => break,
                () = unless(&engine, &wake, |e| e.ignores_wakes(account, false)) => break,
                () = unless(&engine, &resume, |e| e.login_rejected(account)) => break,
                () = tokio::time::sleep_until(if noop { next } else { until }) => {}
            }
            if !noop {
                break;
            }
            match keep_alive(
                &engine,
                account,
                &mut session,
                poll_inbox,
                &mut cancellation,
            )
            .await
            {
                KeepAlive::Alive { changed: true } => {
                    if let Some(s) = &session {
                        add_changed_flags(&mut changed_flags, s.changed_flags());
                    }
                    busy = busy.saturating_add(1);
                    if busy <= BUSY {
                        break;
                    }
                    if busy == BUSY + 1 {
                        tracing::debug!(%account, "INBOX keeps changing; slowing down");
                    }
                }
                KeepAlive::Alive { changed: false } => {
                    if poll_inbox {
                        busy = 0;
                    }
                }
                KeepAlive::Closed => {}
                KeepAlive::OverLimit(e) => {
                    let delay = went_over_limit(&engine, account, e, &schedule);
                    until = tokio::time::Instant::now() + delay;
                }
            }
        }
    }
}

/// Reports that the server said the account went over its download limit outside a
/// step (in IDLE, or answering a keepalive NOOP), as for a step that fails so; returns
/// how long to wait for it to lift.
fn went_over_limit(
    engine: &Weak<Engine>,
    account: AccountId,
    e: catmail_proto::imap::Error,
    schedule: &SyncSchedule,
) -> Duration {
    tracing::warn!(%account, error = %e, delay = ?schedule.over_limit,
        "background sync failed; trying again later");
    if let Some(engine) = engine.upgrade() {
        let failed = Err::<(), _>(Error::from(e));
        engine.report(account, Activity::Sync, &failed);
    }
    schedule.over_limit
}

/// Sends the connection background sync waits on a NOOP, so that the server doesn't
/// close it as idle before the next step: NetEase does after a minute, and offers no
/// IDLE, so that each check of INBOX would log in again. With `poll_inbox`, also checks
/// the answer for changes to INBOX ([`pool::poll`]). Closes one that died, or one from
/// before a sleep (see [`Engine::usable`]); reports if the server said the account went
/// over its download limit. Queued changes and waking from sleep don't cut the NOOP
/// short, which would leave its answer half read: their signals keep a permit, so the
/// wait after it ends at once.
async fn keep_alive(
    engine: &Weak<Engine>,
    account: AccountId,
    slot: &mut Option<Session>,
    poll_inbox: bool,
    cancellation: &mut Cancellation,
) -> KeepAlive {
    let Some(session) = slot.as_mut() else {
        return KeepAlive::Closed;
    };
    // The task ends at its next look if the engine is gone.
    let Some(usable) = engine.upgrade().map(|e| e.usable(session)) else {
        return KeepAlive::Closed;
    };
    if !usable {
        tracing::debug!(%account, "waiting connection from before a sleep; closed");
        *slot = None;
        return KeepAlive::Closed;
    }
    let stop = cancellation.cancelled();
    let pinged = if poll_inbox {
        pool::poll(account, session, "background sync", stop).await
    } else {
        pool::ping(account, session, "background sync", stop).await
    };
    match pinged {
        Some(Pinged::Alive { changed }) => KeepAlive::Alive { changed },
        Some(Pinged::OverLimit(e)) => {
            *slot = None;
            KeepAlive::OverLimit(e)
        }
        Some(Pinged::Died) | None => {
            *slot = None;
            KeepAlive::Closed
        }
    }
}

/// Returns once `signal` is notified while `ignored` doesn't hold, or the engine is gone:
/// neither queued changes nor waking from sleep bring on another rejected login, nor do
/// queued changes add to an account over its limit (see [`Engine::wake`]), not even when
/// notified before that was known.
async fn unless(engine: &Weak<Engine>, signal: &Notify, ignored: impl Fn(&Engine) -> bool) {
    loop {
        signal.notified().await;
        if engine.upgrade().is_none_or(|e| !ignored(&e)) {
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

/// IDLEs until the server reports a change, local changes are queued (unless background
/// sync ignores them for now, see [`Engine::ignores_wakes`]), the account is removed or
/// the engine is gone, or the next step is due (in `due`: every folder to be synced, or a
/// draft to be uploaded). An IDLE that ends quietly is started over: its round trip
/// showed the connection alive.
async fn idle(
    session: &mut Session,
    engine: &Weak<Engine>,
    account: AccountId,
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
            stopped(engine, account, wake, cancellation).await;
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

/// Resolves once local changes are queued that background sync doesn't ignore, the
/// account is removed or the engine is gone.
async fn stopped(
    engine: &Weak<Engine>,
    account: AccountId,
    wake: &Notify,
    cancellation: &mut Cancellation,
) {
    tokio::select! {
        () = unless(engine, wake, |e| e.ignores_wakes(account, true)) => {}
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
        sync::run(engine, session, account, Uploads::Settled).await?;
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
            // Last, so that it is the selected mailbox to IDLE on (or poll with NOOP).
            // Only the flags the server said changed are fetched: without CONDSTORE the
            // rest would be every message's, too much for each new one.
            match sync::sync_inbox(engine, session, account, changed_flags).await {
                Ok(true) if session.can_idle() => Ok(Wait::Idle),
                Ok(true) => Ok(Wait::Poll),
                Ok(false) => Ok(Wait::NoInbox),
                Err(e) if e.is_refusal() => {
                    tracing::warn!(%account, error = %e, "can't sync INBOX");
                    Ok(Wait::InboxFailed)
                }
                Err(e) => Err(e),
            }
        })
        .await
}
