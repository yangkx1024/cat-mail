//! Fake IMAP and SMTP servers holding the repository's fixture messages, and a client
//! synced to them, for tests that drive the real views; a web server for remote images.

use std::sync::Arc;
use std::time::{Duration, Instant};

use catmail_core::{
    AccountId, Config, MailClient, MemorySecrets, NewAccount, Security, ServerConfig, SyncSchedule,
    Verify,
};
use catmail_testkit::{
    FakeHttp, FakeImap, FakeMailbox, FakeSmtp, Reply, ServerState, SmtpState, response,
};

/// INBOX, in UID order, with their IMAP flags.
const FIXTURES: &[(&[u8], &[&str])] = &[
    (
        include_bytes!("../../../testenv/fixtures/01-plain.eml"),
        &["\\Seen"],
    ),
    (
        include_bytes!("../../../testenv/fixtures/02-utf8-subject.eml"),
        &["\\Flagged"],
    ),
    (
        include_bytes!("../../../testenv/fixtures/03-attachment.eml"),
        &[],
    ),
    (
        include_bytes!("../../../testenv/fixtures/04-gb2312.eml"),
        &[],
    ),
    (
        include_bytes!("../../../testenv/fixtures/06-html-inline.eml"),
        &[],
    ),
    (include_bytes!("../fixtures/newsletter.eml"), &[]),
];

/// The server's folders besides INBOX, with their special-use attributes.
const FOLDERS: &[(&str, &str)] = &[
    ("Archive", "\\Archive"),
    ("Drafts", "\\Drafts"),
    ("Sent", "\\Sent"),
    ("Trash", "\\Trash"),
];

pub struct TestMail {
    pub client: MailClient,
    account: Option<AccountId>,
    fake: FakeImap,
    smtp: FakeSmtp,
    // Hosts the fake server only; the client runs its own runtime, as in the app.
    _server_runtime: tokio::runtime::Runtime,
    _dir: tempfile::TempDir,
}

impl TestMail {
    /// One account (`alice@example.com`), synced.
    pub fn start() -> Self {
        Self::start_with_folders(FOLDERS)
    }

    /// [`start`](Self::start), the client keeping the account in sync in the background as
    /// the app does: for tests of what the engine syncs on its own. Returns once its first
    /// sync of every folder is done.
    pub fn start_syncing_in_background() -> Self {
        let mut mail = Self::with_state(fixtures(FOLDERS), Some(SyncSchedule::default()));
        mail.add_account("alice@example.com");
        wait_until(|| !mail.client.account_status(mail.account()).syncing);
        mail
    }

    /// [`start`](Self::start), with only these folders besides INBOX.
    pub fn start_with_folders(folders: &[(&str, &str)]) -> Self {
        let mut mail = Self::with_state(fixtures(folders), None);
        mail.add_account("alice@example.com");
        mail
    }

    /// A Gmail account (`alice@gmail.com`), synced. In All Mail: "Plan" in INBOX and
    /// Work, "Trip" with Work and Personal, "Notes" with none.
    pub fn start_gmail() -> Self {
        let mut state = ServerState::gmail("alice", "pw");
        state
            .mailboxes
            .extend([FakeMailbox::new("Personal"), FakeMailbox::new("Work")]);
        let all = state.all_mail();
        for (subject, labels) in [
            ("Notes", &[][..]),
            ("Trip", &["Work", "Personal"]),
            ("Plan", &["\\Inbox", "Work"]),
        ] {
            let uid = all.append_simple(subject);
            all.set_labels(uid, labels);
        }
        let mut mail = Self::with_state(state, None);
        mail.add_account("alice@gmail.com");
        mail
    }

    fn add_account(&mut self, email: &str) {
        let new = NewAccount {
            email: email.into(),
            display_name: None,
            username: "alice".into(),
            imap: self.server(),
            smtp: self.smtp_server(),
        };
        self.account = Some(pollster::block_on(async {
            let account = self
                .client
                .add_account(new, "pw".into(), Verify::Skip)
                .await
                .unwrap();
            self.client.sync_account(account.id).await.unwrap();
            account.id
        }));
    }

    /// The server (user `alice`, password `pw`) and a client with no accounts.
    pub fn without_account() -> Self {
        Self::with_state(fixtures(FOLDERS), None)
    }

    /// [`without_account`](Self::without_account), the client keeping accounts in sync in
    /// the background as the app does: for tests of what the engine syncs on its own.
    pub fn without_account_syncing_in_background() -> Self {
        Self::with_state(fixtures(FOLDERS), Some(SyncSchedule::default()))
    }

    fn with_state(state: ServerState, sync_in_background: Option<SyncSchedule>) -> Self {
        let server_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let fake = server_runtime.block_on(FakeImap::start(state));
        let smtp = server_runtime.block_on(FakeSmtp::start(SmtpState::new("alice", "pw")));
        let dir = tempfile::tempdir().unwrap();
        // Sends in the background, as the app does; unless asked, syncs only when told to,
        // so tests decide when the server is looked at.
        let client = MailClient::builder(Config::with_data_dir(dir.path()))
            .secrets(Arc::new(MemorySecrets::default()))
            .sync_in_background(sync_in_background)
            .send_in_background(true)
            .open()
            .unwrap();
        Self {
            client,
            account: None,
            fake,
            smtp,
            _server_runtime: server_runtime,
            _dir: dir,
        }
    }

    /// Where the fake server listens; it speaks plain IMAP.
    pub fn server(&self) -> ServerConfig {
        ServerConfig {
            host: "127.0.0.1".into(),
            port: self.fake.port(),
            security: Security::Plain,
        }
    }

