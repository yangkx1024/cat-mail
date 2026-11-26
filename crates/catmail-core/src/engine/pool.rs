//! An account's idle connections, kept between on-demand requests: held by the engine
//! for each account, taken and returned by the rules in [`connection`](crate::engine::connection). While a connection waits here it is
//! kept alive: every [`EngineTuning::keepalive`](crate::EngineTuning) it is sent a NOOP,
//! so that servers and NAT boxes don't close it as idle, and one that died is closed at
//! once rather than found dead by the next request. There is no limit on how long: while
//! the engine runs, it is there to open mail. Background sync keeps its own connection
//! alive the same way ([`ping`], [`poll`]).

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use catmail_proto::imap::{self, ChangedFlags, Session};
use catmail_types::AccountId;
use tokio::task::AbortHandle;

use crate::task::{Closing, Cutoff, lock};

/// Enough for a message and one of its attachments at once.
const PER_ACCOUNT: usize = 2;

/// An account's idle connections.
pub(crate) struct Pool {
    account: AccountId,
    /// How long a connection waits unused before its NOOP
    /// ([`EngineTuning::keepalive`](crate::EngineTuning)).
    every: Duration,
    /// The engine's: ends the keepalive tasks once it is gone.
    closing: Closing,
    /// When the computer last woke: a connection from before is closed, not kept alive.
    woke: Arc<Cutoff>,
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
    pub fn new(account: AccountId, every: Duration, closing: Closing, woke: Arc<Cutoff>) -> Self {
        Self {
            account,
            every,
            closing,
            woke,
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
    pub fn put(self: &Arc<Self>, session: Session) {
        self.keep(session, Instant::now());
    }

    /// [`put`](Self::put), the connection last used at `used`.
    fn keep(self: &Arc<Self>, session: Session, used: Instant) {
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
            Arc::downgrade(self),
            account,
            self.every,
            self.closing.clone(),
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

/// Whether a connection is worth using: not one made before the computer last slept (as
/// `woke` says), which is likely dead and only found so after a timeout. Engine work asks
/// through `Engine::usable`.
pub(crate) fn usable(woke: &Cutoff, session: &Session) -> bool {
    woke.since(session.opened())
}

/// Whether a connection a command failed on with `e` can be used again: a refusal leaves
/// it in a known state, as usable as it was (what [`Error::is_refusal`] tells of IMAP).
fn survives(e: &imap::Error) -> bool {
    !e.is_fatal()
}

/// Waits `every`, then sends the pooled connection this task keeps alive a NOOP and
/// returns it (with a task of its own), unless that found it dead: then it is closed.
/// Aborted if the connection is taken meanwhile; ends if the engine, and with it the pool,
/// is gone. One from
/// before a sleep is closed without a word.
async fn keep_alive(pool: Weak<Pool>, account: AccountId, every: Duration, mut closing: Closing) {
    tokio::select! {
        () = tokio::time::sleep(every) => {}
        () = closing.closed() => return,
    }
    let Some((mut session, used)) = pool.upgrade().and_then(|pool| {
        let (session, used) = pool.take_own()?;
        if usable(&pool.woke, &session) {
            return Some((session, used));
        }
        tracing::info!(%account, "idle connection from before a sleep; closed");
        None
    }) else {
        return;
    };
    // Not holding the pool meanwhile, which would keep it past the engine.
    if !matches!(
        ping(account, &mut session, "pooled", closing.closed()).await,
        Some(Pinged::Alive { .. })
    ) {
        return;
    }
    if let Some(pool) = pool.upgrade() {
        pool.keep(session, used);
    }
}

pub(crate) enum Pinged {
    /// Answered (with `changed`, the messages whose flags changed, if polled and the
    /// server reported a change to the selected mailbox since it was opened), or refused
    /// (`changed: None`): as usable as it was.
    Alive { changed: Option<ChangedFlags> },
    /// The connection died: closed.
    Died,
    /// The server said the account went over its download limit, refusing the NOOP or
    /// hanging up: nothing works on the account until it lifts, so the connection is
    /// closed either way.
    OverLimit(imap::Error),
}

/// Sends a quiet connection a NOOP, so that servers and NAT boxes don't close it as idle,
/// and tells whether it is still worth keeping. `None` if `stop` resolved first, which
/// leaves the answer half read. `whose` tells the connections apart in the log.
pub(crate) async fn ping(
    account: AccountId,
    session: &mut Session,
    whose: &'static str,
    stop: impl Future<Output = ()>,
) -> Option<Pinged> {
    let opened = session.opened();
    let noop = async { session.noop().await.map(|()| None) };
    answered(account, whose, opened, noop, stop).await
}

/// Like [`ping`], but asks the server for changes to the selected mailbox
/// ([`Session::poll`]) rather than ignoring the answer.
pub(crate) async fn poll(
    account: AccountId,
    session: &mut Session,
    whose: &'static str,
    stop: impl Future<Output = ()>,
) -> Option<Pinged> {
    let opened = session.opened();
    answered(account, whose, opened, session.poll(), stop).await
}

/// What the `noop` of a session `opened` then says about it, unless `stop` resolves
/// first.
async fn answered(
    account: AccountId,
    whose: &'static str,
    opened: Instant,
    noop: impl Future<Output = imap::Result<Option<ChangedFlags>>>,
    stop: impl Future<Output = ()>,
) -> Option<Pinged> {
    let began = Instant::now();
    let result = tokio::select! {
        result = noop => result,
        () = stop => return None,
    };
    let (took, age) = (began.elapsed(), opened.elapsed());
    Some(match result {
        Ok(changed) => {
            tracing::debug!(%account, whose, changed = changed.is_some(), ?took, ?age,
                "kept an idle connection alive");
            Pinged::Alive { changed }
        }
        Err(e) if e.is_over_limit() => {
            tracing::info!(%account, whose, error = %e, "idle connection over the limit; closed");
            Pinged::OverLimit(e)
        }
        Err(e) if survives(&e) => {
            tracing::debug!(%account, whose, error = %e, ?took, "keepalive NOOP refused");
            Pinged::Alive { changed: None }
        }
        Err(e) => {
            tracing::info!(%account, whose, error = %e, ?took, ?age,
                "idle connection died; closed");
            Pinged::Died
        }
    })
}

#[cfg(test)]
mod tests {
    use catmail_proto::{Credentials, TlsConfig};
    use catmail_testkit::{FakeImap, FakeMailbox, ServerState};
    use catmail_types::{Security, ServerConfig};
    use tokio::sync::watch;

    use super::*;
    use crate::EngineTuning;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_keepalive_leaves_a_connection_where_it_was() {
        let mut state = ServerState::new("alice", "pw");
        state.inbox().append_simple("In");
        let mut archive = FakeMailbox::new("Archive");
        archive.append_simple("Archived");
        state.mailboxes.push(archive);
        let fake = FakeImap::start(state).await;
        let examining = async |name: &str| {
            let server = ServerConfig {
                host: "127.0.0.1".into(),
                port: fake.port(),
                security: Security::Plain,
            };
            let tls = TlsConfig::with_roots_only([]).unwrap();
            let mut session = Session::connect(&server, &tls, Duration::from_secs(10))
                .await
                .unwrap();
            let credentials = Credentials {
                username: "alice".into(),
                password: "pw".into(),
            };
            session.login(&credentials).await.unwrap();
            session.examine(name).await.unwrap();
            session
        };

        let (_engine, closing) = watch::channel(());
        let pool = Arc::new(Pool::new(
            AccountId(1),
            EngineTuning::default().keepalive,
            Closing::new(closing),
            Arc::default(),
        ));
        let before = Instant::now();
        pool.put(examining("INBOX").await);
        // Used before the other, and returned after it by its keepalive.
        pool.keep(examining("Archive").await, before);
        assert!(pool.take().unwrap().examined("INBOX", 1).is_some());
        assert!(pool.take().unwrap().examined("Archive", 1).is_some());
        assert!(pool.take().is_none());
    }
}
