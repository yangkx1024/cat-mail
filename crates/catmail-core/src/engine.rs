use std::collections::HashMap;
use std::fs::File;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use catmail_proto::TlsConfig;
use catmail_proto::imap::Session;
use catmail_store::{BlobStore, Db, Store};
use catmail_types::{AccountId, Event};
use tokio::sync::{Notify, OwnedMutexGuard, broadcast, watch};

use crate::secrets::SecretStore;
use crate::watch::SyncSchedule;
use crate::{Config, Error, Result};

/// State shared by every `MailClient` clone and every engine task.
pub(crate) struct Engine {
    pub config: Config,
    /// Exclusive lock on the data directory, held for the engine's lifetime: one engine
    /// per data directory, so sync workers, the outbox and events are never split
    /// across processes. Other frontends will reach the running engine over IPC.
    _data_dir_lock: File,
    pub store: Store,
    pub blobs: BlobStore,
    pub secrets: Arc<dyn SecretStore>,
    pub events: broadcast::Sender<Event>,
    /// Set by the builder, else the platform verifier, created on first use.
    tls: OnceLock<TlsConfig>,
    /// Wakes the outbox worker, if one runs: something became due.
    pub outbox_kick: Arc<Notify>,
    /// Set if every account is kept in sync in the background (see `watch`).
    pub sync_in_background: Option<SyncSchedule>,
    accounts: Mutex<HashMap<AccountId, Arc<AccountState>>>,
}

struct IdleSession {
    session: Session,
    since: Instant,
}

/// Idle connections older than this are dropped rather than reused: servers and NAT
/// boxes close quiet connections, and a dead one costs a timeout to discover.
const IDLE_SESSION_TTL: Duration = Duration::from_secs(5 * 60);

/// Enough for a message and one of its attachments at once.
const IDLE_SESSIONS_PER_ACCOUNT: usize = 2;

/// What the engine keeps per account.
///
/// A removed account keeps its entry, marked `removed`, so work that looks at it later
/// is cancelled at once rather than racing the removal. Account ids are never reused.
struct AccountState {
    /// Serializes syncs.
    sync: Arc<tokio::sync::Mutex<()>>,
    /// Serializes work on the outbox, so an entry is never sent or saved twice at once.
    send: Arc<tokio::sync::Mutex<()>>,
    /// Set when removal starts; cancels running syncs and on-demand fetches.
    removed: watch::Sender<bool>,
    /// Logged-in connections for on-demand fetches, kept between requests so opening
    /// messages one after another doesn't log in each time. Sync uses its own.
    idle: Mutex<Vec<IdleSession>>,
    /// Wakes the account's background sync, if one runs: local changes are waiting for
    /// the server.
    wake: Arc<Notify>,
    /// When the last sync of every folder finished.
    synced: Mutex<Option<Instant>>,
}

/// Held while syncing an account, or while working on its outbox.
pub(crate) struct Permit {
    _guard: OwnedMutexGuard<()>,
    account: AccountId,
    cancellation: Cancellation,
}

impl Permit {
    /// Runs `work` holding the permit. Fails with [`Error::AccountNotFound`] if the
    /// account was removed before, and stops it with [`Error::Cancelled`] if it is
    /// removed meanwhile.
    pub async fn run<T>(mut self, work: impl Future<Output = Result<T>>) -> Result<T> {
        if self.cancellation.is_cancelled() {
            return Err(Error::AccountNotFound(self.account.to_string()));
        }
        tokio::select! {
            result = work => result,
            () = self.cancellation.cancelled() => Err(Error::Cancelled),
        }
    }
}

/// Tells work on an account that the account is being removed.
pub(crate) struct Cancellation(watch::Receiver<bool>);

impl Cancellation {
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once the account is being removed (at once if it already is), or the
    /// engine is gone: work holding the engine only weakly stops then too.
    pub async fn cancelled(&mut self) {
        // Err: the sender, owned by the engine, is gone.
        let _ = self.0.wait_for(|c| *c).await;
    }
}

impl Engine {
    pub fn new(
        config: Config,
        data_dir_lock: File,
        store: Store,
        blobs: BlobStore,
        secrets: Arc<dyn SecretStore>,
        events: broadcast::Sender<Event>,
        tls: Option<TlsConfig>,
    ) -> Self {
        Self {
            config,
            _data_dir_lock: data_dir_lock,
            store,
            blobs,
            secrets,
            events,
            tls: tls.map(OnceLock::from).unwrap_or_default(),
            outbox_kick: Arc::default(),
            sync_in_background: None,
            accounts: Mutex::default(),
        }
    }

    /// Called from inside the task performing the mutation, so dropping the caller's
    /// future can't suppress the notification for a change that did happen.
    pub fn emit(&self, event: Event) {
        // No subscribers is fine.
        let _ = self.events.send(event);
    }

