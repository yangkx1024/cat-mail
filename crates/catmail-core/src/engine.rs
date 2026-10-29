use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use catmail_proto::TlsConfig;
use catmail_proto::http::ImageFetcher;
use catmail_proto::imap::Session;
use catmail_store::{BlobStore, Db, Store};
use catmail_types::{
    AccountId, AccountStatus, DraftId, Event, FolderId, IdentityId, ReportedError, SendProblem,
    ServerProblem,
};
use tokio::sync::{Notify, OwnedMutexGuard, broadcast, watch};

use crate::image_cache::ImageCache;
use crate::secrets::SecretStore;
use crate::watch::SyncSchedule;
use crate::{Config, DataDirLock, Error, Result};

/// State shared by every `MailClient` clone and every engine task.
pub(crate) struct Engine {
    pub config: Config,
    /// Exclusive lock on the data directory, held for the engine's lifetime: one engine
    /// per data directory, so sync workers, the outbox and events are never split
    /// across processes. Other frontends will reach the running engine over IPC.
    _data_dir_lock: DataDirLock,
    pub store: Store,
    pub blobs: BlobStore,
    pub secrets: Arc<dyn SecretStore>,
    pub events: broadcast::Sender<Event>,
    /// Set by the builder, else the platform verifier, created on first use.
    tls: OnceLock<TlsConfig>,
    /// Created on first use: most sessions never load a remote image.
    image_fetcher: OnceLock<ImageFetcher>,
    pub image_cache: Arc<ImageCache>,
    /// Wakes the outbox worker, if one runs: something became due.
    pub outbox_kick: Arc<Notify>,
    /// Set if every account is kept in sync in the background (see `watch`).
    pub sync_in_background: Option<SyncSchedule>,
    /// How long background sync leaves a draft alone after it changed before uploading
    /// it (see [`held_drafts`](Self::held_drafts)).
    pub draft_upload_delay: Duration,
    pub tuning: EngineTuning,
    /// Never sent to: dropped with the engine, which ends the background tasks holding
    /// it weakly at once (see [`closing`](Self::closing)).
    closing: watch::Sender<()>,
    /// When the computer last woke from sleep, as [`Engine::resume`] was told.
    woke: Cutoff,
    accounts: Mutex<HashMap<AccountId, Arc<AccountState>>>,
}

/// Sizes the engine works in, which tests and users on metered connections may want
/// other than the defaults. Set with
/// [`MailClientBuilder::tuning`](crate::MailClientBuilder::tuning).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineTuning {
    /// New messages fetched per UID FETCH, and stored per transaction [200].
    pub fetch_batch: usize,
    /// Queued changes made on the server per command: UID sets of scattered messages
    /// get long [500].
    pub replay_batch: usize,
    /// Messages up to this size (RFC822.SIZE) are fetched whole when opened; above it,
    /// attachments are left on the server until opened, and text parts above it are not
    /// fetched at all [2 MiB].
    pub whole_message_limit: u32,
}

impl Default for EngineTuning {
    fn default() -> Self {
        Self {
            fetch_batch: 200,
            replay_batch: 500,
            whole_message_limit: 2 * 1024 * 1024,
        }
    }
}

impl EngineTuning {
    /// With the batch sizes at least 1, which chunking needs.
    pub(crate) fn checked(self) -> Self {
        Self {
            fetch_batch: self.fetch_batch.max(1),
            replay_batch: self.replay_batch.max(1),
            ..self
        }
    }
}

/// A moment before which work no longer counts: when the computer last woke from
/// sleep, or a sync of every folder of an account was last asked for.
#[derive(Default)]
struct Cutoff(Mutex<Option<Instant>>);

impl Cutoff {
    fn set(&self, at: Instant) {
        *lock(&self.0) = Some(at);
    }

    fn since(&self, at: Instant) -> bool {
        lock(&self.0).is_none_or(|cutoff| at > cutoff)
    }

