//! `catmail ls` and `catmail search`: folders, and pages of messages.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use catmail_core::{AccountId, MailClient, MessageSummary, SearchQuery};
use clap::Args;

use crate::cmd::{account_folders, cursor_after, one_account, resolve_account};
use crate::json::{self, JsonFolder, JsonSummary};
use crate::output::{ALL_MAIL_HIDDEN, print_folders, print_message_table, printable};

#[derive(Args)]
pub(crate) struct Ls {
    /// Folder name (e.g. INBOX); without it, list the folders
    folder: Option<String>,
    /// Account id or email address [default: the only account]
    #[arg(short, long)]
    account: Option<String>,
    /// Maximum number of messages
    #[arg(short = 'n', long, default_value_t = 50)]
    limit: u32,
    /// Only the messages after this message id, the last one of the previous page
    #[arg(long)]
    after: Option<i64>,
}

#[derive(Args)]
pub(crate) struct Search {
    /// Words or "quoted phrases", all of which must match; prefix with from:, to: or
    /// subject: to search one field. Chinese, Japanese and Korean match as substrings
    #[arg(required = true, num_args = 1..)]
    query: Vec<String>,
    /// Account id or email address [default: every account]
    #[arg(short, long)]
    account: Option<String>,
    /// Only this folder (needs --account if there are several accounts)
    #[arg(long)]
    folder: Option<String>,
    /// Maximum number of messages
    #[arg(short = 'n', long, default_value_t = 50)]
    limit: u32,
    /// Only the matches after this message id, the last one of the previous page
    #[arg(long)]
    after: Option<i64>,
}

pub(crate) async fn ls(client: &MailClient, args: Ls, json: bool) -> Result<()> {
    let account = one_account(client, args.account.as_deref()).await?;
    let Some(name) = args.folder else {
        let folders = client.list_folders(account.id).await?;
        if json {
            json::print_list::<JsonFolder, _>("folders", &folders)?;
        } else if folders.is_empty() {
            println!("No folders yet. Run `catmail sync` first.");
        } else {
            print_folders(&folders);
            if account.all_mail_hidden() {
                println!("\n{ALL_MAIL_HIDDEN}");
            }
        }
        return Ok(());
    };
    let folder = client
        .find_folder(account.id, &name)
        .await?
        .with_context(|| format!("no folder `{name}` in {}", account.email))?;
    let after = cursor_after(client, args.after).await?;
    let messages = client.list_messages(folder.id, args.limit, after).await?;
    if json {
        print_messages(&messages)
    } else if messages.is_empty() {
        println!("No messages in {}.", printable(&folder.name));
        Ok(())
    } else {
        let folders = account_folders(client, [account.id]).await?;
        print_message_table(&messages, &folders, Some(folder.id));
        Ok(())
    }
}

pub(crate) async fn search(client: &MailClient, args: Search, json: bool) -> Result<()> {
    let mut query = SearchQuery::new(args.query.join(" "));
    query.limit = args.limit;
    query.after = cursor_after(client, args.after).await?;
    if let Some(name) = &args.folder {
        let account = one_account(client, args.account.as_deref()).await?;
        let folder = client
            .find_folder(account.id, name)
            .await?
            .with_context(|| format!("no folder `{name}` in {}", account.email))?;
        query.account = Some(account.id);
        query.folder = Some(folder.id);
    } else if let Some(selector) = &args.account {
        query.account = Some(resolve_account(client, selector).await?.id);
    }
    let messages = client.search(query).await?;
    if json {
        return print_messages(&messages);
    }
    if messages.is_empty() {
        println!("No matches.");
        return Ok(());
    }
    let accounts: BTreeSet<AccountId> = messages.iter().map(|m| m.account_id).collect();
    let folders = account_folders(client, accounts).await?;
    print_message_table(&messages, &folders, None);
    Ok(())
}

fn print_messages(messages: &[MessageSummary]) -> Result<()> {
    json::print_list::<JsonSummary, _>("messages", messages)
}
