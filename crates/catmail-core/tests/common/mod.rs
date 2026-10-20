//! Shared setup for the integration tests against the fake IMAP and SMTP servers.

use std::sync::Arc;
use std::time::Duration;

use catmail_core::{
    AccountId, AuthKind, Config, Flags, Folder, MailClient, MailClientBuilder, MemorySecrets,
    MessageId, MessageSummary, NewAccount, Security, ServerConfig,
};
use catmail_testkit::{FakeImap, FakeSmtp, ServerState, SmtpState};
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

pub async fn env(state: ServerState) -> Env {
    env_with(state, SmtpState::new("alice", "pw"), false).await
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
    let server = |port| ServerConfig {
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
    };
    let account = client
        .add_account(
            NewAccount {
                email: "alice@example.com".into(),
                display_name: None,
                username: "alice".into(),
                imap: server(fake.port()),
                smtp: server(smtp.port()),
                auth: AuthKind::Password,
            },
            "pw".into(),
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
    #[allow(dead_code)]
    pub async fn list(&self, name: &str) -> Vec<MessageSummary> {
        let folder = self.folder(name).await;
        self.client.list_messages(folder.id, 100, 0).await.unwrap()
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
            .list_messages(folder.id, 10_000, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|m| (m.subject.unwrap_or_default(), m.flags))
            .collect()
    }

    #[allow(dead_code)]
    pub async fn folder(&self, name: &str) -> Folder {
        self.client
            .find_folder(self.account, name)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("no folder {name}"))
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
