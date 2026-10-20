//! Keeping accounts up to date in the background
//! ([`MailClientBuilder::sync_in_background`](crate::MailClientBuilder::sync_in_background)).
//!
//! One task per account, with one connection, started when the engine opens or the
//! account is added. It syncs every folder now and then, and in
//! between watches INBOX: with IDLE the server says when something changes, without it
//! INBOX is checked at an interval. Queued local changes, and changes to drafts, wake
//! it to make them on the server at once. Its work takes the account's sync permit
//! like any sync, but its waiting doesn't: a sync or replay the user asks for meanwhile
//! runs on a connection of its own.
//!
//! The tasks hold the engine weakly, so dropping the last client ends them, and with
//! them their connections.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use catmail_proto::imap::Session;
use catmail_types::AccountId;
use tokio::sync::Notify;

use crate::engine::{Cancellation, Engine, backoff};
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
    /// in a row, up to `full_sync` [30 seconds]. Queued local changes try again at once.
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
        tokio::spawn(run(Arc::downgrade(engine), account, schedule));
    }
}

/// An IDLE that loses its connection sooner than this after it began counts towards
/// backing off.
const SHORT_IDLE: Duration = Duration::from_secs(60);

/// What the connection does between steps.
enum Wait {
    /// IDLE on INBOX, which the step left selected.
    Idle,
    /// Nothing; check again later.
    Poll,
}

/// Keeps one account in sync until it is removed or the engine is gone.
pub(crate) async fn run(engine: Weak<Engine>, account: AccountId, schedule: SyncSchedule) {
    let Some((wake, mut cancellation)) = engine
        .upgrade()
        .map(|e| (e.wake_signal(account), e.cancellation(account)))
    else {
        return;
    };
    let mut session: Option<Session> = None;
    let mut failures = 0;
    // IDLEs in a row that the connection was lost in soon after they began.
    let mut idle_losses = 0;
    // Cleared if the server refuses IDLE even though it offers it.
    let mut idle_works = true;
    loop {
        let Some(strong) = engine.upgrade() else {
            return;
        };
        if cancellation.is_cancelled() {
            return;
        }
        let result = step(&strong, &mut session, account, schedule).await;
        let full_sync_due = full_sync_in(&strong, account, &schedule);
        drop(strong);

        let pause = match result {
            Ok(wait) => {
                failures = 0;
                match (wait, session.as_mut()) {
                    (Wait::Idle, Some(s)) if idle_works => {
                        let start = Instant::now();
                        match idle(s, &wake, &mut cancellation, &schedule, full_sync_due).await {
                            Ok(()) => {
                                idle_losses = 0;
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
                    _ => schedule.poll.min(full_sync_due),
                }
            }
            Err(Error::AccountNotFound(_) | Error::Cancelled) => return,
            Err(e) => {
                session = None;
                failures += 1;
                let delay = backoff(schedule.retry, failures, schedule.full_sync);
                tracing::warn!(%account, error = %e, ?delay,
                    "background sync failed; trying again later");
                delay
            }
        };
        tokio::select! {
            () = stopped(&wake, &mut cancellation) => {}
            () = tokio::time::sleep(pause) => {}
        }
    }
}

/// IDLEs until the server reports a change, local changes are queued, the account is
/// removed or the engine is gone, or every folder is due to be synced (in
/// `full_sync_due`). An IDLE that ends quietly is started over: its round trip showed
/// the connection alive.
async fn idle(
    session: &mut Session,
    wake: &Notify,
    cancellation: &mut Cancellation,
    schedule: &SyncSchedule,
    full_sync_due: Duration,
) -> catmail_proto::imap::Result<()> {
    let start = Instant::now();
    loop {
        let left = full_sync_due.saturating_sub(start.elapsed());
        let mut stop = false;
        let stopping = async {
            stopped(wake, cancellation).await;
            stop = true;
        };
        let changed = session.idle(stopping, schedule.idle.min(left)).await?;
        if changed || stop || left <= schedule.idle {
            return Ok(());
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

/// How long until every folder is due to be synced again; zero if it is.
fn full_sync_in(engine: &Engine, account: AccountId, schedule: &SyncSchedule) -> Duration {
    engine.synced(account).map_or(Duration::ZERO, |t| {
        schedule.full_sync.saturating_sub(t.elapsed())
    })
}

/// Syncs every folder if that is due, else makes queued local changes on the server
/// (changed drafts included); then syncs INBOX. On the connection there, or a new one (see
/// [`on_session`](connection::on_session)).
async fn step(
    engine: &Arc<Engine>,
    session: &mut Option<Session>,
    account: AccountId,
    schedule: SyncSchedule,
) -> Result<Wait> {
    // Connected before taking the permit: waiting for an unreachable server shouldn't
    // hold up syncs the user asks for.
    connection::on_session(engine, account, session, |s| {
        let engine = engine.clone();
        Box::pin(async move { work(&engine, s, account, &schedule).await })
    })
    .await
}

async fn work(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    schedule: &SyncSchedule,
) -> Result<Wait> {
    let permit = engine.sync_permit(account).await;
    permit
        .run(async {
            if full_sync_in(engine, account, schedule).is_zero() {
                sync::run(engine, session, account).await?;
            } else {
                ops::replay_queued(engine, session, account).await?;
            }
            // Last, so that it is the selected mailbox to IDLE on.
            match sync::sync_inbox(engine, session, account).await {
                Ok(true) if session.can_idle() => Ok(Wait::Idle),
                Ok(_) => Ok(Wait::Poll),
                Err(Error::Imap(e)) if !e.is_fatal() => {
                    tracing::warn!(%account, error = %e, "can't sync INBOX");
                    Ok(Wait::Poll)
                }
                Err(e) => Err(e),
            }
        })
        .await
}
