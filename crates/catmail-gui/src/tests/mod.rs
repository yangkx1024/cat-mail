//! The app's windows driven as a user would, headless (nsh-testing's `Headless`, on the
//! shell's headless toolkit: `shell/headless.rs`), against the fake IMAP and SMTP
//! servers: reading, writing, the settings and the folders.
//!
//! Each test runs the whole app on a client of its own, one test at a time (the engine
//! and the settings are the process's). The client syncs and sends in the background, as
//! the app does: what the window changes reaches the servers on its own.

mod folders;
mod reading;
mod settings;
mod writing;

use std::sync::Arc;
use std::time::Duration;

use catmail_core::{
    AccountId, Config, DraftSummary, MailClient, MemorySecrets, NewAccount, Security, ServerConfig,
    SyncSchedule, Verify,
};
use catmail_testkit::{FakeImap, FakeMailbox, FakeSmtp, ServerState, SmtpState};

use crate::shell::toolkit::{Desktop, TestWindow};

/// How long a step waits for what it started (a sync, a send) to show.
const WAIT: Duration = Duration::from_secs(10);

/// INBOX, oldest first: (subject, text), from Bob to the account.
const INBOX: &[(&str, &str)] = &[
    ("Lunch on Friday", "Shall we try the new place?\r\n"),
    ("Quarterly report", "The numbers are in.\r\n"),
    ("Weekend plans", "Hiking, if the weather holds.\r\n"),
];

/// The server's folders besides INBOX, with their special-use attributes.
const FOLDERS: &[(&str, &str)] = &[
    ("Archive", "\\Archive"),
    ("Drafts", "\\Drafts"),
    ("Sent", "\\Sent"),
    ("Trash", "\\Trash"),
];

/// The fake servers, a client and, unless [`TestMail::without_account`], an account on
/// them that it has synced.
pub struct TestMail {
    pub client: MailClient,
    pub account: Option<AccountId>,
    pub imap: FakeImap,
    pub smtp: FakeSmtp,
    dir: tempfile::TempDir,
    // The fake servers' only; the client runs its own runtime, as in the app.
    _servers: tokio::runtime::Runtime,
}

impl TestMail {
    /// alice@example.com (password "pw"), with [`INBOX`] and [`FOLDERS`], synced.
    pub fn start() -> Self {
        let mut mail = Self::without_account();
        let account = pollster::block_on(mail.client.add_account(
            NewAccount {
                email: "alice@example.com".into(),
                display_name: Some("Alice".into()),
                username: "alice".into(),
                imap: local(mail.imap.port()),
                smtp: local(mail.smtp.port()),
            },
            "pw".into(),
            Verify::Skip,
        ))
        .unwrap()
        .id;
        pollster::block_on(mail.client.sync_account(account)).unwrap();
        mail.account = Some(account);
        mail
    }

    /// The servers of [`start`](Self::start), and a client with no account yet.
    pub fn without_account() -> Self {
        let mut state = ServerState::new("alice", "pw");
        for (n, (subject, text)) in INBOX.iter().enumerate() {
            let raw = format!(
                "From: Bob <bob@example.com>\r\nTo: alice@example.com\r\nSubject: {subject}\r\n\
                 Date: Sat, 17 Oct 2026 09:0{n}:00 +0800\r\nMessage-ID: <{n}@example.com>\r\n\
                 \r\n{text}"
            );
            state.inbox().append(raw.as_bytes(), &[]);
        }
        for (name, attribute) in FOLDERS {
            state
                .mailboxes
                .push(FakeMailbox::new(name).with_attribute(attribute));
        }
        let servers = tokio::runtime::Runtime::new().unwrap();
        let imap = servers.block_on(FakeImap::start(state));
        let smtp = servers.block_on(FakeSmtp::start(SmtpState::new("alice", "pw")));
        let dir = tempfile::tempdir().unwrap();
        Self {
            client: open_client(dir.path()),
            account: None,
            imap,
            smtp,
            dir,
            _servers: servers,
        }
    }

    pub fn account(&self) -> AccountId {
        self.account.expect("an account")
    }

    /// The subjects in the server's mailbox `name`, in UID order.
    pub fn on_server(&self, name: &str) -> Vec<String> {
        let state = self.imap.state();
        let Some(mailbox) = state.mailboxes.iter().find(|m| m.name == name) else {
            return Vec::new();
        };
        mailbox
            .messages
            .iter()
            .filter_map(|m| header(&String::from_utf8_lossy(&m.raw), "Subject"))
            .collect()
    }

    /// The messages the SMTP server delivered, in order, as text.
    pub fn delivered(&self) -> Vec<String> {
        self.smtp
            .state()
            .received
            .iter()
            .map(|m| String::from_utf8_lossy(&m.data).into_owned())
            .collect()
    }

    /// The app on this client, and its main window once it lists the INBOX.
    pub fn open(&self) -> (Desktop, TestWindow) {
        let (desktop, mut main) = self.open_window();
        let last = INBOX.last().unwrap().0;
        main.app
            .wait_for_within(last, WAIT)
            .unwrap_or_else(|e| panic!("{e}"));
        (desktop, main)
    }

    /// The app on this client, and its main window as it opens.
    pub fn open_window(&self) -> (Desktop, TestWindow) {
        let desktop = Desktop::start(self.client.clone());
        let main = desktop.window(crate::shell::APP_NAME);
        (desktop, main)
    }

    /// The account's drafts, read by a client opened again on the data directory once
    /// the app has quit.
    pub fn drafts_after_quitting(&self) -> Vec<DraftSummary> {
        let client = open_client(self.dir.path());
        pollster::block_on(client.list_drafts(self.account())).unwrap()
    }
}

fn open_client(dir: &std::path::Path) -> MailClient {
    MailClient::builder(Config::with_data_dir(dir))
        .secrets(Arc::new(MemorySecrets::default()))
        .sync_in_background(Some(SyncSchedule::default()))
        .send_in_background(true)
        .open()
        .unwrap()
}

fn local(port: u16) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
    }
}

/// The value of the header field `name` of the message `raw`.
pub fn header(raw: &str, name: &str) -> Option<String> {
    let prefix = format!("{name}: ");
    raw.lines()
        .take_while(|line| !line.is_empty())
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::to_owned)
}

/// Waits for `done` to hold, for what a step started outside the window (on the
/// servers).
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let until = std::time::Instant::now() + WAIT;
    while !done() {
        assert!(std::time::Instant::now() < until, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
