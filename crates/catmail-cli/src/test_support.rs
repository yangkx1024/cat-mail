//! Setup shared by the unit tests.

use std::sync::Arc;

use catmail_core::{
    AccountId, AuthKind, Config, MailClient, MemorySecrets, NewAccount, Security, ServerConfig,
};
use catmail_testkit::FakeImap;

/// A client on `dir` with alice@example.com on the fake server, synced once.
pub(crate) async fn synced(fake: &FakeImap, dir: &std::path::Path) -> (MailClient, AccountId) {
    let client = MailClient::builder(Config::with_data_dir(dir))
        .runtime(tokio::runtime::Handle::current())
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
    client.sync_account(account).await.unwrap();
    (client, account)
}