    /// When work that `began` then finished, to record for [`since`](Self::since) to
    /// check later, if it counts: not if it began before the cutoff. The time is taken
    /// before the check, so a cutoff set after the check is later and leaves the record
    /// out; taken after it, a cutoff set in between would let the record count.
    fn finished(&self, began: Instant, now: impl FnOnce() -> Instant) -> Option<Instant> {
        let finished = now();
        self.since(began).then_some(finished)
    }
}

struct IdleSession {
    session: Session,
    since: Instant,
}

/// [`Engine::draft_upload_delay`] unless the builder sets it.
pub const DEFAULT_DRAFT_UPLOAD_DELAY: Duration = Duration::from_secs(15);

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
    /// Serializes syncs. A sync of every folder holds it for one folder at a time (see
    /// `sync::run`); the lock is fair, so whatever waits for it meanwhile goes next.
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
    /// Tells the account's background sync, if one runs, to look at once whether its
    /// connection and the last sync of every folder are from before a sleep (see
    /// [`Engine::resume`]).
    resume: Arc<Notify>,
    /// When the last sync of every folder finished. Neither this nor `folders_synced`
    /// counts a sync begun before the computer last woke (see [`Engine::since_woke`]);
    /// this doesn't count one begun before `asked` either.
    synced: Mutex<Option<Instant>>,
    /// When a sync of every folder was last asked for ([`Engine::ask_to_sync`]).
    asked: Cutoff,
    /// When each folder last completed a sync, for a sync of every folder to leave out
    /// those another one completed since it began, and for the sync of an opened folder
    /// to leave out one synced recently. Not `last_synced_at`: the wall clock can be set
    /// back, and a time left in the future would have both skip the folder until the
    /// clock caught up.
    folders_synced: Mutex<HashMap<FolderId, Instant>>,
    status: Mutex<Status>,
    /// When drafts were last changed, for holding back their upload while they are
    /// being written. Entries older than the delay are dropped as they are found.
    draft_edits: Mutex<HashMap<DraftId, Instant>>,
}

/// [`AccountStatus`], with the send problems by identity.
#[derive(Debug, Default)]
struct Status {
    syncing: bool,
    sync: Option<ServerProblem>,
    send: BTreeMap<IdentityId, SendProblem>,
}

/// What an attempt that [`Engine::report`] records was doing.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Activity<'a> {
    Sync,
    /// A sync begun then that syncs every folder whenever that is due: a step of
    /// background sync, or `sync_account`. Every folder is due once a sync of them all
    /// was asked for (see [`Engine::begin_syncing`]), so one begun after that answers it:
    /// ends [`AccountStatus::syncing`] unless asked again since. Any sync that finds the
    /// login rejected ends it too.
    SyncBegun(Instant),
    /// As this identity of the account, with the address it sends as.
    Send(IdentityId, &'a str),
}

/// Held while syncing (a folder of) an account, or while working on its outbox.
pub(crate) struct Permit {
    _guard: OwnedMutexGuard<()>,
    cancellation: Cancellation,
}

impl Permit {
    /// Runs `work` holding the permit, as [`Cancellation::run`].
    pub async fn run<T>(self, work: impl Future<Output = Result<T>>) -> Result<T> {
        self.cancellation.run(work).await
    }
}

/// Tells a task holding the engine weakly that the engine is gone.
pub(crate) struct Closing(watch::Receiver<()>);

impl Closing {
    /// Resolves once the engine is gone.
    pub async fn closed(&mut self) {
        // Nothing is ever sent: this ends when the sender, owned by the engine, is gone.
        let _ = self.0.changed().await;
    }
}

/// Tells work on an account that the account is being removed.
pub(crate) struct Cancellation {
    removed: watch::Receiver<bool>,
    account: AccountId,
}

impl Cancellation {
    pub fn is_cancelled(&self) -> bool {
        *self.removed.borrow()
    }

    /// Runs `work` on the account. Fails with [`Error::AccountNotFound`] if the account
    /// was removed before, and stops it with [`Error::Cancelled`] if it is removed
    /// meanwhile.
    pub async fn run<T>(mut self, work: impl Future<Output = Result<T>>) -> Result<T> {
        if self.is_cancelled() {
            return Err(Error::AccountNotFound(self.account.to_string()));
        }
        tokio::select! {
            result = work => result,
            () = self.cancelled() => Err(Error::Cancelled),
        }
    }

