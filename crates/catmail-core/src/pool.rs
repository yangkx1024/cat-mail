//! Logged-in IMAP connections kept between on-demand requests (bodies, attachments,
//! saving to Sent, replays, the sync of an opened folder), so that opening messages one
//! after another doesn't log in each time; and the rules for using a connection again,
//! pooled or not ([`with_session`], [`on_session`]). Sync of every folder and background
//! sync keep connections of their own, which follow the same rules.
//!
//! While a connection waits in the pool it is kept alive: every
//! [`EngineTuning::keepalive`](crate::EngineTuning) it is sent a NOOP, so that servers
//! and NAT boxes don't close it as idle, and one that died is closed at once rather than
//! found dead by the next request. There is no limit on how long: while the engine runs,
//! it is there to open mail.

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use catmail_proto::imap::{self, Session};
use catmail_types::AccountId;
use tokio::task::AbortHandle;

use crate::connection::connect;
use crate::engine::{Closing, Engine, lock};
use crate::{Error, Result};

/// Enough for a message and one of its attachments at once.
const PER_ACCOUNT: usize = 2;

/// An account's idle connections.
pub(crate) struct Pool {
    account: AccountId,
    idle: Mutex<Idle>,
}

#[derive(Default)]
struct Idle {
    /// Ordered by `used`: the one used last at the end.
    sessions: Vec<Pooled>,
    /// The account is being removed: nothing is kept any more.
    closed: bool,
}

struct Pooled {
    session: Session,
    /// When a request last returned it; a keepalive leaves this alone. The pool hands out
    /// the one used last, which likely has the folder wanted open still.
    used: Instant,
    /// The task keeping it alive (see [`keep_alive`]), which also tells it from the others.
    keepalive: AbortHandle,
}

impl Pool {
    pub fn new(account: AccountId) -> Self {
        Self {
            account,
            idle: Mutex::default(),
        }
    }

    /// The connection used last, if there is one.
    pub fn take(&self) -> Option<Session> {
        let account = self.account;
        let mut idle = lock(&self.idle);
        let Some(pooled) = idle.sessions.pop() else {
            tracing::debug!(%account, "no idle connection");
            return None;
        };
        // Still waiting: one that sends its NOOP took the connection out first.
        pooled.keepalive.abort();
        tracing::debug!(%account, idle = ?pooled.used.elapsed(),
            age = ?pooled.session.opened().elapsed(), left = idle.sessions.len(),
            "reusing an idle connection");
        Some(pooled.session)
    }

    /// Keeps a connection a request is done with for the next one, unless the account is
    /// being removed or enough are kept already. Call from inside the engine runtime.
    pub fn put(&self, engine: &Arc<Engine>, session: Session) {
        self.keep(engine, session, Instant::now());
    }

    /// [`put`](Self::put), the connection last used at `used`.
    fn keep(&self, engine: &Arc<Engine>, session: Session, used: Instant) {
        let account = self.account;
        let mut idle = lock(&self.idle);
        if idle.closed {
            tracing::debug!(%account, "account being removed; connection closed");
            return;
        }
        if idle.sessions.len() >= PER_ACCOUNT {
            tracing::debug!(%account, "enough idle connections; connection closed");
            return;
        }
        // Spawned under the lock: the task finds the connection here when it first looks.
        let keepalive = tokio::spawn(keep_alive(
            Arc::downgrade(engine),
            account,
            engine.tuning.keepalive,
            engine.closing(),
        ))
        .abort_handle();
        let i = idle.sessions.partition_point(|p| p.used <= used);
        idle.sessions.insert(
            i,
            Pooled {
                session,
                used,
                keepalive,
            },
        );
        tracing::debug!(%account, kept = idle.sessions.len(), "kept an idle connection");
    }

    /// The connection the current task keeps alive, if it is still here, and when it was
    /// last used.
    fn take_own(&self) -> Option<(Session, Instant)> {
        let task = tokio::task::id();
        let mut idle = lock(&self.idle);
        let i = idle
            .sessions
            .iter()
            .position(|p| p.keepalive.id() == task)?;
        let pooled = idle.sessions.remove(i);
        Some((pooled.session, pooled.used))
    }

    /// Closes the connections and refuses those returned later: the account is being
    /// removed. Under the same lock as returning one, so none can slip back in after.
    pub fn close(&self) {
        let mut idle = lock(&self.idle);
        idle.closed = true;
        let closed = idle.sessions.len();
        for pooled in idle.sessions.drain(..) {
            pooled.keepalive.abort();
        }
        if closed > 0 {
            tracing::info!(account = %self.account, closed,
                "account being removed; idle connections closed");
        }
    }
}

/// Whether a connection is worth using: not one made before the computer last slept
/// (see [`Engine::since_woke`]), which is likely dead and only found so after a timeout.
fn usable(engine: &Engine, session: &Session) -> bool {
    engine.since_woke(session.opened())
}

/// Whether a connection a command failed on with `e` can be used again: a refusal leaves
/// it in a known state, as usable as it was (what [`Error::is_refusal`] tells of IMAP).
fn survives(e: &imap::Error) -> bool {
    !e.is_fatal()
}

