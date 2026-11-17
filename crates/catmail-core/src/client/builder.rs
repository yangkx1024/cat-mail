//! Opening the engine: [`MailClientBuilder`], and the background work started once it is
//! open. The files handed off to other programs are looked after here too, as opening
//! clears them.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use catmail_proto::TlsConfig;
use catmail_store::{BlobStore, Store};
use catmail_types::Account;
use tokio::runtime::Handle;
use tokio::sync::broadcast;

use super::{EVENT_CAPACITY, MailClient, Shared};
use crate::engine::{DEFAULT_DRAFT_UPLOAD_DELAY, Engine, EngineTuning, now};
use crate::runtime::Runtime;
use crate::secrets::{KeyringSecrets, SecretStore};
use crate::watch::{self, SyncSchedule};
use crate::{BodyPrefetch, Config, DataDirLock, Result, gc, prefetch, rebuild, send, sleep};

pub struct MailClientBuilder {
    config: Config,
    /// Taken before the builder, by [`MailClient::builder_locked`].
    lock: Option<DataDirLock>,
    runtime: Option<Handle>,
    secrets: Option<Arc<dyn SecretStore>>,
    tls: Option<TlsConfig>,
    send_in_background: bool,
    sync_in_background: Option<SyncSchedule>,
    prefetch_bodies: Option<BodyPrefetch>,
    collect_garbage_every: Option<Duration>,
    draft_upload_delay: Duration,
    tuning: EngineTuning,
}

/// What [`MailClientBuilder::open_engine`] opened, for [`MailClient::start`]. Background
/// sync is the engine's to know: it starts for accounts added later too.
struct Opened {
    engine: Engine,
    /// The accounts to keep in sync, and fetch bodies of, in the background.
    watched: Vec<Account>,
    config: Config,
    send_in_background: bool,
    collect_garbage_every: Option<Duration>,
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

    /// Fetch bodies in the background for as long as the client exists, so that search
    /// finds messages never opened by their text too [default: off]: the newest first,
    /// each account on a connection of its own, starting once its folders were synced,
    /// within a daily budget. For long-running frontends. The [`BodyPrefetch`] says how,
    /// `BodyPrefetch::default()` for the usual.
    pub fn prefetch_bodies(mut self, prefetch: Option<BodyPrefetch>) -> Self {
        self.prefetch_bodies = prefetch;
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

    /// Everything a frontend that stays up does in the background: syncs, prefetches
    /// bodies, sends and collects garbage, as usual. The app's, and `catmail serve`'s:
    /// one engine on a data directory behaves the same whichever of them holds it.
    pub fn resident(self) -> Self {
        self.sync_in_background(Some(SyncSchedule::default()))
            .prefetch_bodies(Some(BodyPrefetch::default()))
            .send_in_background(true)
            .collect_garbage_every(Some(crate::DEFAULT_GC_INTERVAL))
    }

    /// Batch sizes, the whole-message limit and the keepalive of idle connections
    /// [default: `EngineTuning::default()`].
    /// Batch sizes below 1 count as 1.
    pub fn tuning(mut self, tuning: EngineTuning) -> Self {
        self.tuning = tuning.checked();
        self
    }

    /// Locks the data directory, empties [`Config::handoff_dir`], then opens the database
    /// (migrating it if needed) and the blob store. Blocks the calling thread meanwhile,
    /// which a migration can make take a while: a frontend with a UI thread uses
    /// [`open_async`](Self::open_async). Fails with [`Error::AlreadyRunning`] if another
    /// engine, in this or another process, has the directory open, and with
    /// [`Error::DataDirNotOwned`] if it belongs to another user. Sends a previous engine
    /// left unfinished before the end of the message go back to the queue.
    ///
    /// [`Error::AlreadyRunning`]: crate::Error::AlreadyRunning
    /// [`Error::DataDirNotOwned`]: crate::Error::DataDirNotOwned
    pub fn open(mut self) -> Result<MailClient> {
        let rt = self.take_runtime()?;
        let opened = self.open_engine()?;
        Ok(MailClient::start(rt, opened))
    }

    /// [`open`](Self::open), on the engine runtime's blocking pool. Like the client's
    /// methods, the future can be polled by any executor.
    pub async fn open_async(mut self) -> Result<MailClient> {
        let rt = self.take_runtime()?;
        let opened = rt.run_blocking(move || self.open_engine()).await?;
        Ok(MailClient::start(rt, opened))
    }

    fn take_runtime(&mut self) -> Result<Runtime> {
        match self.runtime.take() {
            Some(handle) => Ok(Runtime::shared(handle)),
            None => Runtime::owned(),
        }
    }

    /// Blocking.
    fn open_engine(mut self) -> Result<Opened> {
        let data_dir_lock = match self.lock.take() {
            Some(lock) => lock,
            None => self.config.lock()?,
        };
        let handoff_dir = self.config.handoff_dir();
        clear_handoff_dir(&handoff_dir);
        catmail_store::create_private_dir(&handoff_dir)?;
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
        let watched = if self.sync_in_background.is_some() || self.prefetch_bodies.is_some() {
            store.read(|db| db.accounts().list())?
        } else {
            Vec::new()
        };
        let blobs = BlobStore::open(self.config.blob_dir())?;
        let secrets = self
            .secrets
            .unwrap_or_else(|| Arc::new(KeyringSecrets::default()));
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        tracing::info!(
            background_sync = self.sync_in_background.is_some(),
            prefetch_bodies = self.prefetch_bodies.is_some(),
            accounts = watched.len(),
            "engine opened"
        );

        let mut engine = Engine::new(
            &self.config,
            data_dir_lock,
            store,
            blobs,
            secrets,
            events,
            self.tls,
        );
        engine.sync_in_background = self.sync_in_background;
        engine.prefetch = self.prefetch_bodies;
        engine.draft_upload_delay = self.draft_upload_delay;
        engine.tuning = self.tuning;
        Ok(Opened {
            engine,
            watched,
            config: self.config,
            send_in_background: self.send_in_background,
            collect_garbage_every: self.collect_garbage_every,
        })
    }
}

impl MailClient {
    pub fn builder(config: Config) -> MailClientBuilder {
        MailClientBuilder {
            config,
            lock: None,
            runtime: None,
            secrets: None,
            tls: None,
            send_in_background: false,
            sync_in_background: None,
            prefetch_bodies: None,
            collect_garbage_every: None,
            draft_upload_delay: DEFAULT_DRAFT_UPLOAD_DELAY,
            tuning: EngineTuning::default(),
        }
    }

