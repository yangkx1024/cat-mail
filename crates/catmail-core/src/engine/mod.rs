//! The engine: the state every `MailClient` clone and every engine task share
//! ([`Engine`]), and what it holds and hands out. The data directory and the keyring
//! ([`storage`]: where they are, [`config`]; the passwords, [`secrets`]; remote images
//! shown, [`image_cache`]); each account's idle connections ([`pool`]) and the rules for
//! taking one ([`connection`]), what its servers last said ([`status`]), its searches'
//! connection ([`search_slot`]) and the bodies being fetched for someone waiting
//! ([`downloading`]); what it is set to do ([`settings`]); the events it announces after
//! a commit ([`events`]); the runtime its work runs on ([`runtime`]); and noticing that
//! the computer slept ([`sleep`]).

pub(crate) mod config;
pub(crate) mod connection;
pub(crate) mod downloading;
pub(crate) mod events;
pub(crate) mod image_cache;
pub(crate) mod pool;
pub(crate) mod runtime;
pub(crate) mod search_slot;
pub(crate) mod secrets;
pub(crate) mod settings;
pub(crate) mod sleep;
pub(crate) mod status;
pub(crate) mod storage;

use std::collections::{HashMap, HashSet};
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use catmail_proto::TlsConfig;
use catmail_proto::http::ImageFetcher;
use catmail_proto::imap::Session;
use catmail_store::Db;
use catmail_types::{AccountId, AccountStatus, DraftId, Event, FolderId, IdentityId, MessageId};
use tokio::sync::{Notify, OwnedMutexGuard, broadcast, watch};
use tokio::task::unconstrained;

use crate::task::{Cancellation, Closing, Cutoff, blocking, lock};
use crate::{Error, Result};
use downloading::{Claim, Downloading};
use pool::Pool;
use search_slot::SearchSlot;
use settings::EngineSettings;
use status::{Activity, Failures, Recorded, Status};
use storage::Storage;

/// State shared by every `MailClient` clone and every engine task.
pub(crate) struct Engine {
    /// The data directory, locked, and the keyring.
    storage: Storage,
    events: broadcast::Sender<Event>,
    /// Set by the builder, else the platform verifier, created on first use.
    tls: OnceLock<TlsConfig>,
    /// Created on first use: most sessions never load a remote image.
    image_fetcher: OnceLock<ImageFetcher>,
    /// Wakes the outbox worker, if one runs: something became due.
    outbox_kick: Arc<Notify>,
    settings: EngineSettings,
    /// Never sent to: dropped with the engine, which ends the background tasks holding
    /// it weakly at once (see [`closing`](Self::closing)).
    closing: watch::Sender<()>,
    /// When the computer last woke from sleep, as [`Engine::resume`] was told.
    woke: Arc<Cutoff>,
    accounts: Mutex<HashMap<AccountId, Arc<AccountState>>>,
    /// Folders being deleted on the server (`folders::delete`): local changes to their
    /// messages are refused meanwhile, as the deletion would take those along. Shared
    /// with the write transactions that check it.
    deleting: Arc<Mutex<HashSet<FolderId>>>,
    /// Drafts open in an editor, and in how many (`drafts::EditMark`): changes to them
    /// from anyone else are refused. Shared with the write transactions that check it.
    editing: Editing,
    /// Bodies being fetched for someone waiting on them: another fetch waits for that.
    downloading: Downloading,
}

/// Drafts open in an editor, by how many ([`Engine::editing`]).
pub(crate) type Editing = Arc<Mutex<HashMap<DraftId, usize>>>;

/// Folders marked as being deleted, by [`Engine::deleting`]; unmarked when dropped.
pub(crate) struct Deleting {
    set: Arc<Mutex<HashSet<FolderId>>>,
    folders: Vec<FolderId>,
}

impl Drop for Deleting {
    fn drop(&mut self) {
        let mut set = lock(&self.set);
        for folder in &self.folders {
            set.remove(folder);
        }
    }
}