    /// Runs a read on the blocking pool of the current (engine) runtime.
    pub async fn read<T: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&Db<'_>) -> catmail_store::Result<T> + Send + 'static,
    ) -> Result<T> {
        let engine = self.clone();
        blocking(move || Ok(engine.store.read(f)?)).await
    }

    /// Runs a write transaction on the blocking pool. Emit events after this returns.
    pub async fn write<T: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&Db<'_>) -> catmail_store::Result<T> + Send + 'static,
    ) -> Result<T> {
        let engine = self.clone();
        blocking(move || Ok(engine.store.write(f)?)).await
    }

    /// [`write`](Self::write), on disk even across a power cut; see
    /// [`Store::write_durable`](catmail_store::Store::write_durable).
    pub async fn write_durable<T: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&Db<'_>) -> catmail_store::Result<T> + Send + 'static,
    ) -> Result<T> {
        let engine = self.clone();
        blocking(move || Ok(engine.store.write_durable(f)?)).await
    }

    pub fn tls(&self) -> Result<TlsConfig> {
        if let Some(tls) = self.tls.get() {
            return Ok(tls.clone());
        }
        let tls = TlsConfig::platform().map_err(|e| Error::Tls(e.to_string()))?;
        Ok(self.tls.get_or_init(|| tls).clone())
    }

    /// Waits for any other sync of `account` to finish.
    pub async fn sync_permit(&self, account: AccountId) -> Permit {
        self.permit(account, |state| &state.sync).await
    }

    /// Waits for any other work on `account`'s outbox to finish.
    pub async fn send_permit(&self, account: AccountId) -> Permit {
        self.permit(account, |state| &state.send).await
    }

    async fn permit(
        &self,
        account: AccountId,
        lock: impl FnOnce(&AccountState) -> &Arc<tokio::sync::Mutex<()>>,
    ) -> Permit {
        let state = self.account(account);
        let cancellation = Cancellation(state.removed.subscribe());
        let guard = lock(&state).clone().lock_owned().await;
        Permit {
            _guard: guard,
            account,
            cancellation,
        }
    }

    /// Wakes the account's background sync, if one runs, to make queued local changes
    /// on the server. A wake while it is busy is kept for when it next waits.
    pub fn wake(&self, account: AccountId) {
        self.account(account).wake.notify_one();
    }

    /// What [`wake`](Self::wake) notifies.
    pub fn wake_signal(&self, account: AccountId) -> Arc<Notify> {
        self.account(account).wake.clone()
    }

    /// Records that every folder of the account was just synced.
    pub fn set_synced(&self, account: AccountId) {
        *lock(&self.account(account).synced) = Some(Instant::now());
    }

    /// When every folder of the account was last synced, if ever since the engine
    /// started.
    pub fn synced(&self, account: AccountId) -> Option<Instant> {
        *lock(&self.account(account).synced)
    }

    /// For work on `account` other than sync. Obtain it before checking that the
    /// account (or the row worked on) still exists: a removal is then seen whether it
    /// started before or after.
    pub fn cancellation(&self, account: AccountId) -> Cancellation {
        Cancellation(self.account(account).removed.subscribe())
    }

    /// A logged-in connection that was recently in use, if there is one.
    pub fn take_session(&self, account: AccountId) -> Option<Session> {
        let state = self.account(account);
        let mut idle = lock(&state.idle);
        idle.retain(|s| s.since.elapsed() < IDLE_SESSION_TTL);
        idle.pop().map(|s| s.session)
    }

    /// Keeps a connection that is still usable for the next request, unless the account
    /// is being removed.
    pub fn return_session(&self, account: AccountId, session: Session) {
        let state = self.account(account);
        let mut idle = lock(&state.idle);
        // Checked under the pool lock, which removal takes after setting the flag, so a
        // connection can't slip back in after the pool was emptied.
        if *state.removed.borrow() || idle.len() >= IDLE_SESSIONS_PER_ACCOUNT {
            return;
        }
        idle.push(IdleSession {
            session,
            since: Instant::now(),
        });
    }

    /// Marks `account` removed: running syncs and fetches are cancelled, idle connections
    /// closed. Waits until a running sync has stopped.
    pub async fn start_removal(&self, account: AccountId) -> OwnedMutexGuard<()> {
        let state = self.account(account);
        state.removed.send_replace(true);
        lock(&state.idle).clear();
        state.sync.clone().lock_owned().await
    }

    /// After [`start_removal`](Self::start_removal) found no such account: forget the
    /// mark, or the account that gets this id later would start out removed.
    pub fn cancel_removal(&self, account: AccountId) {
        lock(&self.accounts).remove(&account);
    }

    fn account(&self, account: AccountId) -> Arc<AccountState> {
        lock(&self.accounts)
            .entry(account)
            .or_insert_with(|| {
                Arc::new(AccountState {
                    sync: Arc::default(),
                    send: Arc::default(),
                    removed: watch::Sender::new(false),
                    idle: Mutex::default(),
                    wake: Arc::default(),
                    synced: Mutex::default(),
                })
            })
            .clone()
    }
}

/// A panic while holding one of these locks can't leave the data inconsistent.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs blocking work (SQLite, keyring) on the current runtime's blocking pool.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(crate::runtime::join_error)?
}

/// `first`, doubled for each failure in a row after the first, up to `max`.
pub(crate) fn backoff(first: Duration, failures: u32, max: Duration) -> Duration {
    first
        .saturating_mul(1 << failures.saturating_sub(1).min(16))
        .min(max)
}

/// Now, in unix seconds.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
