use std::fs::{self, File, TryLockError};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use catmail_proto::TlsConfig;
use catmail_store::{BlobStore, Store};
use catmail_types::{
    Account, AccountId, AccountStatus, AttachmentId, Draft, DraftAttachment, DraftAttachmentId,
    DraftContent, DraftId, Event, Flags, Folder, FolderId, FolderSyncReport, GcReport, Message,
    MessageId, MessageSummary, NewAccount, OutboxEntry, OutboxId, PendingOp, PendingOpId,
    ReplayReport, SearchQuery, SyncReport,
};
use secrecy::{ExposeSecret, SecretString};
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::engine::{DEFAULT_DRAFT_UPLOAD_DELAY, Engine, blocking, now};
use crate::runtime::Runtime;
use crate::secrets::{KeyringSecrets, SecretStore};
use crate::watch::{self, SyncSchedule};
use crate::{Config, Error, Result, auth, body, connection, drafts, gc, ops, send, sync};

const EVENT_CAPACITY: usize = 256;

/// The only entry point frontends use. Cheap to clone; all clones share one engine.
#[derive(Clone)]
pub struct MailClient {
    // Kept apart from `Engine` so tasks running on the runtime never hold the last
    // reference to the runtime itself.
    rt: Arc<Runtime>,
    engine: Arc<Engine>,
}

pub struct MailClientBuilder {
    config: Config,
    runtime: Option<Handle>,
    secrets: Option<Arc<dyn SecretStore>>,
    tls: Option<TlsConfig>,
    send_in_background: bool,
    sync_in_background: Option<SyncSchedule>,
    collect_garbage_every: Option<Duration>,
    draft_upload_delay: Duration,
}

impl MailClientBuilder {
    /// Run engine work on an existing tokio runtime instead of creating one.
    /// Use this when the caller already lives inside tokio (e.g. the CLI).
    pub fn runtime(mut self, handle: Handle) -> Self {
        self.runtime = Some(handle);
        self
    }

    /// Replace the OS keyring, e.g. with [`crate::MemorySecrets`] in tests.
    pub fn secrets(mut self, secrets: Arc<dyn SecretStore>) -> Self {
        self.secrets = Some(secrets);
        self
    }

    /// How server certificates are verified [default: the platform verifier].
    pub fn tls(mut self, tls: TlsConfig) -> Self {
        self.tls = Some(tls);
        self
    }

    /// Send queued messages in the background as they come due, retrying with backoff,
    /// for as long as the client exists [default: off]. For long-running frontends
    /// (the GUI); a short-lived one calls [`MailClient::flush_outbox`] instead, so that
    /// nothing is sent behind the user's back by an unrelated command.
    pub fn send_in_background(mut self, on: bool) -> Self {
        self.send_in_background = on;
        self
    }

    /// Keep every account in sync in the background for as long as the client exists
    /// [default: off]: all folders now and then, INBOX as it changes (pushed by the
    /// server with IDLE, else checked often), and changes to messages made on the server
    /// as soon as they are queued. For long-running frontends; a short-lived one calls
    /// [`MailClient::sync_account`] and [`MailClient::replay_pending`] instead. The
    /// [`SyncSchedule`] says how often, `SyncSchedule::default()` for the usual.
    pub fn sync_in_background(mut self, schedule: Option<SyncSchedule>) -> Self {
        self.sync_in_background = schedule;
        self
    }

    /// Delete stored bodies and attachments nothing refers to any more (see
    /// [`MailClient::collect_garbage`]) this often, for as long as the client exists,
    /// starting a few minutes after it opens [default: never]. For long-running
    /// frontends: while one has the data directory, nothing else can do it.
    pub fn collect_garbage_every(mut self, every: Option<Duration>) -> Self {
        self.collect_garbage_every = every;
        self
    }

    /// How long background sync waits after a draft last changed before copying it to
    /// the server's Drafts folder [default: [`DEFAULT_DRAFT_UPLOAD_DELAY`], 15 seconds],
    /// so that a frontend saving as the user types doesn't replace the copy with each
    /// save. Uploads asked for ([`MailClient::replay_pending`],
    /// [`MailClient::sync_account`]) don't wait.
    pub fn draft_upload_delay(mut self, delay: Duration) -> Self {
        self.draft_upload_delay = delay;
        self
    }

