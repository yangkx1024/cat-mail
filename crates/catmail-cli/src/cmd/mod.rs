//! The commands, one module per group, and the lookups they share. How results look
//! is `output.rs` for people and `json.rs` for scripts.

pub(crate) mod account;
pub(crate) mod compose;
pub(crate) mod folder;
pub(crate) mod gc;
pub(crate) mod identity;
pub(crate) mod mcp;
pub(crate) mod ops;
pub(crate) mod outbox;
pub(crate) mod read;
pub(crate) mod search;
pub(crate) mod sync;
pub(crate) mod watch;

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use catmail_core::{
    Account, AccountId, Folder, FolderId, MailClient, MessageCursor, MessageId, MessageSummary,
};

/// The account `selector` names, or the only account there is.
pub(crate) async fn one_account(client: &MailClient, selector: Option<&str>) -> Result<Account> {
    match selector {
        Some(selector) => resolve_account(client, selector).await,
        None => {
            let mut accounts = client.list_accounts().await?;
            match accounts.len() {
                0 => bail!("no accounts; add one with `catmail account add`"),
                1 => Ok(accounts.remove(0)),
                _ => bail!("there are several accounts; choose one with --account"),
            }
        }
    }
}

/// Accepts either a numeric id or an email address.
pub(crate) async fn resolve_account(client: &MailClient, selector: &str) -> Result<Account> {
    let found = match selector.parse::<i64>() {
        Ok(id) => client.get_account(AccountId(id)).await?,
        Err(_) => client.find_account_by_email(selector).await?,
    };
    found.with_context(|| format!("no account matches `{selector}`"))
}

/// The account's folder of that name (see [`MailClient::find_folder`]); fails if there is
/// none.
pub(crate) async fn find_folder(
    client: &MailClient,
    account: AccountId,
    name: &str,
) -> Result<Folder> {
    client
        .find_folder(account, name)
        .await?
        .with_context(|| format!("no folder `{name}` in account {account}"))
}

/// Where the page after message `id` starts.
pub(crate) async fn cursor_after(
    client: &MailClient,
    id: Option<i64>,
) -> Result<Option<MessageCursor>> {
    let Some(id) = id else {
        return Ok(None);
    };
    Ok(Some(summary(client, MessageId(id)).await?.cursor()))
}

/// Message `id` without its body; fails if there is no such message.
pub(crate) async fn summary(client: &MailClient, id: MessageId) -> Result<MessageSummary> {
    client
        .message_summaries(vec![id])
        .await?
        .into_iter()
        .next()
        .with_context(|| format!("message {id} not found"))
}

/// The accounts' folders, by id: for FOLDER columns, and the names of labels.
pub(crate) async fn account_folders(
    client: &MailClient,
    accounts: impl IntoIterator<Item = AccountId>,
) -> Result<HashMap<FolderId, Folder>> {
    let mut folders = HashMap::new();
    for account in accounts {
        for folder in client.list_folders(account).await? {
            folders.insert(folder.id, folder);
        }
    }
    Ok(folders)
}