    /// Where the fake SMTP server listens; plain, like the IMAP one.
    pub fn smtp_server(&self) -> ServerConfig {
        ServerConfig {
            host: "127.0.0.1".into(),
            port: self.smtp.port(),
            security: Security::Plain,
        }
    }

    pub fn account(&self) -> AccountId {
        self.account.unwrap()
    }

    /// The messages the SMTP server took, as sent.
    pub fn sent(&self) -> Vec<String> {
        self.smtp
            .state()
            .received
            .iter()
            .map(|m| String::from_utf8_lossy(&m.data).into_owned())
            .collect()
    }

    /// The server's flags on a message in `mailbox`, by UID.
    pub fn server_flags(&self, mailbox: &str, uid: u32) -> Vec<String> {
        let mut state = self.fake.state();
        state.mailbox_mut(mailbox).message_mut(uid).flags.clone()
    }

    /// The Gmail labels of the message with `subject` in All Mail, on the server, in wire
    /// form (`\\Inbox`, `Work`).
    pub fn server_labels(&self, subject: &str) -> Vec<String> {
        let mut state = self.fake.state();
        let all = state.all_mail();
        let msg = all
            .messages
            .iter()
            .find(|m| String::from_utf8_lossy(&m.raw).contains(&format!("Subject: {subject}\r\n")))
            .expect("in All Mail");
        msg.labels.clone()
    }

    /// Hides All Mail from IMAP, as Gmail's settings can, and syncs.
    pub fn hide_all_mail_and_sync(&self) {
        self.fake
            .state()
            .mailboxes
            .retain(|m| m.name != "[Gmail]/All Mail");
        pollster::block_on(self.client.sync_account(self.account.unwrap())).unwrap();
    }

    /// The UIDs in a mailbox on the server.
    pub fn server_uids(&self, mailbox: &str) -> Vec<u32> {
        let mut state = self.fake.state();
        state
            .mailbox_mut(mailbox)
            .messages
            .iter()
            .map(|m| m.uid)
            .collect()
    }

    /// Expunges INBOX's message `uid` on the server and syncs.
    pub fn expunge_and_sync(&self, uid: u32) {
        self.fake.state().inbox().expunge(uid);
        pollster::block_on(self.client.sync_account(self.account.unwrap())).unwrap();
    }

    /// From now on the server refuses changes to messages, with this reason.
    pub fn refuse_changes(&self, reason: &str) {
        self.fake.state().faults.refuse_changes = Some(reason.into());
    }

    /// Puts a new message into one of the server's mailboxes, without syncing.
    pub fn deliver(&self, mailbox: &str, subject: &str) {
        self.fake
            .state()
            .mailbox_mut(mailbox)
            .append_simple(subject);
    }

    /// Holds the server's answers to header FETCHes, keeping a sync of new messages from
    /// finishing, until the gate gets permits.
    pub fn hold_fetches(&self) -> Arc<tokio::sync::Semaphore> {
        self.fake.state().hold_fetches()
    }

    /// The subjects of the messages stored in `folder`, newest first.
    pub fn stored_subjects(&self, folder: &str) -> Vec<String> {
        pollster::block_on(async {
            let folder = self.client.find_folder(self.account(), folder).await?;
            let folder = folder.expect("the folder");
            self.client.list_messages(folder.id, 100, None).await
        })
        .unwrap()
        .into_iter()
        .filter_map(|m| m.subject)
        .collect()
    }

    /// Puts `raw` into the server's INBOX and syncs.
    pub fn deliver_raw_and_sync(&self, raw: &[u8]) {
        self.fake.state().mailbox_mut("INBOX").append(raw, &[]);
        pollster::block_on(self.client.sync_account(self.account.unwrap())).unwrap();
    }

    /// A web server answering every request with a red 40 px square, `/slow.png` only once
    /// [released](FakeHttp::release).
    pub fn serve_images(&self) -> FakeHttp {
        let reply = response(
            "200 OK",
            &["Content-Type: image/png"],
            &solid_png(40, 40, [255, 0, 0]),
        );
        self._server_runtime.block_on(FakeHttp::start(move |path| {
            let reply = reply.clone();
            match path {
                "/slow.png" => Reply::Held(reply),
                _ => Reply::Send(reply),
            }
        }))
    }

    /// Puts a new message into the server's INBOX and syncs, as another client's sync
    /// would; the window learns about it only through the engine's events.
    pub fn deliver_and_sync(&self, subject: &str) {
        self.deliver("INBOX", subject);
        pollster::block_on(self.client.sync_account(self.account.unwrap())).unwrap();
    }
}

/// The server (user `alice`, password `pw`) with the fixtures in INBOX, and `folders`.
fn fixtures(folders: &[(&str, &str)]) -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    for (raw, flags) in FIXTURES {
        state.inbox().append(raw, flags);
    }
    for (name, attribute) in folders {
        state
            .mailboxes
            .push(FakeMailbox::new(name).with_attribute(attribute));
    }
    state
}

/// A PNG all of one color.
pub fn solid_png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut png = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        &rgb.repeat((width * height) as usize),
        width,
        height,
        image::ExtendedColorType::Rgb8,
    )
    .unwrap();
    png
}

/// Calls `step` until it returns true; `step` should let GPUI run its queued work first.
/// The engine works on its own threads, which GPUI's test scheduler doesn't see, so this
/// also waits in real time between calls.
pub fn wait_until(mut step: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if step() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
