//! What the unit tests of the client and the engine behind it share: clients on a
//! temporary data directory, accounts and drafts to work on, and secret stores that
//! misbehave on cue.

use std::path::Path;
use std::sync::{Arc, Mutex};

use catmail_types::{
    AccountId, Address, Draft, DraftContent, NewAccount, OutboxEntry, Security, ServerConfig,
};
use secrecy::{ExposeSecret, SecretString};
use tokio::runtime::Handle;

use crate::{Config, MailClient, MemorySecrets, Result, SecretStore, Verify};

/// A client on `dir`, on the test's runtime.
pub(crate) fn open_client(dir: &Path, secrets: Arc<dyn SecretStore>) -> MailClient {
    MailClient::builder(Config::with_data_dir(dir))
        .runtime(Handle::current())
        .secrets(secrets)
        .open()
        .unwrap()
}

/// A client on `dir` with one account, `email`.
pub(crate) async fn client_with_account(
    dir: &Path,
    secrets: Arc<dyn SecretStore>,
    email: &str,
) -> (MailClient, AccountId) {
    let client = open_client(dir, secrets);
    let account = client
        .add_account(new_account(email), "pw".into(), Verify::Skip)
        .await
        .unwrap();
    (client, account.id)
}

pub(crate) fn new_account(email: &str) -> NewAccount {
    NewAccount {
        email: email.into(),
        display_name: None,
        username: email.into(),
        imap: ServerConfig {
            host: "imap.example.com".into(),
            port: 993,
            security: Security::Tls,
        },
        smtp: ServerConfig {
            host: "smtp.example.com".into(),
            port: 465,
            security: Security::Tls,
        },
    }
}

/// A draft to `b@example.com`.
pub(crate) async fn draft_to_b(client: &MailClient, account: AccountId) -> Draft {
    let content = DraftContent {
        to: vec![Address {
            name: None,
            email: "b@example.com".into(),
        }],
        ..Default::default()
    };
    client
        .create_draft(account, None, content, Vec::new())
        .await
        .unwrap()
}

pub(crate) async fn queue_one(client: &MailClient, account: AccountId) -> OutboxEntry {
    let draft = draft_to_b(client, account).await;
    client.send_draft(draft.id).await.unwrap()
}

/// The key the account's password is stored under.
pub(crate) fn secret_key(client: &MailClient, id: AccountId) -> String {
    client
        .engine()
        .unwrap()
        .store
        .read(|db| db.accounts().secret_key(id))
        .unwrap()
        .unwrap()
}

pub(crate) fn stored(secrets: &MemorySecrets, key: &str) -> Option<String> {
    let secret = secrets.get(key).unwrap()?;
    Some(secret.expose_secret().to_owned())
}

/// Blocks inside `set` until the test releases it, to widen race windows.
pub(crate) struct GatedSecrets {
    pub(crate) inner: MemorySecrets,
    pub(crate) entered: Arc<tokio::sync::Notify>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl GatedSecrets {
    pub(crate) fn new() -> (Arc<Self>, std::sync::mpsc::Sender<()>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let secrets = Arc::new(Self {
            inner: MemorySecrets::default(),
            entered: Arc::default(),
            release: Mutex::new(rx),
        });
        (secrets, tx)
    }
}

impl SecretStore for GatedSecrets {
    fn set(&self, key: &str, secret: &SecretString) -> Result<()> {
        self.entered.notify_one();
        self.release.lock().unwrap().recv().unwrap();
        self.inner.set(key, secret)
    }
    fn get(&self, key: &str) -> Result<Option<SecretString>> {
        self.inner.get(key)
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.inner.delete(key)
    }
}
