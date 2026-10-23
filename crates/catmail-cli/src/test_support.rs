//! Setup shared by the unit tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use catmail_core::{
    AccountId, AuthKind, Config, MailClient, MemorySecrets, NewAccount, SecretStore, SecretString,
    Security, ServerConfig,
};
use catmail_testkit::FakeImap;

/// Secrets in memory, counting how many were ever stored.
#[derive(Default)]
pub(crate) struct CountingSecrets {
    inner: MemorySecrets,
    stored: AtomicUsize,
}

impl CountingSecrets {
    /// Whether nothing was ever stored.
    pub(crate) fn is_empty(&self) -> bool {
        self.stored.load(Ordering::SeqCst) == 0
    }
}

impl SecretStore for CountingSecrets {
    fn set(&self, key: &str, secret: &SecretString) -> catmail_core::Result<()> {
        self.stored.fetch_add(1, Ordering::SeqCst);
        self.inner.set(key, secret)
    }

    fn get(&self, key: &str) -> catmail_core::Result<Option<SecretString>> {
        self.inner.get(key)
    }

    fn delete(&self, key: &str) -> catmail_core::Result<()> {
        self.inner.delete(key)
    }
}

/// A client on `dir`, without accounts.
pub(crate) fn client(dir: &std::path::Path) -> (MailClient, Arc<CountingSecrets>) {
    let secrets = Arc::new(CountingSecrets::default());
    let client = MailClient::builder(Config::with_data_dir(dir))
        .runtime(tokio::runtime::Handle::current())
        .secrets(secrets.clone())
        .open()
        .unwrap();
    (client, secrets)
}

/// A client on `dir` with alice@example.com on the fake server, synced once.
pub(crate) async fn synced(fake: &FakeImap, dir: &std::path::Path) -> (MailClient, AccountId) {
    synced_with_smtp(fake, fake.port(), dir).await
}

/// Like [`synced`], sending through the SMTP server on `smtp_port`.
pub(crate) async fn synced_with_smtp(
    fake: &FakeImap,
    smtp_port: u16,
    dir: &std::path::Path,
) -> (MailClient, AccountId) {
    let (client, _) = client(dir);
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
                smtp: server(smtp_port),
                auth: AuthKind::Password,
            },
            "pw".into(),
        )
        .await
        .unwrap()
        .id;
    client.sync_account(account).await.unwrap();
    (client, account)
}
