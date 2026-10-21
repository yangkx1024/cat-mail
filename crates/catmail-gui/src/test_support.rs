//! Fake IMAP and SMTP servers holding the repository's fixture messages, and a client
//! synced to them, for tests that drive the real views.

use std::sync::Arc;
use std::time::{Duration, Instant};

use catmail_core::{
    AccountId, AuthKind, Config, MailClient, MemorySecrets, Message, NewAccount, Security,
    ServerConfig,
};
use catmail_testkit::{FakeImap, FakeMailbox, FakeSmtp, ServerState, SmtpState};

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

    /// [`start`](Self::start), with only these folders besides INBOX.
    pub fn start_with_folders(folders: &[(&str, &str)]) -> Self {
        let mut mail = Self::with_folders(folders);
        let new = NewAccount {
            email: "alice@example.com".into(),
            display_name: None,
            username: "alice".into(),
            imap: mail.server(),
            smtp: mail.smtp_server(),
            auth: AuthKind::Password,
        };
        mail.account = Some(pollster::block_on(async {
            let account = mail.client.add_account(new, "pw".into()).await.unwrap();
            mail.client.sync_account(account.id).await.unwrap();
            account.id
        }));
        mail
    }

    /// The server (user `alice`, password `pw`) and a client with no accounts.
    pub fn without_account() -> Self {
        Self::with_folders(FOLDERS)
    }

    fn with_folders(folders: &[(&str, &str)]) -> Self {
        let server_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let mut state = ServerState::new("alice", "pw");
        for (raw, flags) in FIXTURES {
            state.inbox().append(raw, flags);
        }
        for (name, attribute) in folders {
            state
                .mailboxes
                .push(FakeMailbox::new(name).with_attribute(attribute));
        }
        let fake = server_runtime.block_on(FakeImap::start(state));
        let smtp = server_runtime.block_on(FakeSmtp::start(SmtpState::new("alice", "pw")));
        let dir = tempfile::tempdir().unwrap();
        // Sends in the background, as the app does; syncs only when told to, so tests
        // decide when the server is looked at.
        let client = MailClient::builder(Config::with_data_dir(dir.path()))
            .secrets(Arc::new(MemorySecrets::default()))
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

    /// INBOX's message with this subject, its body downloaded.
    pub fn download(&self, subject: &str) -> Message {
        pollster::block_on(async {
            let account = self.account.unwrap();
            let inbox = self.client.find_folder(account, "inbox").await.unwrap();
            let inbox = inbox.unwrap();
            let listed = self
                .client
                .list_messages(inbox.id, 100, None)
                .await
                .unwrap();
            let summary = listed
                .into_iter()
                .find(|m| m.subject.as_deref() == Some(subject))
                .unwrap();
            self.client.download_message(summary.id).await.unwrap()
        })
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

    /// Puts a new message into the server's INBOX and syncs, as another client's sync
    /// would; the window learns about it only through the engine's events.
    pub fn deliver_and_sync(&self, subject: &str) {
        self.deliver("INBOX", subject);
        pollster::block_on(self.client.sync_account(self.account.unwrap())).unwrap();
    }
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
