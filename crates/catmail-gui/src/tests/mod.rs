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

use catmail_core::test_support::{add_synced_account, local_account};
use catmail_core::{
    AccountId, Config, DraftSummary, ListQuery, MailClient, MemorySecrets, MessageId, SyncSchedule,
};
use catmail_testkit::{FakeImap, FakeMailbox, FakeSmtp, ServerState, SmtpState};
use mail_parser::{Message, MessageParser};
use nsh_testing::Headless;

use crate::shell::headless::{Desktop, TestWindow, Turn, WAIT, poll_until};

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
    client: MailClient,
    account: Option<AccountId>,
    pub imap: FakeImap,
    pub smtp: FakeSmtp,
    dir: tempfile::TempDir,
    // The fake servers' only; the client runs its own runtime, as in the app.
    _servers: tokio::runtime::Runtime,
    // Taken first and let go of last: a test waiting for its turn holds no servers or
    // client yet.
    turn: Turn,
}

impl TestMail {
    /// alice@example.com (password "pw"), with [`INBOX`] and [`FOLDERS`], synced.
    pub fn start() -> Self {
        Self::start_with_older(0)
    }

    /// [`start`](Self::start), with `older` more messages in the INBOX ([`older_subject`]),
    /// a day before the others: a list longer than the window.
    pub fn start_with_older(older: usize) -> Self {
        let mut mail = Self::without_account_with_older(older);
        let mut new = local_account("alice", mail.imap.port(), mail.smtp.port());
        new.display_name = Some("Alice".into());
        let account = pollster::block_on(add_synced_account(&mail.client, new, "pw"));
        mail.account = Some(account);
        mail
    }

    /// The servers of [`start`](Self::start), and a client with no account yet.
    pub fn without_account() -> Self {
        Self::without_account_with_older(0)
    }

    fn without_account_with_older(older: usize) -> Self {
        let turn = Turn::take();
        let mut state = ServerState::new("alice", "pw");
        for n in 0..older {
            let raw = format!(
                "From: Bob <bob@example.com>\r\nTo: alice@example.com\r\nSubject: {}\r\n\
                 Date: Fri, 16 Oct 2026 {:02}:{:02}:00 +0800\r\nMessage-ID: <older{n}@example.com>\r\n\
                 \r\nOlder.\r\n",
                older_subject(n),
                n / 60,
                n % 60,
            );
            state.inbox().append(raw.as_bytes(), &[]);
        }
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
        let servers = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
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
            turn,
        }
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
            .filter_map(|m| Some(parse(&m.raw).subject()?.to_owned()))
            .collect()
    }

    /// The INBOX's message with subject `subject`, as the client has it.
    pub fn inbox_message(&self, subject: &str) -> (AccountId, MessageId) {
        let account = self.account.expect("an account");
        let messages = pollster::block_on(
            self.client
                .list_inboxes(Some(vec![account]), ListQuery::new(1000)),
        )
        .unwrap();
        let message = messages
            .iter()
            .find(|m| m.subject.as_deref() == Some(subject))
            .unwrap_or_else(|| panic!("no {subject:?} in the INBOX"));
        (account, message.id)
    }

    /// Whether the server has a mailbox `name`.
    pub fn has_folder(&self, name: &str) -> bool {
        self.imap.state().mailbox_names().iter().any(|n| n == name)
    }

    /// The messages the SMTP server delivered, in order.
    pub fn delivered(&self) -> Vec<Vec<u8>> {
        self.smtp
            .state()
            .received
            .iter()
            .map(|m| m.data.clone())
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
        let desktop = Desktop::start(self.client.clone(), &self.turn);
        let main = desktop.window(crate::shell::APP_NAME);
        (desktop, main)
    }

    /// The account's drafts, read by a client opened again on the data directory once
    /// the app has quit.
    pub fn drafts_after_quitting(&self) -> Vec<DraftSummary> {
        let client = open_client(self.dir.path());
        let account = self.account.expect("an account");
        pollster::block_on(client.list_drafts(account)).unwrap()
    }
}

/// The subject of the `n`th of [`TestMail::start_with_older`]'s messages.
pub fn older_subject(n: usize) -> String {
    format!("Older note {n}")
}

fn open_client(dir: &std::path::Path) -> MailClient {
    MailClient::builder(Config::with_data_dir(dir))
        .secrets(Arc::new(MemorySecrets::default()))
        .sync_in_background(Some(SyncSchedule::default()))
        .send_in_background(true)
        .open()
        .unwrap()
}

pub fn parse(raw: &[u8]) -> Message<'_> {
    MessageParser::new().parse(raw).expect("a message")
}

/// Waits for `done` to hold, for what a step started outside the window (on the
/// servers).
pub fn wait_until(what: &str, done: impl FnMut() -> bool) {
    assert!(poll_until(WAIT, done), "timed out: {what}");
}

/// Opens the message `subject` from the list, and waits for the reader to show it.
pub fn open_message(app: &mut Headless, subject: &str) {
    app.press(subject).unwrap();
    app.wait_for_within(&format!("id:reader >> text:{subject}"), WAIT)
        .unwrap();
}

/// A new message's composer, opened from the main window.
pub fn compose(desktop: &Desktop, main: &mut TestWindow) -> TestWindow {
    main.app.press("Compose").unwrap();
    let mut composer = desktop.window("New message");
    composer.app.wait_for_within("label:To", WAIT).unwrap();
    composer
}