    /// Locks the data directory, empties [`Config::handoff_dir`], then opens the database
    /// (migrating it if needed) and the blob store. Blocks briefly. Fails with
    /// [`Error::AlreadyRunning`] if another engine, in this or another process, has the
    /// directory open, and with [`Error::DataDirNotOwned`] if it belongs to another user.
    /// Sends a previous engine left unfinished before the end of the message go back to
    /// the queue.
    pub fn open(self) -> Result<MailClient> {
        let data_dir_lock = lock_data_dir(&self.config)?;
        let handoff_dir = self.config.handoff_dir();
        clear_handoff_dir(&handoff_dir);
        catmail_store::create_private_dir(&handoff_dir)?;
        let rt = match self.runtime {
            Some(handle) => Runtime::shared(handle),
            None => Runtime::owned()?,
        };
        let store = Store::open(&self.config.db_path())?;
        // The lock means no engine is sending; see Outbox::recover_interrupted.
        let interrupted = store.write(|db| db.outbox().recover_interrupted(None, now()))?;
        if interrupted > 0 {
            tracing::info!(
                interrupted,
                "requeued sends interrupted before the message ended"
            );
        }
        // Read now: no account can be added before the client exists.
        let watched = match self.sync_in_background {
            Some(_) => store.read(|db| db.accounts().list())?,
            None => Vec::new(),
        };
        let blobs = BlobStore::open(self.config.blob_dir())?;
        let secrets = self
            .secrets
            .unwrap_or_else(|| Arc::new(KeyringSecrets::default()));
        let (events, _) = broadcast::channel(EVENT_CAPACITY);

        let mut engine = Engine::new(
            self.config,
            data_dir_lock,
            store,
            blobs,
            secrets,
            events,
            self.tls,
        );
        engine.sync_in_background = self.sync_in_background;
        engine.draft_upload_delay = self.draft_upload_delay;
        let client = MailClient {
            rt: Arc::new(rt),
            engine: Arc::new(engine),
        };
        if self.send_in_background {
            client.rt.spawn(send::worker(
                Arc::downgrade(&client.engine),
                client.engine.outbox_kick.clone(),
            ));
        }
        if let Some(every) = self.collect_garbage_every {
            client
                .rt
                .spawn(gc::run(Arc::downgrade(&client.engine), every));
        }
        if let Some(schedule) = self.sync_in_background {
            for account in watched {
                let engine = Arc::downgrade(&client.engine);
                client.rt.spawn(watch::run(engine, account.id, schedule));
            }
        }
        Ok(client)
    }
}

impl MailClient {
    pub fn builder(config: Config) -> MailClientBuilder {
        MailClientBuilder {
            config,
            runtime: None,
            secrets: None,
            tls: None,
            send_in_background: false,
            sync_in_background: None,
            collect_garbage_every: None,
            draft_upload_delay: DEFAULT_DRAFT_UPLOAD_DELAY,
        }
    }

    pub fn config(&self) -> &Config {
        &self.engine.config
    }

    /// Whether the engine gets through to the account's servers, as its latest attempts
    /// found (in the background, or asked for). Changes are announced with
    /// [`Event::AccountStatusChanged`].
    pub fn account_status(&self, id: AccountId) -> AccountStatus {
        self.engine.status(id)
    }