    /// Resolves once the account is being removed (at once if it already is), or the
    /// engine is gone: work holding the engine only weakly stops then too.
    pub async fn cancelled(&mut self) {
        // Err: the sender, owned by the engine, is gone.
        let _ = self.removed.wait_for(|c| *c).await;
    }
}

impl Engine {
    pub fn new(
        config: Config,
        data_dir_lock: DataDirLock,
        store: Store,
        blobs: BlobStore,
        secrets: Arc<dyn SecretStore>,
        events: broadcast::Sender<Event>,
        tls: Option<TlsConfig>,
    ) -> Self {
        let image_cache = Arc::new(ImageCache::new(config.image_cache_dir()));
        Self {
            config,
            _data_dir_lock: data_dir_lock,
            store,
            blobs,
            secrets,
            events,
            tls: tls.map(OnceLock::from).unwrap_or_default(),
            image_fetcher: OnceLock::new(),
            image_cache,
            outbox_kick: Arc::default(),
            sync_in_background: None,
            draft_upload_delay: DEFAULT_DRAFT_UPLOAD_DELAY,
            tuning: EngineTuning::default(),
            closing: watch::Sender::new(()),
            woke: Cutoff::default(),
            accounts: Mutex::default(),
        }
    }

    /// For background tasks that hold the engine weakly and wait long between rounds: on
    /// a borrowed runtime they would otherwise outlive the client until their timer
    /// fired. Per-account tasks have [`Cancellation::cancelled`] instead.
    pub fn closing(&self) -> Closing {
        Closing(self.closing.subscribe())
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

    pub fn image_fetcher(&self) -> Result<ImageFetcher> {
        if let Some(fetcher) = self.image_fetcher.get() {
            return Ok(fetcher.clone());
        }
        let fetcher = ImageFetcher::new(&self.tls()?)?;
        Ok(self.image_fetcher.get_or_init(|| fetcher).clone())
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
        let cancellation = self.cancellation(account);
        let guard = lock(&self.account(account)).clone().lock_owned().await;
        Permit {
            _guard: guard,
            cancellation,
        }
    }

    /// Wakes the account's background sync, if one runs, to make queued local changes
    /// on the server. A wake while it is busy is kept for when it next waits. Ignored
    /// while the server rejects the login ([`login_rejected`](Self::login_rejected)),
    /// also one kept from before that was known: the changes wait for a login that works,
    /// since each try is another rejected login, which servers count towards locking the
    /// account.
    pub fn wake(&self, account: AccountId) {
        self.account(account).wake.notify_one();
    }

    /// Whether the latest attempt to sync the account found the login rejected.
    pub fn login_rejected(&self, account: AccountId) -> bool {
        self.existing(account).is_some_and(|state| {
            lock(&state.status)
                .sync
                .as_ref()
                .is_some_and(ServerProblem::login_rejected)
        })
    }

    /// What [`wake`](Self::wake) notifies.
    pub fn wake_signal(&self, account: AccountId) -> Arc<Notify> {
        self.account(account).wake.clone()
    }

    /// What [`resume`](Self::resume) notifies.
    pub fn resume_signal(&self, account: AccountId) -> Arc<Notify> {
        self.account(account).resume.clone()
    }

    /// After the computer slept: connections are likely dead, and the monotonic clock
    /// the engine times its work by (`Instant`, tokio's timers) didn't count the sleep on
    /// Linux and macOS. Connections made and syncs begun before now no longer count (see
    /// [`since_woke`](Self::since_woke)), so every folder is due; background sync and
    /// the outbox look at once. Ignored like [`wake`](Self::wake) while the server
    /// rejects the login, which waits as long as ever.
    pub fn resume(&self) {
        self.woke.set(Instant::now());
        for state in lock(&self.accounts).values() {
            state.resume.notify_one();
        }
        self.outbox_kick.notify_one();
    }

    /// Whether `at` is after the computer last woke from sleep. A connection made before
    /// is likely dead; a sync begun before may have seen the server before the sleep,
    /// though it finished after.
    pub fn since_woke(&self, at: Instant) -> bool {
        self.woke.since(at)
    }

    /// Asks for every folder of the account to be synced at once (see
    /// [`MailClient::sync_now`](crate::MailClient::sync_now)), as
    /// [`begin_syncing`](Self::begin_syncing), and wakes background sync to make it.
    pub fn ask_to_sync(&self, account: AccountId) {
        if self.begin_syncing(account) {
            self.account(account).wake.notify_one();
        }
    }

    /// Syncs of every folder of the account begun before now no longer count, so
    /// background sync's next step makes one, and the account shows as syncing until an
    /// attempt begun after this ends. Called on its own before background sync's task is
    /// spawned, for its first step, so that an account just added shows as syncing at
    /// once. Returns whether it did: not while the account is syncing already, which
    /// the sync under way or due answers, nor while the server rejects the login, which
    /// waits as long as ever (see [`wake`](Self::wake)).
    pub fn begin_syncing(&self, account: AccountId) -> bool {
        let state = self.account(account);
        {
            // Held while asking: an attempt ending meanwhile can't take the account out
            // of syncing after this put it in.
            let mut status = lock(&state.status);
            let rejected = status
                .sync
                .as_ref()
                .is_some_and(ServerProblem::login_rejected);
            if status.syncing || rejected {
                return false;
            }
            state.asked.set(Instant::now());
            status.syncing = true;
        }
        self.emit(Event::AccountStatusChanged { account });
        true
    }

    /// Records that the draft was just changed (or created), so background sync leaves
    /// it for [`draft_upload_delay`](Self::draft_upload_delay): a frontend saving as the
    /// user types would otherwise replace the copy on the server with each keystroke.
    /// Returns whether it was not held back already. Only background sync (`watch`)
    /// holds drafts back and forgets these: without it, nothing is recorded.
    pub fn draft_edited(&self, account: AccountId, draft: DraftId) -> bool {
        if self.sync_in_background.is_none() {
            return true;
        }
        lock(&self.account(account).draft_edits)
            .insert(draft, Instant::now())
            .is_none_or(|at| at.elapsed() >= self.draft_upload_delay)
    }

    /// Forgets the draft's last change, once it is deleted or being sent.
    pub fn draft_gone(&self, account: AccountId, draft: DraftId) {
        if let Some(state) = self.existing(account) {
            lock(&state.draft_edits).remove(&draft);
        }
    }

    /// The account's drafts changed within the last
    /// [`draft_upload_delay`](Self::draft_upload_delay), which background sync doesn't
    /// upload yet. Uploads the user asks for don't wait.
    pub fn held_drafts(&self, account: AccountId) -> HashSet<DraftId> {
        let Some(state) = self.existing(account) else {
            return HashSet::new();
        };
        lock(&state.draft_edits)
            .iter()
            .filter(|(_, at)| at.elapsed() < self.draft_upload_delay)
            .map(|(draft, _)| *draft)
            .collect()
    }

    /// How long until the first draft held back is due for upload; zero for one that
    /// became due after the last [`forget_settled_drafts`](Self::forget_settled_drafts)
    /// (during a sync that left it alone, say), which the next one uploads.
    pub fn next_draft_due(&self, account: AccountId) -> Option<Duration> {
        lock(&self.existing(account)?.draft_edits)
            .values()
            .map(|at| self.draft_upload_delay.saturating_sub(at.elapsed()))
            .min()
    }

    /// Forgets the drafts no longer held back, as background sync begins a step, which
    /// uploads them.
    pub fn forget_settled_drafts(&self, account: AccountId) {
        if let Some(state) = self.existing(account) {
            lock(&state.draft_edits).retain(|_, at| at.elapsed() < self.draft_upload_delay);
        }
    }

    /// Records that every folder of the account was just synced, by a sync that `began`
    /// then.
    pub fn set_synced(&self, account: AccountId, began: Instant) {
        let state = self.account(account);
        // Taken before the check: see `Cutoff::finished`.
        let finished = Instant::now();
        if self.full_sync_counts(&state, began) {
            *lock(&state.synced) = Some(finished);
        }
    }

    /// When every folder of the account was last synced, if ever since the engine
    /// started, the computer last woke and such a sync was last asked for.
    pub fn synced(&self, account: AccountId) -> Option<Instant> {
        let state = self.existing(account)?;
        let synced = *lock(&state.synced);
        synced.filter(|&at| self.full_sync_counts(&state, at))
    }

    /// Whether a sync of every folder of the account, begun or finished `at`, counts:
    /// after the computer last woke, and such a sync was last asked for.
    fn full_sync_counts(&self, state: &AccountState, at: Instant) -> bool {
        self.since_woke(at) && state.asked.since(at)
    }

    /// Records that `folder` just completed a sync, which `began` then: its messages and
    /// all their flags.
    pub fn set_folder_synced(&self, account: AccountId, folder: FolderId, began: Instant) {
        if let Some(at) = self.woke.finished(began, Instant::now) {
            lock(&self.account(account).folders_synced).insert(folder, at);
        }
    }

    /// When `folder` last completed a sync, if ever since the engine started and the
    /// computer last woke.
    pub fn folder_synced(&self, account: AccountId, folder: FolderId) -> Option<Instant> {
        let synced = lock(&self.existing(account)?.folders_synced)
            .get(&folder)
            .copied();
        synced.filter(|&at| self.since_woke(at))
    }

    /// Records how an attempt to get through to the account's server for `activity`
    /// ended, and announces what that changes in [`status`](Self::status). Returns the
    /// failures in a row, this one included (0 for a success). The account being removed
    /// says nothing about the server, and isn't recorded.
    pub fn report<T>(&self, account: AccountId, activity: Activity, outcome: &Result<T>) -> u32 {
        let e = match outcome {
            Err(Error::Cancelled | Error::AccountNotFound(_) | Error::IdentityNotFound(_)) => {
                return 0;
            }
            Err(e) => Some(e),
            Ok(_) => None,
        };
        let state = self.account(account);
        // Whether the status changed, and for a success, whether the login had been rejected;
        // whether the problem did.
        let (changed, rejected, failures, problem_changed) = {
            let mut status = lock(&state.status);
            match activity {
                Activity::Sync | Activity::SyncBegun(_) => {
                    let (changed, rejected, failures) = record(&mut status.sync, e);
                    let answered = match activity {
                        Activity::SyncBegun(began) => state.asked.since(began),
                        _ => false,
                    };
                    // Nor is it answered while the login is rejected, which background
                    // sync waits out (see `wake`), whenever the rejected attempt began.
                    let refused = status
                        .sync
                        .as_ref()
                        .is_some_and(ServerProblem::login_rejected);
                    let done = status.syncing && (answered || refused);
                    if done {
                        status.syncing = false;
                    }
                    (changed || done, rejected, failures, changed)
                }
                Activity::Send(identity, email) => {
                    let mut slot = status.send.remove(&identity).map(|p| p.problem);
                    let recorded = record(&mut slot, e);
                    if let Some(problem) = slot {
                        let email = email.to_owned();
                        let problem = SendProblem {
                            identity,
                            email,
                            problem,
                        };
                        status.send.insert(identity, problem);
                    }
                    let (changed, rejected, failures) = recorded;
                    (changed, rejected, failures, changed)
                }
            }
        };
        // What frontends show, so what a report from a user is about: logged as it changes,
        // not at each attempt.
        if problem_changed {
            let identity = match activity {
                Activity::Send(identity, _) => Some(identity),
                Activity::Sync | Activity::SyncBegun(_) => None,
            };
            let identity = identity.map(tracing::field::display);
            match e {
                Some(e) => tracing::info!(%account, identity, error = %e, failures,
                    "can't get through to the server"),
                None => tracing::info!(%account, identity, "getting through to the server again"),
            }
        }
        if changed {
            self.emit(Event::AccountStatusChanged { account });
        }
        // Background sync stopped retrying when the login was rejected (see `wake`):
        // through again, it needn't wait any longer.
        if let (Activity::Sync | Activity::SyncBegun(_), true) = (activity, rejected) {
            state.wake.notify_one();
        }
        failures
    }

    /// Drops what was found about sending as an identity that is gone.
    pub fn forget_identity(&self, account: AccountId, identity: IdentityId) {
        let Some(state) = self.existing(account) else {
            return;
        };
        if lock(&state.status).send.remove(&identity).is_some() {
            self.emit(Event::AccountStatusChanged { account });
        }
    }

    /// What the latest attempts to get through to the account's servers found.
    pub fn status(&self, account: AccountId) -> AccountStatus {
        let Some(state) = self.existing(account) else {
            return AccountStatus::default();
        };
        let status = lock(&state.status);
        AccountStatus {
            syncing: status.syncing,
            sync: status.sync.clone(),
            send: status.send.values().cloned().collect(),
        }
    }

    /// For work on `account` without a permit. Obtain it before checking that the
    /// account (or the row worked on) still exists: a removal is then seen whether it
    /// started before or after.
    pub fn cancellation(&self, account: AccountId) -> Cancellation {
        Cancellation {
            removed: self.account(account).removed.subscribe(),
            account,
        }
    }

    /// A logged-in connection that was recently in use, if there is one.
    pub fn take_session(&self, account: AccountId) -> Option<Session> {
        let state = self.existing(account)?;
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

    /// The account's state, created on first use. Only what work on the account needs
    /// creates it: frontends can ask about any id, and the map never shrinks.
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
                    resume: Arc::default(),
                    synced: Mutex::default(),
                    asked: Cutoff::default(),
                    folders_synced: Mutex::default(),
                    status: Mutex::default(),
                    draft_edits: Mutex::default(),
                })
            })
            .clone()
    }

    /// The account's state, if anything created it: what it holds starts out empty.
    fn existing(&self, account: AccountId) -> Option<Arc<AccountState>> {
        lock(&self.accounts).get(&account).cloned()
    }

    #[cfg(test)]
    pub fn tracked_accounts(&self) -> usize {
        lock(&self.accounts).len()
    }

    #[cfg(test)]
    pub fn tracked_draft_edits(&self, account: AccountId) -> usize {
        self.existing(account)
            .map_or(0, |state| lock(&state.draft_edits).len())
    }
}

