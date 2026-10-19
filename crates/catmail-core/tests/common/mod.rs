//! Shared setup for the integration tests against the fake IMAP server.

use std::sync::Arc;

use catmail_core::{
    AccountId, AuthKind, Config, MailClient, MemorySecrets, NewAccount, Security, ServerConfig,
};
use catmail_testkit::{FakeImap, ServerState};
use tokio::runtime::Handle;

/// A fake server and a client with one account on it (alice / pw), not yet synced.
pub struct Env {
    pub fake: FakeImap,
    pub client: MailClient,
    pub account: AccountId,
    _dir: tempfile::TempDir,
}

pub async fn env(state: ServerState) -> Env {
    let fake = FakeImap::start(state).await;
    let dir = tempfile::tempdir().unwrap();
    let client = MailClient::builder(Config::with_data_dir(dir.path()))
        .runtime(Handle::current())
        .secrets(Arc::new(MemorySecrets::default()))
        .open()
        .unwrap();
    let server = ServerConfig {
        host: "127.0.0.1".into(),
        port: fake.port(),
        security: Security::Plain,
    };
    let account = client
        .add_account(
            NewAccount {
                email: "alice@example.com".into(),
                display_name: None,
                username: "alice".into(),
                imap: server.clone(),
                smtp: server,
                auth: AuthKind::Password,
            },
            "pw".into(),
        )
        .await
        .unwrap()
        .id;
    Env {
        fake,
        client,
        account,
        _dir: dir,
    }
}

impl Env {
    /// Commands the server received from the `start`-th on, without tags.
    pub fn commands_since(&self, start: usize) -> Vec<String> {
        self.fake.state().log[start..]
            .iter()
            .map(|c| c.line.clone())
            .collect()
    }
}