/// What the engine keeps per account.
///
/// A removed account keeps its entry, marked `removed`, so work that looks at it later
/// is cancelled at once rather than racing the removal. Account ids are never reused.
struct AccountState {
    /// Serializes syncs. A sync of every folder holds it for one folder at a time (see
    /// `sync::run`); the lock is fair, so whatever waits for it meanwhile goes next.
    sync: AccountLock,
    /// Serializes work on the outbox, so an entry is never sent or saved twice at once.
    send: AccountLock,
    /// Set when removal starts; cancels running syncs and on-demand fetches.
    removed: watch::Sender<bool>,
    /// Logged-in connections for on-demand fetches (see [`pool`]).
    pool: Arc<Pool>,
    /// The connection searches on the server use (see [`search`](crate::search)).
    search: Arc<SearchSlot>,
    /// Wakes the account's background sync, if one runs: local changes are waiting for
    /// the server.
    wake: Arc<Notify>,
    /// Tells the account's background sync, if one runs, to look at once whether its
    /// connection and the last sync of every folder are from before a sleep (see
    /// [`Engine::resume`]).
    resume: Arc<Notify>,
    /// Notified each time a sync of every folder finishes (see
    /// [`Engine::synced_signal`]).
    synced_signal: Arc<Notify>,
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

/// A fair lock on work on an account, which counts the callers waiting for it: tests
/// hold work at the fake server and wait until a call is queued behind it. Only the
/// sync lock's count is read ([`Engine::sync_waiting`]); the send lock counts too, so
/// that both are taken the same way.
#[derive(Default)]
struct AccountLock {
    mutex: Arc<tokio::sync::Mutex<()>>,
    waiting: AtomicUsize,
}

impl AccountLock {
    async fn lock(&self) -> OwnedMutexGuard<()> {
        // Unconstrained: out of its task's budget, the first poll would be Pending without
        // queueing the call. Only this one acquisition is exempt.
        let mut locking = pin!(unconstrained(self.mutex.clone().lock_owned()));
        // Uncounted however the wait ends, given up on too.
        let mut counted = None;
        poll_fn(|cx| {
            let poll = locking.as_mut().poll(cx);
            // Counted only once the first poll has put the call in the lock's queue: a
            // test that sees the count may let the holder go, and the lock must then
            // pass to this call, not back to the holder.
            if poll.is_pending() && counted.is_none() {
                self.waiting.fetch_add(1, Ordering::Release);
                counted = Some(Uncount(&self.waiting));
            }
            poll
        })
        .await
    }
}

struct Uncount<'a>(&'a AtomicUsize);

impl Drop for Uncount<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Engine {
    pub fn new(
        storage: Storage,
        events: broadcast::Sender<Event>,
        tls: Option<TlsConfig>,
        settings: EngineSettings,
    ) -> Self {
        Self {
            storage,
            events,
            tls: tls.map(OnceLock::from).unwrap_or_default(),
            image_fetcher: OnceLock::new(),
            outbox_kick: Arc::default(),
            settings,
            closing: watch::Sender::new(()),
            woke: Arc::default(),
            accounts: Mutex::default(),
            deleting: Arc::default(),
            editing: Arc::default(),
            downloading: Downloading::default(),
        }
    }

    /// For blocking work, which needs no more of the engine; it holds the data directory
    /// locked until dropped (see [`Storage::released`]).
    pub fn storage(&self) -> Storage {
        self.storage.clone()
    }

    /// What the builder set, which stays as it is.
    pub fn settings(&self) -> EngineSettings {
        self.settings
    }

    /// The drafts open in an editor, to mark one or check from inside a write
    /// transaction.
    pub fn editing(&self) -> Editing {
        self.editing.clone()
    }

    /// Marks `folders` as being deleted until the guard returned is dropped.
    pub fn deleting(&self, folders: Vec<FolderId>) -> Deleting {
        lock(&self.deleting).extend(folders.iter().copied());
        Deleting {
            set: self.deleting.clone(),
            folders,
        }
    }

    /// The folders being deleted, to check from inside a write transaction.
    pub fn being_deleted(&self) -> Arc<Mutex<HashSet<FolderId>>> {
        self.deleting.clone()
    }

    /// For background tasks that hold the engine weakly and wait long between rounds: on
    /// a borrowed runtime they would otherwise outlive the client until their timer
    /// fired. Per-account tasks have [`Cancellation::cancelled`] instead.
    pub fn closing(&self) -> Closing {
        Closing::new(self.closing.subscribe())
    }

    /// Called from inside the task performing the mutation, so dropping the caller's
    /// future can't suppress the notification for a change that did happen.
    pub fn emit(&self, event: Event) {
        // No subscribers is fine.
        let _ = self.events.send(event);
    }

    /// The events emitted from now on; closed once the engine is.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Wakes the outbox worker, if one runs: something became due.
    pub fn kick_outbox(&self) {
        self.outbox_kick.notify_one();
    }

    /// What [`kick_outbox`](Self::kick_outbox) notifies, for the worker to wait on
    /// without holding the engine.
    pub fn outbox_signal(&self) -> Arc<Notify> {
        self.outbox_kick.clone()
    }

