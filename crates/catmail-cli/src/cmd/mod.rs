//! The commands, one module per group, and the lookups they share. How results look
//! is `output.rs` for people and `json.rs` for scripts.

pub(crate) mod account;
pub(crate) mod compose;
pub(crate) mod gc;
pub(crate) mod identity;
pub(crate) mod ops;
pub(crate) mod outbox;
pub(crate) mod read;
pub(crate) mod search;
pub(crate) mod sync;
pub(crate) mod watch;

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use catmail_core::{Account, AccountId, FolderId, MailClient, MessageCursor, MessageId};

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

/// Where the page after message `id` starts.
pub(crate) async fn cursor_after(
    client: &MailClient,
    id: Option<i64>,
) -> Result<Option<MessageCursor>> {
    let Some(id) = id else {
        return Ok(None);
    };
    let message = client
        .get_message(MessageId(id))
        .await?
        .with_context(|| format!("no message {id}"))?;
    Ok(Some(message.summary.cursor()))
}

/// Display names of the accounts' folders, for FOLDER columns.
pub(crate) async fn folder_names(
    client: &MailClient,
    accounts: impl IntoIterator<Item = AccountId>,
) -> Result<HashMap<FolderId, String>> {
    let mut folders = HashMap::new();
    for account in accounts {
        for folder in client.list_folders(account).await? {
            folders.insert(folder.id, folder.name);
        }
    }
    Ok(folders)
}