    /// Change notifications. Receivers that fall behind get `RecvError::Lagged` and
    /// should refresh whatever they display.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.engine.events.subscribe()
    }

    pub async fn list_accounts(&self) -> Result<Vec<Account>> {
        let engine = self.engine.clone();
        self.rt
            .run_blocking(move || Ok(engine.store.read(|db| db.accounts().list())?))
            .await
    }

    pub async fn get_account(&self, id: AccountId) -> Result<Option<Account>> {
        let engine = self.engine.clone();
        self.rt
            .run_blocking(move || Ok(engine.store.read(|db| db.accounts().get(id))?))
            .await
    }

    pub async fn find_account_by_email(&self, email: &str) -> Result<Option<Account>> {
        let engine = self.engine.clone();
        let email = email.to_owned();
        self.rt
            .run_blocking(move || {
                Ok(engine
                    .store
                    .read(|db| db.accounts().find_by_email(&email))?)
            })
            .await
    }

    /// Connects to the IMAP server and logs in with these settings, storing nothing, so a
    /// frontend can reject a wrong password or server before the account exists. Fails as
    /// a sync would: [`Error::AuthFailed`], [`Error::Unsupported`], or a network error.
    pub async fn check_login(&self, new: &NewAccount, password: &SecretString) -> Result<()> {
        validate_new_account(new)?;
        if password.expose_secret().is_empty() {
            return Err(Error::InvalidInput("password must not be empty".into()));
        }
        let engine = self.engine.clone();
        let imap = new.imap.clone();
        let credentials = auth::new_account_credentials(new, password);
        self.rt
            .run(async move {
                connection::login(&engine, &imap, &credentials)
                    .await?
                    .logout()
                    .await;
                Ok(())
            })
            .await
    }

    /// Stores the account and its password. Does not contact the server; the first
    /// [`sync_account`](Self::sync_account) does.
    pub async fn add_account(&self, new: NewAccount, password: SecretString) -> Result<Account> {
        validate_new_account(&new)?;
        if password.expose_secret().is_empty() {
            return Err(Error::InvalidInput("password must not be empty".into()));
        }
        let engine = self.engine.clone();
        self.rt
            .run_blocking(move || {
                // Secret first, row second: whoever can see the row can also see its
                // secret, so a concurrent removal always finds and deletes it. The random
                // key keeps data directories apart.
                let secret_key = format!("account:{}:{}", new.email, Uuid::new_v4().simple());
                engine.secrets.set(&secret_key, &password)?;
                let account = match engine
                    .store
                    .write(|db| db.accounts().insert(&new, &secret_key))
                {
                    Ok(account) => account,
                    Err(e) => {
                        if let Err(cleanup) = engine.secrets.delete(&secret_key) {
                            tracing::warn!(error = %cleanup,
                                "failed to delete password after account insert failed");
                        }
                        return Err(e.into());
                    }
                };
                engine.emit(Event::AccountAdded {
                    account: account.id,
                });
                watch::spawn(&engine, account.id);
                Ok(account)
            })
            .await
    }

    /// Removes the account, all local data cached for it, and its stored password.
    /// A sync of the account that is running is cancelled first.
    pub async fn remove_account(&self, id: AccountId) -> Result<()> {
        let engine = self.engine.clone();
        self.rt
            .run(async move {
                let _stopped = engine.start_removal(id).await;
                let result = blocking({
                    let engine = engine.clone();
                    move || {
                        let Some(secret_key) = engine.store.write(|db| db.accounts().delete(id))?
                        else {
                            return Err(Error::AccountNotFound(id.to_string()));
                        };
                        // The account is already gone; an orphaned keyring entry is only
                        // clutter.
                        if let Err(e) = engine.secrets.delete(&secret_key) {
                            tracing::warn!(account = %id, error = %e,
                                "failed to delete stored password");
                        }
                        engine.emit(Event::AccountRemoved { account: id });
                        Ok(())
                    }
                })
                .await;
                if let Err(e) = &result {
                    // Nothing was removed.
                    engine.cancel_removal(id);
                    // Its background sync stopped for the removal; the account stays.
                    if !matches!(e, Error::AccountNotFound(_)) {
                        watch::spawn(&engine, id);
                    }
                }
                result
            })
            .await
    }

    /// Brings the account's folders and message headers up to date with the server.
    /// Waits if the account is already syncing. Progress is committed as it goes, so
    /// a sync that fails (or whose future is dropped) leaves what it fetched, and the
    /// next one resumes. A folder the server refuses is reported in its
    /// [`FolderSyncReport`](crate::FolderSyncReport); losing the connection fails the
    /// whole call.
    pub async fn sync_account(&self, id: AccountId) -> Result<SyncReport> {
        self.rt
            .run(sync::sync_account(self.engine.clone(), id))
            .await
    }

    /// Brings one folder up to date with the server, as [`sync_account`](Self::sync_account)
    /// does for each (queued local changes go first), unless it completed a sync less than
    /// `max_age` ago, e.g. in the account's sync this call waited for. `None` if it didn't
    /// sync: the folder was fresh enough or can't hold messages. For a folder the user
    /// opens: background sync keeps only INBOX current between full syncs.
    pub async fn sync_folder(
        &self,
        folder: FolderId,
        max_age: Duration,
    ) -> Result<Option<FolderSyncReport>> {
        self.rt
            .run(sync::sync_one(self.engine.clone(), folder, max_age))
            .await
    }

    /// The account's folders, INBOX first.
    pub async fn list_folders(&self, account: AccountId) -> Result<Vec<Folder>> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { engine.read(move |db| db.folders().list(account)).await })
            .await
    }

    pub async fn get_folder(&self, id: FolderId) -> Result<Option<Folder>> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { engine.read(move |db| db.folders().get(id)).await })
            .await
    }

    /// By display name (`已发送`), server name (`&XfJT0ZAB-`), or `inbox` in any case.
    pub async fn find_folder(&self, account: AccountId, name: &str) -> Result<Option<Folder>> {
        let engine = self.engine.clone();
        let name = name.to_owned();
        self.rt
            .run(async move {
                engine
                    .read(move |db| db.folders().find_by_name(account, &name))
                    .await
            })
            .await
    }

    /// Messages in the folder, newest first.
    pub async fn list_messages(
        &self,
        folder: FolderId,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<MessageSummary>> {
        let engine = self.engine.clone();
        self.rt
            .run(async move {
                engine
                    .read(move |db| db.messages().list(folder, limit, offset))
                    .await
            })
            .await
    }

    /// The message as stored locally; `body` is `None` until downloaded.
    pub async fn get_message(&self, id: MessageId) -> Result<Option<Message>> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { engine.read(move |db| db.messages().get(id)).await })
            .await
    }

    /// The message with its body, downloading it first if needed (which leaves the
    /// message unread on the server). Fails with [`Error::OutOfSync`] if the server no
    /// longer has the message as it was synced.
    pub async fn download_message(&self, id: MessageId) -> Result<Message> {
        self.rt
            .run(body::download_message(self.engine.clone(), id))
            .await
    }

    /// The decoded content of an attachment or inline part, downloading it first if
    /// needed.
    pub async fn attachment_content(&self, id: AttachmentId) -> Result<Vec<u8>> {
        self.rt
            .run(body::attachment_content(self.engine.clone(), id))
            .await
    }

    /// The message exactly as the server stores it (the `.eml`), downloading it first if
    /// needed.
    pub async fn raw_message(&self, id: MessageId) -> Result<Vec<u8>> {
        self.rt
            .run(body::raw_message(self.engine.clone(), id))
            .await
    }

    /// Sets `add` and clears `remove` (`\Seen`, `\Flagged`, `\Answered`) on the
    /// messages. The change is made locally at once and reaches the server at the next
    /// sync or [`replay_pending`](Self::replay_pending), or right away with
    /// [`sync_in_background`](MailClientBuilder::sync_in_background); one the server
    /// refuses is undone. Fails, changing nothing, if a message doesn't exist. Returns
    /// the accounts whose messages changed.
    pub async fn set_flags(
        &self,
        messages: Vec<MessageId>,
        add: Flags,
        remove: Flags,
    ) -> Result<Vec<AccountId>> {
        self.rt
            .run(ops::set_flags(self.engine.clone(), messages, add, remove))
            .await
    }

    /// Moves the messages to `to`, a folder of their account. They show there at once;
    /// the server follows as with [`set_flags`](Self::set_flags).
    pub async fn move_messages(
        &self,
        messages: Vec<MessageId>,
        to: FolderId,
    ) -> Result<Vec<AccountId>> {
        self.rt
            .run(ops::move_messages(self.engine.clone(), messages, to))
            .await
    }

    /// Moves the messages to their account's Trash, or deletes them for good if they
    /// are in Trash already or `permanently` is set. An account without a Trash folder
    /// only allows the latter. The server follows as with [`set_flags`](Self::set_flags).
    pub async fn delete_messages(
        &self,
        messages: Vec<MessageId>,
        permanently: bool,
    ) -> Result<Vec<AccountId>> {
        self.rt
            .run(ops::delete_messages(
                self.engine.clone(),
                messages,
                permanently,
            ))
            .await
    }

    /// Makes the account's queued changes on the server now, without syncing, and saves
    /// changed drafts to its Drafts folder. Waits for a sync of the account to finish
    /// first (it does the same). Losing the connection fails the call and leaves the rest
    /// queued.
    pub async fn replay_pending(&self, account: AccountId) -> Result<ReplayReport> {
        self.rt
            .run(ops::replay_account(self.engine.clone(), account))
            .await
    }

    /// Changes not yet made on the server, and those it refused or that no longer
    /// apply, oldest first; all accounts' with `None`.
    pub async fn list_pending(&self, account: Option<AccountId>) -> Result<Vec<PendingOp>> {
        self.rt.run(ops::list(self.engine.clone(), account)).await
    }

    /// Forgets a failed or conflicting change. Pending ones can't be dismissed.
    pub async fn dismiss_pending(&self, id: PendingOpId) -> Result<()> {
        self.rt.run(ops::dismiss(self.engine.clone(), id)).await
    }

    /// Searches subjects, senders and recipients of all synced messages, and the text
    /// of downloaded bodies. Newest first.
    pub async fn search(&self, query: SearchQuery) -> Result<Vec<MessageSummary>> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { engine.read(move |db| db.messages().search(&query)).await })
            .await
    }

    /// A new draft. Addresses are checked as they will be needed for sending.
    ///
    /// Drafts are copied to the account's Drafts folder, the copy replaced after every
    /// change, at the next sync or [`replay_pending`](Self::replay_pending), or right
    /// away with [`sync_in_background`](MailClientBuilder::sync_in_background).
    pub async fn create_draft(&self, account: AccountId, content: DraftContent) -> Result<Draft> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { drafts::create(&engine, account, content).await })
            .await
    }

    /// A new draft answering the message, to its sender (or Reply-To) and, with `all`,
    /// to everyone else it went to, quoting its text and threaded to it. Downloads the
    /// body first if needed.
    pub async fn reply(&self, message: MessageId, all: bool) -> Result<Draft> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { drafts::reply(&engine, message, all).await })
            .await
    }

    /// A new draft forwarding the message, to no one yet: `Fwd:` and its subject, its
    /// headers and text below an empty start, and copies of its attachments (not the
    /// images shown within it). Downloads the body and attachments first if needed;
    /// fails, creating nothing, if one can't be had.
    pub async fn forward(&self, message: MessageId) -> Result<Draft> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { drafts::forward(&engine, message).await })
            .await
    }

    /// Replaces what the draft says. Fails with [`Error::Conflict`] while it is in the
    /// outbox.
    pub async fn update_draft(&self, id: DraftId, content: DraftContent) -> Result<Draft> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { drafts::update(&engine, id, content).await })
            .await
    }

    pub async fn get_draft(&self, id: DraftId) -> Result<Option<Draft>> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { engine.read(move |db| db.drafts().get(id)).await })
            .await
    }

    /// The account's drafts, most recently changed first. Drafts being sent are not
    /// among them; see [`list_outbox`](Self::list_outbox).
    pub async fn list_drafts(&self, account: AccountId) -> Result<Vec<Draft>> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { engine.read(move |db| db.drafts().list(account)).await })
            .await
    }

    /// How many drafts [`list_drafts`](Self::list_drafts) returns.
    pub async fn count_drafts(&self, account: AccountId) -> Result<usize> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { engine.read(move |db| db.drafts().count(account)).await })
            .await
    }

    /// Deletes the draft, and its copy on the server the way changes to messages are
    /// made.
    pub async fn delete_draft(&self, id: DraftId) -> Result<()> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { drafts::delete(&engine, id).await })
            .await
    }

    /// Attaches a copy of the file: changing or deleting the file afterwards doesn't
    /// affect the draft.
    pub async fn attach_file(
        &self,
        draft: DraftId,
        path: impl Into<PathBuf>,
    ) -> Result<DraftAttachment> {
        let engine = self.engine.clone();
        let path = path.into();
        self.rt
            .run(async move { drafts::attach_file(&engine, draft, path).await })
            .await
    }

    /// Returns the draft as it is now.
    pub async fn remove_draft_attachment(&self, id: DraftAttachmentId) -> Result<Draft> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { drafts::remove_attachment(&engine, id).await })
            .await
    }

    /// Hands the draft to the outbox. The message is built now, exactly as it will be
    /// sent; the draft stays locked until it is sent (then it is deleted) or the send is
    /// cancelled. Sending happens in the background if the client was built with
    /// [`MailClientBuilder::send_in_background`], else on
    /// [`flush_outbox`](Self::flush_outbox).
    pub async fn send_draft(&self, id: DraftId) -> Result<OutboxEntry> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { send::queue_draft(&engine, id).await })
            .await
    }

    /// Every entry still in the outbox (sent ones are gone), oldest first; all
    /// accounts' with `None`.
    pub async fn list_outbox(&self, account: Option<AccountId>) -> Result<Vec<OutboxEntry>> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { engine.read(move |db| db.outbox().list(account)).await })
            .await
    }

    /// Sends everything queued and saves what was sent to Sent, now, without waiting
    /// for backoff. Waits for background sending of the account to pause first. Returns
    /// the entries worked on, as they ended up; failures are recorded in them, not
    /// returned.
    pub async fn flush_outbox(&self, account: AccountId) -> Result<Vec<OutboxEntry>> {
        self.rt
            .run(send::flush(self.engine.clone(), account, false))
            .await
    }

    /// Tries a failed or unknown send again (an unknown one may then arrive twice), or
    /// saving an accepted one to Sent. Due at once.
    pub async fn retry_outbox(&self, id: OutboxId) -> Result<OutboxEntry> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { send::retry(&engine, id).await })
            .await
    }

    /// For a send with an unknown outcome that the user found was delivered: it is only
    /// saved to Sent from now on.
    pub async fn mark_outbox_sent(&self, id: OutboxId) -> Result<OutboxEntry> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { send::mark_sent(&engine, id).await })
            .await
    }

    /// Removes the entry. If the message never went out (queued, failed, unknown), its
    /// draft can be edited again; if it was sent, this only gives up saving it to Sent.
    /// A send of this entry in progress is waited for, and the decision applies to where
    /// it left the entry; one still waiting its turn goes at once. Returns the entry as
    /// it was.
    pub async fn cancel_outbox(&self, id: OutboxId) -> Result<OutboxEntry> {
        let engine = self.engine.clone();
        self.rt
            .run(async move { send::cancel(&engine, id).await })
            .await
    }

    /// Deletes stored bodies and attachments nothing refers to any more (after messages
    /// or accounts were removed). Safe to run at any time.
    pub async fn collect_garbage(&self) -> Result<GcReport> {
        let engine = self.engine.clone();
        self.rt.run_blocking(move || gc::collect(&engine)).await
    }

    /// Writes `contents` to a new file in [`Config::handoff_dir`], readable by this user
    /// only, for another program (a browser) to open, and returns its path. The file is
    /// left there: the other program reads it after this returns. It goes when the engine
    /// next opens, or when a later hand-off finds it older than ten minutes.
    pub async fn hand_off(&self, extension: &str, contents: Vec<u8>) -> Result<PathBuf> {
        let dir = self.config().handoff_dir();
        let suffix = format!(".{extension}");
        self.rt
            .run_blocking(move || {
                remove_old_files(&dir, KEEP_HANDED_OFF);
                let mut file = tempfile::Builder::new()
                    .prefix("catmail-")
                    .suffix(&suffix)
                    .tempfile_in(&dir)?;
                file.write_all(&contents)?;
                let (_, path) = file.keep().map_err(|e| e.error)?;
                Ok(path)
            })
            .await
    }
}

