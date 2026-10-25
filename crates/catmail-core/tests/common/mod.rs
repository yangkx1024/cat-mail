//! Shared setup for the integration tests against the fake IMAP and SMTP servers.

use std::sync::Arc;
use std::time::Duration;

use catmail_core::{
    AccountId, Address, Config, Draft, DraftContent, DraftId, Event, Flags, Folder, FolderId,
    FolderSyncReport, MailClient, MailClientBuilder, MemorySecrets, MessageId, MessageSummary,
    NewAccount, ReplayReport, SearchQuery, Security, ServerConfig, SyncReport, Verify,
};
use catmail_testkit::{FakeImap, FakeMailbox, FakeSmtp, ServerState, SmtpState};
use tokio::runtime::Handle;

/// Fake servers and a client with one account on them (alice@example.com, logging in
/// as alice / pw), not yet synced.
pub struct Env {
    pub fake: FakeImap,
    // Only the send tests look at it; the others just keep it running.
    #[allow(dead_code)]
    pub smtp: FakeSmtp,
    pub client: MailClient,
    pub account: AccountId,
    // For opening the client again.
    #[allow(dead_code)]
    pub secrets: Arc<MemorySecrets>,
    #[allow(dead_code)]
    pub dir: tempfile::TempDir,
}

impl Env {
    /// Replaces what the draft says, keeping who it is from.
    #[allow(dead_code)]
    pub async fn edit(&self, id: DraftId, content: DraftContent) -> catmail_core::Result<Draft> {
        let draft = self.client.get_draft(id).await?.expect("the draft");
        self.client.update_draft(id, draft.identity, content).await
    }
}

pub async fn env(state: ServerState) -> Env {
    env_with(state, SmtpState::new("alice", "pw"), false).await
}

/// The events received and not read yet.
/// A recipient without a display name.
#[allow(dead_code)]
pub fn addr(email: &str) -> Address {
    Address {
        name: None,
        email: email.into(),
    }
}

#[allow(dead_code)]
pub fn drain(events: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<Event> {
    std::iter::from_fn(|| events.try_recv().ok()).collect()
}

/// [`env`], synced once.
#[allow(dead_code)]
pub async fn synced_env(state: ServerState) -> Env {
    let env = env(state).await;
    env.client.sync_account(env.account).await.unwrap();
    env
}

/// With `background`, the client sends queued messages on its own.
pub async fn env_with(state: ServerState, smtp: SmtpState, background: bool) -> Env {
    env_built(state, smtp, |b| b.send_in_background(background)).await
}

/// With the client built by `build`, from a builder with the data directory, runtime and
/// secrets set.
pub async fn env_built(
    state: ServerState,
    smtp: SmtpState,
    build: impl FnOnce(MailClientBuilder) -> MailClientBuilder,
) -> Env {
    env_added(state, smtp, Verify::Skip, build).await
}

/// [`env_built`], the account added with `verify`.
pub async fn env_added(
    state: ServerState,
    smtp: SmtpState,
    verify: Verify,
    build: impl FnOnce(MailClientBuilder) -> MailClientBuilder,
) -> Env {
    let fake = FakeImap::start(state).await;
    let smtp = FakeSmtp::start(smtp).await;
    let dir = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecrets::default());
    let client = build(
        MailClient::builder(Config::with_data_dir(dir.path()))
            .runtime(Handle::current())
            .secrets(secrets.clone()),
    )
    .open()
    .unwrap();
    let account = client
        .add_account(
            NewAccount {
                email: "alice@example.com".into(),
                display_name: None,
                username: "alice".into(),
                imap: local_server(fake.port()),
                smtp: local_server(smtp.port()),
            },
            "pw".into(),
            verify,
        )
        .await
        .unwrap()
        .id;
    Env {
        fake,
        smtp,
        client,
        account,
        secrets,
        dir,
    }
}

/// A fake server on this machine, without TLS.
pub fn local_server(port: u16) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
    }
}

/// Waits until `ready` returns true, trying every few milliseconds. Panics after 10
/// seconds.
#[allow(dead_code)]
pub async fn eventually(what: &str, mut ready: impl AsyncFnMut() -> bool) {
    let poll = async {
        while !ready().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(10), poll)
        .await
        .is_err()
    {
        panic!("timed out waiting for {what}");
    }
}

impl Env {
    /// Where the fake SMTP server listens.
    #[allow(dead_code)]
    pub fn smtp_config(&self) -> ServerConfig {
        local_server(self.smtp.port())
    }