/// Waits `every`, then sends the pooled connection this task keeps alive a NOOP and
/// returns it (with a task of its own), unless that found it dead: then it is closed.
/// Aborted if the connection is taken meanwhile; ends if the engine is gone. One from
/// before a sleep is closed without a word.
async fn keep_alive(
    engine: Weak<Engine>,
    account: AccountId,
    every: Duration,
    mut closing: Closing,
) {
    tokio::select! {
        () = tokio::time::sleep(every) => {}
        () = closing.closed() => return,
    }
    let Some((mut session, used)) = engine.upgrade().and_then(|engine| {
        let (session, used) = engine.pool(account).take_own()?;
        if usable(&engine, &session) {
            return Some((session, used));
        }
        tracing::info!(%account, "idle connection from before a sleep; closed");
        None
    }) else {
        return;
    };
    // Not holding the engine meanwhile, which would keep it from closing.
    let began = Instant::now();
    let result = tokio::select! {
        result = session.noop() => result,
        () = closing.closed() => return,
    };
    let (took, age) = (began.elapsed(), session.opened().elapsed());
    match result {
        Ok(()) => tracing::debug!(%account, ?took, ?age, "kept an idle connection alive"),
        Err(e) if survives(&e) => {
            tracing::debug!(%account, error = %e, ?took, "keepalive NOOP refused");
        }
        Err(e) => {
            tracing::info!(%account, error = %e, ?took, ?age, "idle connection died; closed");
            return;
        }
    }
    if let Some(engine) = engine.upgrade() {
        engine.pool(account).keep(&engine, session, used);
    }
}

/// Runs `op` on one of the account's idle connections, or a new one, and keeps the
/// connection for next time unless it failed; see [`on_session`].
///
/// Pass an `async move` closure that owns what it captures (a clone of the engine, not a
/// reference to it): with a borrowed capture held across an `.await`, the closure's
/// future can't be proven `Send` (a compiler limitation with higher-ranked lifetimes),
/// so neither can the caller's.
pub(crate) async fn with_session<T>(
    engine: &Arc<Engine>,
    account: AccountId,
    op: impl AsyncFnMut(&mut Session) -> Result<T>,
) -> Result<T> {
    let pool = engine.pool(account);
    let mut slot = pool.take();
    let result = on_session(engine, account, &mut slot, op).await;
    if let Some(session) = slot
        && result
            .as_ref()
            .err()
            .and_then(Error::imap)
            .is_none_or(survives)
    {
        pool.put(engine, session);
    }
    result
}

/// Runs `op` on the connection in `slot`, or on a new one put there: also in place of one
/// that isn't [`usable`]. If the one there turns out to be dead (disconnected, not merely
/// unusable after a refusal), `op` runs once more on a fresh one.
pub(crate) async fn on_session<T>(
    engine: &Arc<Engine>,
    account: AccountId,
    slot: &mut Option<Session>,
    mut op: impl AsyncFnMut(&mut Session) -> Result<T>,
) -> Result<T> {
    if slot.as_ref().is_some_and(|s| !usable(engine, s)) {
        tracing::info!(%account, "connection from before a sleep; reconnecting");
        *slot = None;
    }
    if let Some(session) = slot {
        match op(session).await {
            // A fresh connection, not another idle one: those are likely dead too.
            Err(e) if e.imap().is_some_and(imap::Error::is_disconnected) => {
                tracing::info!(%account, error = %e, "reused connection was dead; reconnecting");
            }
            result => return result,
        }
    }
    *slot = None;
    let session = slot.insert(connect(engine, account).await?);
    op(session).await
}

#[cfg(test)]
mod tests {
    use catmail_proto::{Credentials, TlsConfig};
    use catmail_testkit::{FakeImap, FakeMailbox, ServerState};
    use catmail_types::{Security, ServerConfig};
    use tokio::runtime::Handle;

    use super::*;
    use crate::connection::IO_TIMEOUT;
    use crate::{Config, MailClient, MemorySecrets};

    #[tokio::test(flavor = "multi_thread")]
    async fn a_keepalive_leaves_a_connection_where_it_was() {
        let mut state = ServerState::new("alice", "pw");
        state.mailboxes.push(FakeMailbox::new("Archive"));
        let fake = FakeImap::start(state).await;
        let dir = tempfile::tempdir().unwrap();
        let client = MailClient::builder(Config::with_data_dir(dir.path()))
            .runtime(Handle::current())
            .secrets(Arc::new(MemorySecrets::default()))
            .open()
            .unwrap();
        let engine = client.engine();
        let examining = async |name: &str| {
            let server = ServerConfig {
                host: "127.0.0.1".into(),
                port: fake.port(),
                security: Security::Plain,
            };
            let tls = TlsConfig::with_roots_only([]).unwrap();
            let mut session = Session::connect(&server, &tls, IO_TIMEOUT).await.unwrap();
            let credentials = Credentials {
                username: "alice".into(),
                password: "pw".into(),
            };
            session.login(&credentials).await.unwrap();
            session.examine(name).await.unwrap();
            session
        };

        let pool = Pool::new(AccountId(1));
        let before = Instant::now();
        pool.put(engine, examining("INBOX").await);
        // Used before the other, and returned after it by its keepalive.
        pool.keep(engine, examining("Archive").await, before);
        assert!(pool.take().unwrap().examined("INBOX").is_some());
        assert!(pool.take().unwrap().examined("Archive").is_some());
        assert!(pool.take().is_none());
    }
}