/// How long a handed-off file is kept within a session. The other program reads it as
/// soon as it opens it; after that it is only a copy of a message lying on disk, and a
/// GUI left open for days would otherwise keep days of them.
const KEEP_HANDED_OFF: Duration = Duration::from_secs(10 * 60);

fn lock_data_dir(config: &Config) -> Result<File> {
    create_data_dir(config.data_dir())?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(config.lock_path())?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => {
            Err(Error::AlreadyRunning(config.data_dir().to_path_buf()))
        }
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Creates the data directory readable by the user alone: the database holds every
/// message's headers and text, and the drafts. Parent directories are made as usual. An
/// existing directory is left as it is (it may be one the user chose, such as their
/// home; the files the engine makes in it are private anyway), but it must belong to
/// this user.
fn create_data_dir(dir: &Path) -> Result<()> {
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    // Windows: the new directory inherits the user profile's ACL.
    catmail_store::create_private_dir(dir)?;
    check_data_dir_owner(dir)
}

/// Its owner could swap the files in it, and a symbolic link someone else planted where
/// the directory was expected (in a shared location) points to one they own. A link to
/// a directory of the user's own is fine. Windows: the directory's ACL is the user's to
/// set.
fn check_data_dir_owner(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(dir)?;
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(Error::DataDirNotOwned(dir.to_path_buf()));
        }
        let mode = metadata.mode() & 0o777;
        if mode & 0o077 != 0 {
            tracing::warn!(
                dir = %dir.display(),
                mode = format!("{mode:o}"),
                "other users can list the data directory; the files in it are private"
            );
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Files handed to other programs in earlier sessions hold whole messages. The lock just
/// taken means no frontend is writing them any more. Cleared at start rather than on
/// close: another program may still be reading them then, and destructors don't always
/// run (macOS terminates the app). Failing only costs disk space.
fn clear_handoff_dir(dir: &Path) {
    match fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(error = %e, dir = %dir.display(), "couldn't clear handed-off files")
        }
    }
}