/// A panic while holding one of these locks can't leave the data inconsistent.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
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

/// Records an attempt's outcome (`None`: success) in what is known of a server. Returns
/// whether that changed, whether a success follows a rejected login, and the failures in
/// a row.
fn record(slot: &mut Option<ServerProblem>, e: Option<&Error>) -> (bool, bool, u32) {
    match e {
        None => {
            let cleared = slot.take();
            let rejected = cleared.as_ref().is_some_and(ServerProblem::login_rejected);
            (cleared.is_some(), rejected, 0)
        }
        Some(e) => {
            let error = ReportedError::from(e);
            // Not each failure: what is wrong is what frontends show.
            match slot {
                Some(problem) => {
                    let changed = problem.error != error;
                    problem.failures += 1;
                    problem.error = error;
                    (changed, false, problem.failures)
                }
                None => {
                    *slot = Some(ServerProblem {
                        error,
                        since: now(),
                        failures: 1,
                    });
                    (true, false, 1)
                }
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Work begun before the computer slept, finishing as it wakes: whenever the wake
    /// comes, the record doesn't count.
    #[test]
    fn work_begun_before_a_wake_never_counts() {
        let began = Instant::now();
        let later = |by| began + Duration::from_secs(by);

        // As the time is taken.
        let woke = Cutoff::default();
        let finished = woke.finished(began, || {
            woke.set(later(1));
            later(2)
        });
        assert!(finished.is_none_or(|at| !woke.since(at)));

        // Between the check and the record.
        let woke = Cutoff::default();
        let finished = woke.finished(began, || later(1));
        woke.set(later(2));
        assert!(finished.is_none_or(|at| !woke.since(at)));

        // Work begun after a wake counts.
        let woke = Cutoff::default();
        woke.set(began);
        let finished = woke.finished(later(1), || later(2));
        assert!(finished.is_some_and(|at| woke.since(at)));
    }
}