    /// Claims fetching the body of `id` (see [`Downloading`]).
    pub fn claim_body(&self, id: MessageId) -> Claim {
        self.downloading.claim(id)
    }

    /// Whether someone is fetching the body of `id` and waiting on it.
    pub fn body_claimed(&self, id: MessageId) -> bool {
        self.downloading.claimed(id)
    }

    /// Runs a read on the blocking pool of the current (engine) runtime.
    pub async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Db<'_>) -> catmail_store::Result<T> + Send + 'static,
    ) -> Result<T> {
        let storage = self.storage();
        blocking(move || storage.read(f)).await
    }

    /// Runs a write transaction on the blocking pool. Emit events after this returns.
    pub async fn write<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Db<'_>) -> catmail_store::Result<T> + Send + 'static,
    ) -> Result<T> {
        let storage = self.storage();
        blocking(move || storage.write(f)).await
    }

    /// [`write`](Self::write), on disk even across a power cut; see
    /// [`Store::write_durable`](catmail_store::Store::write_durable).
    pub async fn write_durable<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Db<'_>) -> catmail_store::Result<T> + Send + 'static,
    ) -> Result<T> {
        let storage = self.storage();
        blocking(move || storage.write_durable(f)).await
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

    /// How many calls wait for [`sync_permit`](Self::sync_permit) on `account`.
    #[cfg(feature = "test-support")]
    pub fn sync_waiting(&self, account: AccountId) -> usize {
        self.existing(account)
            .map_or(0, |state| state.sync.waiting.load(Ordering::Acquire))
    }

    async fn permit(
        &self,
        account: AccountId,
        lock: impl FnOnce(&AccountState) -> &AccountLock,
    ) -> Permit {
        let cancellation = self.cancellation(account);
        let guard = lock(&self.account(account)).lock().await;
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
    /// account. Ignored as well while the server says the account is over its download
    /// limit ([`ServerProblem::over_limit`](catmail_types::ServerProblem::over_limit)), unless a sync was asked for: the changes
    /// wait for it to lift rather than add to it (see [`ignores_wakes`](Self::ignores_wakes)).
    pub fn wake(&self, account: AccountId) {
        self.account(account).wake.notify_one();
    }

    /// Whether the latest attempt to sync the account found the login rejected.
    pub fn login_rejected(&self, account: AccountId) -> bool {
        self.existing(account)
            .is_some_and(|state| lock(&state.status).login_rejected())
    }

    /// Whether the latest attempt to sync the account found a failure background sync
    /// waits out ([`ServerProblem::waited_out`](catmail_types::ServerProblem::waited_out)).
    pub fn waits_out(&self, account: AccountId) -> bool {
        self.existing(account)
            .is_some_and(|state| lock(&state.status).waits_out())
    }

    /// Whether background sync ignores [`wake`](Self::wake) for now: the server says the
    /// account is over its limit and no sync of every folder was asked for since
    /// ([`begin_syncing`](Self::begin_syncing)), which the user waits for; or it rejects
    /// the login, unless background sync is `logged_in` already (idling on a connection
    /// from before), which a rejected login doesn't hold up. Also when another sync found
    /// that out, not background sync itself.
    pub fn ignores_wakes(&self, account: AccountId, logged_in: bool) -> bool {
        self.existing(account)
            .is_some_and(|state| lock(&state.status).ignores_wakes(logged_in))
    }

    pub fn wake_signal(&self, account: AccountId) -> Arc<Notify> {
        self.account(account).wake.clone()
    }

    pub fn resume_signal(&self, account: AccountId) -> Arc<Notify> {
        self.account(account).resume.clone()
    }

    /// After the computer slept: connections are likely dead, and the monotonic clock
    /// the engine times its work by (`Instant`, tokio's timers) didn't count the sleep on
    /// Linux and macOS. Connections made and syncs begun before now no longer count (see
    /// [`since_woke`](Self::since_woke)), so every folder is due; background sync and
    /// the outbox look at once. Ignored like [`wake`](Self::wake) while the server
    /// rejects the login, which waits as long as ever; not while the account is over its
    /// limit, whose wait the timers didn't count the sleep towards either.
    pub fn resume(&self) {
        self.woke.set(Instant::now());
        for state in lock(&self.accounts).values() {
            state.resume.notify_one();
        }
        self.kick_outbox();
    }

    /// Whether `at` is after the computer last woke from sleep. A connection made before
    /// is likely dead; a sync begun before may have seen the server before the sleep,
    /// though it finished after.
    pub fn since_woke(&self, at: Instant) -> bool {
        self.woke.since(at)
    }

    /// Whether a connection is worth using (see [`pool::usable`]).
    pub fn usable(&self, session: &Session) -> bool {
        pool::usable(&self.woke, session)
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
            if !status.begin_syncing() {
                return false;
            }
            state.asked.set(Instant::now());
        }
        self.emit(Event::AccountStatusChanged { account });
        true
    }

    /// Records that the draft was just changed (or created), so background sync leaves
    /// it for [`draft_upload_delay`](EngineSettings::draft_upload_delay): a frontend saving as the
    /// user types would otherwise replace the copy on the server with each keystroke.
    /// Returns whether it was not held back already. Only background sync (`watch`)
    /// holds drafts back and forgets these: without it, nothing is recorded.
    pub fn draft_edited(&self, account: AccountId, draft: DraftId) -> bool {
        if self.settings.sync_in_background.is_none() {
            return true;
        }
        lock(&self.account(account).draft_edits)
            .insert(draft, Instant::now())
            .is_none_or(|at| at.elapsed() >= self.settings.draft_upload_delay)
    }

    /// Forgets the draft's last change, once it is deleted or being sent.
    pub fn draft_gone(&self, account: AccountId, draft: DraftId) {
        if let Some(state) = self.existing(account) {
            lock(&state.draft_edits).remove(&draft);
        }
    }

    /// The account's drafts changed within the last
    /// [`draft_upload_delay`](EngineSettings::draft_upload_delay), which background sync doesn't
    /// upload yet. Uploads the user asks for don't wait.
    pub fn held_drafts(&self, account: AccountId) -> HashSet<DraftId> {
        let Some(state) = self.existing(account) else {
            return HashSet::new();
        };
        lock(&state.draft_edits)
            .iter()
            .filter(|(_, at)| at.elapsed() < self.settings.draft_upload_delay)
            .map(|(draft, _)| *draft)
            .collect()
    }

    /// How long until the first draft held back is due for upload; zero for one that
    /// became due after the last [`forget_settled_drafts`](Self::forget_settled_drafts)
    /// (during a sync that left it alone, say), which the next one uploads.
    pub fn next_draft_due(&self, account: AccountId) -> Option<Duration> {
        lock(&self.existing(account)?.draft_edits)
            .values()
            .map(|at| {
                self.settings
                    .draft_upload_delay
                    .saturating_sub(at.elapsed())
            })
            .min()
    }

    /// Forgets the drafts no longer held back, as background sync begins a step, which
    /// uploads them.
    pub fn forget_settled_drafts(&self, account: AccountId) {
        if let Some(state) = self.existing(account) {
            lock(&state.draft_edits)
                .retain(|_, at| at.elapsed() < self.settings.draft_upload_delay);
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
        state.synced_signal.notify_waiters();
    }

    /// Notified to those waiting each time a sync of every folder of the account
    /// finishes, whether or not it counts (see [`synced`](Self::synced)): nothing else
    /// tells of one that changed nothing.
    pub fn synced_signal(&self, account: AccountId) -> Arc<Notify> {
        self.account(account).synced_signal.clone()
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
    /// failures in a row. The account being removed says nothing about the server, and
    /// isn't recorded.
    pub fn report<T>(
        &self,
        account: AccountId,
        activity: Activity,
        outcome: &Result<T>,
    ) -> Failures {
        let e = match outcome {
            Err(Error::Cancelled | Error::AccountNotFound(_) | Error::IdentityNotFound(_)) => {
                return Failures::default();
            }
            Err(e) => Some(e),
            Ok(_) => None,
        };
        let state = self.account(account);
        // Answered under the lock: an ask meanwhile would otherwise be taken as answered.
        let Recorded {
            changed,
            problem_changed,
            waited,
            failures,
        } = lock(&state.status).record(
            activity,
            e,
            |began| state.asked.since(began),
            |at| !self.since_woke(at),
        );
        // What frontends show, so what a report from a user is about: logged as it changes,
        // not at each attempt.
        if problem_changed {
            let identity = match activity {
                Activity::Send(identity, _) => Some(identity),
                Activity::Sync | Activity::SyncBegun(_) => None,
            };
            let identity = identity.map(tracing::field::display);
            match e {
                Some(e) => tracing::info!(%account, identity, error = %e,
                    failures = failures.in_a_row, "can't get through to the server"),
                None => tracing::info!(%account, identity, "getting through to the server again"),
            }
        }
        if changed {
            self.emit(Event::AccountStatusChanged { account });
        }
        // Background sync slowed down retrying when the login was rejected or the account
        // went over its limit (see `wake`): through again, it needn't wait any longer.
        if let (Activity::Sync | Activity::SyncBegun(_), true) = (activity, waited) {
            state.wake.notify_one();
        }
        failures
    }

    /// Drops what was found about sending as an identity that is gone.
    pub fn forget_identity(&self, account: AccountId, identity: IdentityId) {
        let Some(state) = self.existing(account) else {
            return;
        };
        let changed = lock(&state.status).forget_identity(identity);
        if changed {
            self.emit(Event::AccountStatusChanged { account });
        }
    }

    /// What the latest attempts to get through to the account's servers found.
    pub fn status(&self, account: AccountId) -> AccountStatus {
        let Some(state) = self.existing(account) else {
            return AccountStatus::default();
        };
        lock(&state.status).shown()
    }

    /// For work on `account` without a permit. Obtain it before checking that the
    /// account (or the row worked on) still exists: a removal is then seen whether it
    /// started before or after.
    pub fn cancellation(&self, account: AccountId) -> Cancellation {
        Cancellation::new(self.account(account).removed.subscribe(), account)
    }

    pub fn pool(&self, account: AccountId) -> Arc<Pool> {
        self.account(account).pool.clone()
    }

    pub fn search_slot(&self, account: AccountId) -> Arc<SearchSlot> {
        self.account(account).search.clone()
    }

    /// Marks `account` removed: running syncs and fetches are cancelled, idle connections
    /// closed. Waits until a running sync has stopped.
    pub async fn start_removal(&self, account: AccountId) -> OwnedMutexGuard<()> {
        let state = self.account(account);
        state.removed.send_replace(true);
        state.pool.close();
        // One a search holds goes with it, cancelled.
        state.search.close_idle();
        state.sync.lock().await
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
                    sync: AccountLock::default(),
                    send: AccountLock::default(),
                    removed: watch::Sender::new(false),
                    pool: Arc::new(Pool::new(
                        account,
                        self.settings.tuning.keepalive,
                        self.closing(),
                        self.woke.clone(),
                    )),
                    search: Arc::default(),
                    wake: Arc::default(),
                    resume: Arc::default(),
                    synced_signal: Arc::default(),
                    synced: Mutex::default(),
                    asked: Cutoff::default(),
                    folders_synced: Mutex::default(),
                    status: Mutex::new(Status::new()),
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

#[cfg(test)]
mod tests {
    use std::task::Poll;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lock_counts_a_call_once_it_is_queued_until_it_stops_waiting() {
        let lock = Arc::new(AccountLock::default());
        let waiting = || lock.waiting.load(Ordering::Acquire);
        let holder = lock.lock().await;
        assert_eq!(waiting(), 0, "taken at once: never waited");

        let queued = tokio::spawn({
            let lock = lock.clone();
            async move { lock.lock().await }
        });
        while waiting() == 0 {
            tokio::task::yield_now().await;
        }
        // Counted only once queued: let go of, the lock goes to it, never to a newcomer.
        drop(holder);
        assert!(lock.mutex.try_lock().is_err());
        drop(queued.await.unwrap());
        assert_eq!(waiting(), 0);

        // Given up on while waiting, it is uncounted too.
        let holder = lock.lock().await;
        let given_up = tokio::spawn({
            let lock = lock.clone();
            async move { lock.lock().await }
        });
        while waiting() == 0 {
            tokio::task::yield_now().await;
        }
        given_up.abort();
        assert!(given_up.await.unwrap_err().is_cancelled());
        assert_eq!(waiting(), 0);
        drop(holder);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_call_out_of_budget_is_counted_once_it_is_queued() {
        let lock = Arc::new(AccountLock::default());
        let waiting = || lock.waiting.load(Ordering::Acquire);
        let holder = lock.lock().await;

        let queued = tokio::spawn({
            let lock = lock.clone();
            async move {
                // Out of its budget, a task's polls of tokio's resources are Pending, the
                // lock's before it queues the call.
                poll_fn(|cx| {
                    while tokio::task::coop::has_budget_remaining() {
                        let coop = std::task::ready!(tokio::task::coop::poll_proceed(cx));
                        coop.made_progress();
                    }
                    Poll::Ready(())
                })
                .await;
                lock.lock().await
            }
        });
        // One thread: the call runs only while this waits.
        while waiting() == 0 {
            tokio::task::yield_now().await;
        }
        drop(holder);
        assert!(
            lock.mutex.try_lock().is_err(),
            "counted before it was queued"
        );
        drop(queued.await.unwrap());
        assert_eq!(waiting(), 0);
    }
}