/// Deletes the files in `dir` last written more than `age` ago. Failing only costs disk
/// space (Windows refuses while another program still has the file open).
fn remove_old_files(dir: &Path, age: Duration) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry.metadata().is_ok_and(|meta| {
            meta.is_file()
                && meta
                    .modified()
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|elapsed| elapsed > age)
        });
        if old && let Err(e) = fs::remove_file(entry.path()) {
            tracing::debug!(error = %e, path = %entry.path().display(),
                "couldn't remove handed-off file");
        }
    }
}

fn validate_new_account(new: &NewAccount) -> Result<()> {
    let invalid = |msg: &str| Err(Error::InvalidInput(msg.into()));
    let Some((local, domain)) = new.email.split_once('@') else {
        return invalid("email must contain `@`");
    };
    if local.is_empty() || domain.is_empty() {
        return invalid("email must have a local part and a domain");
    }
    if new.username.is_empty() {
        return invalid("username must not be empty");
    }
    for (proto, server) in [("IMAP", &new.imap), ("SMTP", &new.smtp)] {
        if server.host.is_empty() {
            return Err(Error::InvalidInput(format!(
                "{proto} host must not be empty"
            )));
        }
        if server.port == 0 {
            return Err(Error::InvalidInput(format!("{proto} port must not be 0")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use catmail_types::{AuthKind, Security, ServerConfig};

    use std::sync::Mutex;

    use super::*;
    use crate::MemorySecrets;

    fn secret_key(client: &MailClient, id: AccountId) -> String {
        client
            .engine
            .store
            .read(|db| db.accounts().secret_key(id))
            .unwrap()
            .unwrap()
    }

    fn new_account(email: &str) -> NewAccount {
        NewAccount {
            email: email.into(),
            display_name: None,
            username: email.into(),
            imap: ServerConfig {
                host: "imap.example.com".into(),
                port: 993,
                security: Security::Tls,
            },
            smtp: ServerConfig {
                host: "smtp.example.com".into(),
                port: 465,
                security: Security::Tls,
            },
            auth: AuthKind::Password,
        }
    }

    /// Blocks inside `set` until the test releases it, to widen race windows.
    struct GatedSecrets {
        inner: MemorySecrets,
        entered: Arc<tokio::sync::Notify>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl GatedSecrets {
        fn new() -> (Arc<Self>, std::sync::mpsc::Sender<()>) {
            let (tx, rx) = std::sync::mpsc::channel();
            let secrets = Arc::new(Self {
                inner: MemorySecrets::default(),
                entered: Arc::default(),
                release: Mutex::new(rx),
            });
            (secrets, tx)
        }
    }

    impl SecretStore for GatedSecrets {
        fn set(&self, key: &str, secret: &SecretString) -> Result<()> {
            self.entered.notify_one();
            self.release.lock().unwrap().recv().unwrap();
            self.inner.set(key, secret)
        }
        fn get(&self, key: &str) -> Result<Option<SecretString>> {
            self.inner.get(key)
        }
        fn delete(&self, key: &str) -> Result<()> {
            self.inner.delete(key)
        }
    }

    fn stored(secrets: &MemorySecrets, key: &str) -> Option<String> {
        let secret = secrets.get(key).unwrap()?;
        Some(secret.expose_secret().to_owned())
    }

    fn open_client(dir: &std::path::Path, secrets: Arc<dyn SecretStore>) -> MailClient {
        MailClient::builder(Config::with_data_dir(dir))
            .runtime(Handle::current())
            .secrets(secrets)
            .open()
            .unwrap()
    }

    struct FailingSecrets;

    impl SecretStore for FailingSecrets {
        fn set(&self, _: &str, _: &SecretString) -> Result<()> {
            Err(Error::Secrets("locked".into()))
        }
        fn get(&self, _: &str) -> Result<Option<SecretString>> {
            Ok(None)
        }
        fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn account_lifecycle_on_shared_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let client = MailClient::builder(Config::with_data_dir(dir.path()))
            .runtime(Handle::current())
            .secrets(secrets.clone())
            .open()
            .unwrap();
        let mut events = client.subscribe();

        let account = client
            .add_account(new_account("a@example.com"), "hunter2".into())
            .await
            .unwrap();
        assert_eq!(
            events.recv().await.unwrap(),
            Event::AccountAdded {
                account: account.id
            }
        );
        let key = secret_key(&client, account.id);
        assert_eq!(stored(&secrets, &key).as_deref(), Some("hunter2"));
        assert_eq!(client.list_accounts().await.unwrap(), vec![account.clone()]);

        client.remove_account(account.id).await.unwrap();
        assert_eq!(
            events.recv().await.unwrap(),
            Event::AccountRemoved {
                account: account.id
            }
        );
        assert!(secrets.get(&key).unwrap().is_none());
        assert!(matches!(
            client.remove_account(account.id).await,
            Err(Error::AccountNotFound(_))
        ));
    }

    #[test]
    fn owned_runtime_is_usable_from_a_foreign_executor() {
        // Mirrors the GUI: futures are polled by a non-tokio executor.
        let dir = tempfile::tempdir().unwrap();
        let client = MailClient::builder(Config::with_data_dir(dir.path()))
            .secrets(Arc::new(MemorySecrets::default()))
            .open()
            .unwrap();

        let accounts = pollster::block_on(async {
            client
                .add_account(new_account("a@example.com"), "pw".into())
                .await?;
            client.list_accounts().await
        })
        .unwrap();
        assert_eq!(accounts.len(), 1);
        assert!(
            client.config().db_path().exists(),
            "database is created on disk"
        );
    }

    #[test]
    fn secret_store_failure_rolls_back_account() {
        let dir = tempfile::tempdir().unwrap();
        let client = MailClient::builder(Config::with_data_dir(dir.path()))
            .secrets(Arc::new(FailingSecrets))
            .open()
            .unwrap();

        let result =
            pollster::block_on(client.add_account(new_account("a@example.com"), "pw".into()));
        assert!(matches!(result, Err(Error::Secrets(_))));
        assert!(
            pollster::block_on(client.list_accounts())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let client = MailClient::builder(Config::with_data_dir(dir.path()))
            .secrets(Arc::new(MemorySecrets::default()))
            .open()
            .unwrap();

        let mut bad = new_account("no-at-sign");
        let r = pollster::block_on(client.add_account(bad.clone(), "pw".into()));
        assert!(matches!(r, Err(Error::InvalidInput(_))));

        bad.email = "a@example.com".into();
        bad.imap.port = 0;
        let r = pollster::block_on(client.add_account(bad, "pw".into()));
        assert!(matches!(r, Err(Error::InvalidInput(_))));

        let r = pollster::block_on(client.add_account(new_account("a@example.com"), "".into()));
        assert!(matches!(r, Err(Error::InvalidInput(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn data_dirs_do_not_share_credentials() {
        let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let secrets = Arc::new(MemorySecrets::default());
        let a = open_client(dir_a.path(), secrets.clone());
        let b = open_client(dir_b.path(), secrets.clone());

        let acc_a = a
            .add_account(new_account("a@example.com"), "pw-a".into())
            .await
            .unwrap();
        let acc_b = b
            .add_account(new_account("b@example.com"), "pw-b".into())
            .await
            .unwrap();
        assert_eq!(acc_a.id, acc_b.id, "both databases start at the same id");

        b.remove_account(acc_b.id).await.unwrap();
        assert_eq!(
            stored(&secrets, &secret_key(&a, acc_a.id)).as_deref(),
            Some("pw-a")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remove_during_add_leaves_no_orphaned_secret() {
        let dir = tempfile::tempdir().unwrap();
        let (secrets, release) = GatedSecrets::new();
        let client = open_client(dir.path(), secrets.clone());

        let add = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .add_account(new_account("a@example.com"), "pw".into())
                    .await
            }
        });
        // The password write is in flight; the row must not be visible yet.
        secrets.entered.notified().await;
        let id = AccountId(1);
        assert!(matches!(
            client.remove_account(id).await,
            Err(Error::AccountNotFound(_))
        ));
        release.send(()).unwrap();
        assert_eq!(add.await.unwrap().unwrap().id, id);
        assert!(
            !client.engine.cancellation(id).is_cancelled(),
            "the failed removal must not leave the new account marked removed"
        );

        client.remove_account(id).await.unwrap();
        assert_eq!(secrets.inner.len(), 0, "no orphaned credential");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_engine_per_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let first = open_client(dir.path(), secrets.clone());

        let second = MailClient::builder(Config::with_data_dir(dir.path()))
            .runtime(Handle::current())
            .secrets(secrets.clone())
            .open();
        assert!(matches!(second, Err(Error::AlreadyRunning(_))));

        // A clone shares the engine; dropping every handle releases the lock.
        let clone = first.clone();
        drop(first);
        drop(clone);
        open_client(dir.path(), secrets);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handed_off_files_are_removed_once_old() {
        let dir = tempfile::tempdir().unwrap();
        let client = open_client(dir.path(), Arc::new(MemorySecrets::default()));
        let old = client
            .hand_off("html", b"<p>old</p>".to_vec())
            .await
            .unwrap();
        let recent = client
            .hand_off("html", b"<p>recent</p>".to_vec())
            .await
            .unwrap();
        assert!(old.starts_with(client.config().handoff_dir()));
        assert_eq!(old.extension().unwrap(), "html");
        assert_ne!(old, recent);
        let written = std::time::SystemTime::now() - KEEP_HANDED_OFF - Duration::from_secs(60);
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(written)
            .unwrap();

        let new = client
            .hand_off("html", b"<p>new</p>".to_vec())
            .await
            .unwrap();
        assert!(!old.exists());
        assert_eq!(fs::read_to_string(&recent).unwrap(), "<p>recent</p>");
        assert_eq!(fs::read_to_string(&new).unwrap(), "<p>new</p>");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handed_off_files_last_until_the_next_engine_opens() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let first = open_client(dir.path(), secrets.clone());
        let handoff = first.config().handoff_dir();
        let page = handoff.join("message.html");
        fs::write(&page, "<p>hi</p>").unwrap();

        // An engine that can't take the lock leaves them alone.
        let second = MailClient::builder(Config::with_data_dir(dir.path()))
            .runtime(Handle::current())
            .secrets(secrets.clone())
            .open();
        assert!(matches!(second, Err(Error::AlreadyRunning(_))));
        assert!(page.exists());

        drop(first);
        let reopened = open_client(dir.path(), secrets);
        assert!(!page.exists());
        let handoff = reopened.config().handoff_dir();
        assert_eq!(fs::read_dir(handoff).unwrap().count(), 0);
    }

    /// A client on `dir` with one account, `email`.
    async fn client_with_account(
        dir: &std::path::Path,
        secrets: Arc<dyn SecretStore>,
        email: &str,
    ) -> (MailClient, AccountId) {
        let client = open_client(dir, secrets);
        let account = client
            .add_account(new_account(email), "pw".into())
            .await
            .unwrap();
        (client, account.id)
    }

    /// A draft to `b@example.com`.
    async fn draft_to_b(client: &MailClient, account: AccountId) -> catmail_types::Draft {
        let content = catmail_types::DraftContent {
            to: vec![catmail_types::Address {
                name: None,
                email: "b@example.com".into(),
            }],
            ..Default::default()
        };
        client.create_draft(account, content).await.unwrap()
    }

    async fn queue_one(client: &MailClient, account: AccountId) -> catmail_types::OutboxEntry {
        let draft = draft_to_b(client, account).await;
        client.send_draft(draft.id).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sends_interrupted_before_the_end_go_back_to_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let (client, account) =
            client_with_account(dir.path(), secrets.clone(), "a@example.com").await;
        let mut entries = Vec::new();
        for _ in 0..2 {
            entries.push(queue_one(&client, account).await.id);
        }
        // The process dies sending both: the first before the end of the message, the
        // second after.
        client
            .engine
            .store
            .write(|db| {
                db.outbox().begin_send(entries[0], 1)?;
                db.outbox().begin_send(entries[1], 1)?;
                db.outbox().mark_unknown(entries[1], None, 1)
            })
            .unwrap();
        drop(client);

        let client = open_client(dir.path(), secrets);
        let statuses: Vec<_> = client
            .list_outbox(None)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.status)
            .collect();
        assert_eq!(
            statuses,
            [
                catmail_types::OutboxStatus::Queued,
                catmail_types::OutboxStatus::Unknown
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sends_left_unfinished_are_requeued_without_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let (client, account) = client_with_account(dir.path(), secrets, "a@example.com").await;
        let entry = queue_one(&client, account).await;
        // An attempt that couldn't record how it ended, e.g. a failed write.
        client
            .engine
            .store
            .write(|db| db.outbox().begin_send(entry.id, 1))
            .unwrap();

        let cancelled = client.cancel_outbox(entry.id).await.unwrap();
        assert_eq!(cancelled.status, catmail_types::OutboxStatus::Queued);
        assert!(client.list_outbox(None).await.unwrap().is_empty());
    }

    /// Like a Secret Service whose collection is locked: works once unlocked.
    #[derive(Default)]
    struct LockableSecrets {
        inner: MemorySecrets,
        locked: std::sync::atomic::AtomicBool,
    }

    impl SecretStore for LockableSecrets {
        fn set(&self, key: &str, secret: &SecretString) -> Result<()> {
            self.inner.set(key, secret)
        }
        fn get(&self, key: &str) -> Result<Option<SecretString>> {
            if self.locked.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(Error::Secrets("collection is locked".into()));
            }
            self.inner.get(key)
        }
        fn delete(&self, key: &str) -> Result<()> {
            self.inner.delete(key)
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn locked_keyring_postpones_sending() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(LockableSecrets::default());
        let (client, account) =
            client_with_account(dir.path(), secrets.clone(), "a@example.com").await;
        queue_one(&client, account).await;
        secrets
            .locked
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // Fails before any connection is made, so the fake host doesn't matter.
        let flushed = client.flush_outbox(account).await.unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].status, catmail_types::OutboxStatus::Queued);
        assert!(flushed[0].last_error.as_deref().unwrap().contains("locked"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn account_address_is_checked_before_queueing() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let (client, account) = client_with_account(dir.path(), secrets, "张三@example.com").await;
        let draft = draft_to_b(&client, account).await;
        let queued = client.send_draft(draft.id).await;
        assert!(matches!(queued, Err(Error::InvalidInput(_))), "{queued:?}");
        assert!(client.list_outbox(None).await.unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn data_dir_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("new").join("catmail");
        open_client(&data, Arc::new(MemorySecrets::default()));
        let mode = fs::metadata(&data).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn files_in_a_shared_data_dir_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("shared");
        fs::create_dir(&data).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();
        let client = open_client(&data, Arc::new(MemorySecrets::default()));
        let config = client.config();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&data), 0o755, "the user's directory is left as it is");
        assert_eq!(mode(&config.db_path()), 0o600);
        assert_eq!(mode(&config.handoff_dir()), 0o700);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn data_dir_of_another_user_is_refused() {
        if rustix::process::geteuid().is_root() {
            return; // Root owns the directory below.
        }
        // As if someone planted a link to a directory of theirs in a shared location.
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("catmail");
        std::os::unix::fs::symlink("/", &data).unwrap();
        let opened = MailClient::builder(Config::with_data_dir(&data))
            .runtime(Handle::current())
            .secrets(Arc::new(MemorySecrets::default()))
            .open();
        assert!(matches!(opened, Err(Error::DataDirNotOwned(_))));

        // A link to a directory of the user's own is fine.
        let own = dir.path().join("own");
        fs::create_dir(&own).unwrap();
        fs::remove_file(&data).unwrap();
        std::os::unix::fs::symlink(&own, &data).unwrap();
        open_client(&data, Arc::new(MemorySecrets::default()));
        assert!(own.join("catmail.db").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_insert_deletes_the_new_secret() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let client = open_client(dir.path(), secrets.clone());

        client
            .add_account(new_account("a@example.com"), "pw".into())
            .await
            .unwrap();
        let dup = client
            .add_account(new_account("A@example.com"), "pw".into())
            .await;
        assert!(
            matches!(
                dup,
                Err(Error::Store(catmail_store::Error::AlreadyExists(_)))
            ),
            "{dup:?}"
        );
        assert_eq!(secrets.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_caller_future_still_emits_the_event() {
        let dir = tempfile::tempdir().unwrap();
        let (secrets, release) = GatedSecrets::new();
        let client = open_client(dir.path(), secrets.clone());
        let mut events = client.subscribe();

        tokio::select! {
            _ = client.add_account(new_account("a@example.com"), "pw".into()) => {
                unreachable!("set() is gated")
            }
            _ = secrets.entered.notified() => {} // drops the add_account future
        }
        release.send(()).unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("event was not emitted")
            .unwrap();
        assert_eq!(
            event,
            Event::AccountAdded {
                account: AccountId(1)
            }
        );
    }
}