    /// Works through the outbox once: each entry worked on, as (status, attempts).
    #[allow(dead_code)]
    pub async fn flush(&self) -> Vec<(catmail_core::OutboxStatus, u32)> {
        self.client
            .flush_outbox(self.account)
            .await
            .unwrap()
            .entries
            .into_iter()
            .map(|e| (e.status, e.attempts))
            .collect()
    }

    /// Runs SQL on the client's database behind its back, e.g. to add a trigger that
    /// makes some writes fail, as a full disk would.
    #[allow(dead_code)]
    pub fn sql(&self, sql: &str) {
        rusqlite::Connection::open(self.client.config().db_path())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    #[allow(dead_code)]
    pub async fn list(&self, name: &str) -> Vec<MessageSummary> {
        let folder = self.folder(name).await;
        self.client
            .list_messages(folder.id, 100, None)
            .await
            .unwrap()
    }

    #[allow(dead_code)]
    pub async fn id_of(&self, name: &str, subject: &str) -> MessageId {
        self.list(name)
            .await
            .into_iter()
            .find(|m| m.subject.as_deref() == Some(subject))
            .unwrap_or_else(|| panic!("no {subject} in {name}"))
            .id
    }

    /// (subject, flags) of the folder's messages, newest first.
    #[allow(dead_code)]
    pub async fn messages(&self, name: &str) -> Vec<(String, Flags)> {
        let folder = self.folder(name).await;
        self.client
            .list_messages(folder.id, 10_000, None)
            .await
            .unwrap()
            .into_iter()
            .map(|m| (m.subject.unwrap_or_default(), m.flags))
            .collect()
    }

    /// Subjects the folder lists, newest first.
    #[allow(dead_code)]
    pub async fn subjects(&self, name: &str) -> Vec<String> {
        self.messages(name).await.into_iter().map(|m| m.0).collect()
    }

    /// The folders of the messages a search finds, newest first.
    #[allow(dead_code)]
    pub async fn search_folders(&self, text: &str) -> Vec<FolderId> {
        let found = self.client.search(SearchQuery::new(text)).await.unwrap();
        found.into_iter().map(|m| m.folder_id).collect()
    }

    #[allow(dead_code)]
    pub async fn folder(&self, name: &str) -> Folder {
        self.client
            .find_folder(self.account, name)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("no folder {name}"))
    }

    #[allow(dead_code)]
    pub async fn replay(&self) -> ReplayReport {
        self.client.replay_pending(self.account).await.unwrap()
    }

    /// Mailboxes the server was asked to EXAMINE, from the `start`-th command on.
    #[allow(dead_code)]
    pub fn examined(&self, start: usize) -> Vec<String> {
        self.commands_since(start)
            .into_iter()
            .filter_map(|c| {
                let name = c.strip_prefix("EXAMINE ")?;
                let name = match name.strip_prefix('"') {
                    Some(quoted) => quoted.split('"').next(),
                    None => name.split(' ').next(),
                };
                name.map(str::to_owned)
            })
            .collect()
    }

    /// Commands the server received from the `start`-th on, without tags.
    #[allow(dead_code)]
    pub fn commands_since(&self, start: usize) -> Vec<String> {
        self.fake.state().log[start..]
            .iter()
            .map(|c| c.line.clone())
            .collect()
    }
}

/// The report on the folder named `name`.
#[allow(dead_code)]
pub fn folder_report<'a>(report: &'a SyncReport, name: &str) -> &'a FolderSyncReport {
    report.folders.iter().find(|f| f.name == name).unwrap()
}

/// One message in INBOX, a copy of it in Important and in All Mail (not marked `\\All`),
/// and an empty Trash.
#[allow(dead_code)]
pub fn copied() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    state.inbox().append_simple("copied");
    let raw = state.inbox().messages[0].raw.clone();
    for name in ["All Mail", "Important"] {
        let mut mailbox = FakeMailbox::new(name);
        mailbox.append(&raw, &[]);
        state.mailboxes.push(mailbox);
    }
    state
        .mailboxes
        .push(FakeMailbox::new("Trash").with_attribute("\\Trash"));
    state
}

/// A sparse file of `size` bytes in `dir`: only what is read gets written.
#[allow(dead_code)]
pub fn sparse(dir: &std::path::Path, name: &str, size: u64) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::File::create(&path).unwrap().set_len(size).unwrap();
    path
}

/// Every file under `dir`.
#[allow(dead_code)]
pub fn files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(self::files(&path));
        } else {
            files.push(path);
        }
    }
    files
}
