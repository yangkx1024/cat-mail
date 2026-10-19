//! Shared setup for the integration tests against the fake IMAP and SMTP servers.

use std::sync::Arc;

use catmail_core::{
    AccountId, AuthKind, Config, MailClient, MemorySecrets, NewAccount, Security, ServerConfig,
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
    _dir: tempfile::TempDir,
}

pub async fn env(state: ServerState) -> Env {
    env_with(state, SmtpState::new("alice", "pw"), false).await
}

/// With `background`, the client sends queued messages on its own.
pub async fn env_with(state: ServerState, smtp: SmtpState, background: bool) -> Env {
    let fake = FakeImap::start(state).await;
    let smtp = FakeSmtp::start(smtp).await;
    let dir = tempfile::tempdir().unwrap();
    let client = MailClient::builder(Config::with_data_dir(dir.path()))
        .runtime(Handle::current())
        .secrets(Arc::new(MemorySecrets::default()))
        .send_in_background(background)
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
