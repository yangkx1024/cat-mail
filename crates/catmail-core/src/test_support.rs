//! Setup shared by tests against `catmail-testkit`'s local fake servers.

use crate::{AccountId, MailClient, NewAccount, Security, ServerConfig, Verify};

/// A plaintext server on localhost, for tests against `catmail-testkit`.
pub fn local_server(port: u16) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
    }
}

/// `user`@example.com on plaintext local servers, with no display name.
pub fn local_account(user: &str, imap_port: u16, smtp_port: u16) -> NewAccount {
    NewAccount {
        email: format!("{user}@example.com"),
        display_name: None,
        username: user.into(),
        imap: local_server(imap_port),
        smtp: local_server(smtp_port),
    }
}

/// Adds `account` without checking the login, syncs it once, and returns its id.
pub async fn add_synced_account(
    client: &MailClient,
    account: NewAccount,
    password: &str,
) -> AccountId {
    let id = client
        .add_account(account, password.into(), Verify::Skip)
        .await
        .unwrap()
        .id;
    client.sync_account(id).await.unwrap();
    id
}
