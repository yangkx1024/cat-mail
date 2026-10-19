use std::collections::HashMap;
use std::fs::File;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use catmail_proto::TlsConfig;
use catmail_proto::imap::Session;
use catmail_store::{BlobStore, Db, Store};
use catmail_types::{AccountId, Event};
use tokio::sync::{OwnedMutexGuard, broadcast, watch};

use crate::secrets::SecretStore;
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
    syncs: Mutex<HashMap<AccountId, Arc<AccountSync>>>,
    /// Logged-in connections for on-demand fetches, kept between requests so opening
    /// messages one after another doesn't log in each time. Sync uses its own.
    idle_sessions: Mutex<HashMap<AccountId, Vec<IdleSession>>>,
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

/// Serializes syncs of one account and lets account removal stop a running one, and
/// any on-demand fetch. Replaced by the per-account actor once IDLE arrives (M4).
struct AccountSync {
    lock: Arc<tokio::sync::Mutex<()>>,
    cancel: watch::Sender<bool>,
}

/// Held while syncing an account.
pub(crate) struct SyncPermit {
    _guard: OwnedMutexGuard<()>,
    cancellation: Cancellation,
}

impl SyncPermit {
    /// Resolves once the account is being removed.
    pub async fn cancelled(&mut self) {
        self.cancellation.cancelled().await;
    }
}

/// Tells work on an account that the account is being removed.
pub(crate) struct Cancellation(watch::Receiver<bool>);

impl Cancellation {
    /// Resolves once the account is being removed (at once if it already is).
    pub async fn cancelled(&mut self) {
        // Err means the sender is gone, which only happens once nobody can cancel.
        if self.0.wait_for(|c| *c).await.is_err() {
            std::future::pending::<()>().await;
        }
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
            syncs: Mutex::default(),
            idle_sessions: Mutex::default(),
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

    pub fn tls(&self) -> Result<TlsConfig> {
        if let Some(tls) = self.tls.get() {
            return Ok(tls.clone());
        }
        let tls = TlsConfig::platform().map_err(|e| Error::Tls(e.to_string()))?;
        Ok(self.tls.get_or_init(|| tls).clone())
    }

    /// Waits for any other sync of `account` to finish.
    pub async fn sync_permit(&self, account: AccountId) -> SyncPermit {
        let sync = self.account_sync(account);
        let cancellation = Cancellation(sync.cancel.subscribe());
        let guard = sync.lock.clone().lock_owned().await;
        SyncPermit {
            _guard: guard,
            cancellation,
        }
    }

    /// For work on `account` other than sync. Obtain it before checking that the
    /// account (or the row worked on) still exists: a removal that starts afterwards
    /// is then always seen.
    pub fn cancellation(&self, account: AccountId) -> Cancellation {
        Cancellation(self.account_sync(account).cancel.subscribe())
    }

    /// A logged-in connection that was recently in use, if there is one.
    pub fn take_session(&self, account: AccountId) -> Option<Session> {
        let mut idle = self
            .idle_sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let sessions = idle.get_mut(&account)?;
        sessions.retain(|s| s.since.elapsed() < IDLE_SESSION_TTL);
        sessions.pop().map(|s| s.session)
    }

    /// Keeps a connection that is still usable for the next request, unless the account
    /// is being removed.
    pub fn return_session(&self, account: AccountId, session: Session) {
        let mut idle = self
            .idle_sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Checked under the pool lock: removal cancels before it empties the pool, so a
        // connection returned after that sees the cancellation and is dropped.
        let removing = self
            .syncs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&account)
            .is_none_or(|sync| *sync.cancel.borrow());
        if removing {
            return;
        }
        let sessions = idle.entry(account).or_default();
        if sessions.len() < IDLE_SESSIONS_PER_ACCOUNT {
            sessions.push(IdleSession {
                session,
                since: Instant::now(),
            });
        }
    }

    /// Closes the account's idle connections.
    pub fn forget_sessions(&self, account: AccountId) {
        self.idle_sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&account);
    }

    /// Cancels a running sync of `account` and waits until it has stopped. Syncs that
    /// start afterwards find the account gone.
    pub async fn stop_syncing(&self, account: AccountId) -> OwnedMutexGuard<()> {
        let sync = self.account_sync(account);
        sync.cancel.send_replace(true);
        sync.lock.clone().lock_owned().await
    }

    pub fn forget_sync(&self, account: AccountId) {
        self.syncs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&account);
    }

    fn account_sync(&self, account: AccountId) -> Arc<AccountSync> {
        self.syncs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(account)
            .or_insert_with(|| {
                Arc::new(AccountSync {
                    lock: Arc::default(),
                    cancel: watch::Sender::new(false),
                })
            })
            .clone()
    }
}

/// Runs blocking work (SQLite, keyring) on the current runtime's blocking pool.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(crate::runtime::join_error)?
}