    /// [`builder`](Self::builder) for a data directory already locked, e.g. by a frontend
    /// that has to know it is the one for it before the engine is opened.
    pub fn builder_locked(lock: DataDirLock) -> MailClientBuilder {
        let mut builder = Self::builder(lock.config().clone());
        builder.lock = Some(lock);
        builder
    }

    /// Starts the background work asked for on the engine just opened.
    fn start(rt: Runtime, opened: Opened) -> Self {
        let engine = Arc::new(opened.engine);
        if opened.send_in_background {
            rt.spawn(send::worker(
                Arc::downgrade(&engine),
                engine.outbox_kick.clone(),
                engine.closing(),
            ));
        }
        rt.spawn(sleep::run(
            Arc::downgrade(&engine),
            sleep::CHECK_EVERY,
            sleep::Clocks::now,
            engine.closing(),
        ));
        if let Some(every) = opened.collect_garbage_every {
            rt.spawn(gc::run(Arc::downgrade(&engine), every, engine.closing()));
        }
        rt.spawn(rebuild::run(
            Arc::downgrade(&engine),
            engine.tuning.rebuild_delay,
            engine.closing(),
        ));
        // As `accounts::spawn_background`, from outside the runtime.
        for account in opened.watched {
            if let Some(schedule) = engine.sync_in_background {
                let task = watch::task(&engine, account.id, schedule);
                rt.spawn(task);
            }
            if let Some(settings) = engine.prefetch {
                let task = prefetch::task(&engine, account.id, settings);
                rt.spawn(task);
            }
        }
        MailClient {
            shared: Arc::new(Shared {
                rt,
                closing: engine.closing(),
                config: opened.config,
                engine: Mutex::new(Some(engine)),
            }),
        }
    }
}

/// How long a handed-off file is kept within a session. The other program reads it as
/// soon as it opens it; after that it is only a copy of a message lying on disk, and a
/// GUI left open for days would otherwise keep days of them.
pub(super) const KEEP_HANDED_OFF: Duration = Duration::from_secs(10 * 60);

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
pub(super) fn remove_old_files(dir: &Path, age: Duration) {
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

#[cfg(test)]
mod tests {
    use super::super::fixtures::{new_account, open_client};
    use super::*;
    use crate::{Error, MemorySecrets, Verify};

    #[tokio::test(flavor = "multi_thread")]
    async fn background_tasks_end_with_the_client_on_a_shared_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let metrics = Handle::current().metrics();
        let before = metrics.num_alive_tasks();
        let client = MailClient::builder(Config::with_data_dir(dir.path()))
            .runtime(Handle::current())
            .secrets(Arc::new(MemorySecrets::default()))
            .send_in_background(true)
            .collect_garbage_every(Some(crate::DEFAULT_GC_INTERVAL))
            .open()
            .unwrap();
        assert_eq!(metrics.num_alive_tasks(), before + 4);
        // Waiting: the outbox is empty, the first collection and the rebuild of stale
        // bodies are minutes away, and the clocks are compared every half minute.
        tokio::time::sleep(Duration::from_millis(100)).await;

        drop(client);
        let ended = async {
            while metrics.num_alive_tasks() > before {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), ended)
            .await
            .expect("the outbox worker, GC and sleep detection end once the client is gone");
    }

    #[test]
    fn open_async_from_a_foreign_executor() {
        let dir = tempfile::tempdir().unwrap();
        let builder = || {
            MailClient::builder(Config::with_data_dir(dir.path()))
                .secrets(Arc::new(MemorySecrets::default()))
        };
        let client = pollster::block_on(builder().open_async()).unwrap();
        pollster::block_on(client.add_account(
            new_account("a@example.com"),
            "pw".into(),
            Verify::Skip,
        ))
        .unwrap();
        assert!(matches!(
            pollster::block_on(builder().open_async()),
            Err(Error::AlreadyRunning(_))
        ));
        drop(client);
        let client = pollster::block_on(builder().open_async()).unwrap();
        assert_eq!(pollster::block_on(client.list_accounts()).unwrap().len(), 1);
    }

    #[test]
    fn a_lock_taken_first_is_the_engines() {
        let config = Config::with_data_dir(tempfile::tempdir().unwrap().path().join("data"));
        let lock = config.lock().unwrap();
        assert!(matches!(config.lock(), Err(Error::AlreadyRunning(_))));
        let client = MailClient::builder_locked(lock)
            .secrets(Arc::new(MemorySecrets::default()))
            .open()
            .unwrap();
        assert!(matches!(config.lock(), Err(Error::AlreadyRunning(_))));
        drop(client);
        config.lock().unwrap();
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
        std::fs::File::options()
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
}
