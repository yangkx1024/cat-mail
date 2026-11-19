//! `catmail ls` and `catmail search`: folders, and pages of messages.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use catmail_core::{
    AccountId, FlagFilter, ListFilter, ListQuery, MailClient, MessageSummary, SearchQuery,
};
use clap::{Args, ValueEnum};

use crate::cmd::{account_folders, cursor_after, one_account, resolve_account};
use crate::json::{self, JsonFolder, JsonSummary};
use crate::output::{ALL_MAIL_HIDDEN, outln, print_folders, print_message_table, printable};

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
    /// Only the unread, read, flagged or unflagged messages
    #[arg(long, value_enum)]
    only: Option<Only>,
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
    /// Also ask the servers, for the text of messages whose bodies aren't downloaded
    #[arg(long)]
    server: bool,
    /// Only the unread, read, flagged or unflagged matches
    #[arg(long, value_enum)]
    only: Option<Only>,
}

/// Which messages `--only` lists, by their flags.
#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Only {
    Unread,
    Read,
    Flagged,
    Unflagged,
}

impl Only {
    fn filter(only: Option<Self>) -> ListFilter {
        ListFilter::new(match only {
            None => FlagFilter::All,
            Some(Self::Unread) => FlagFilter::Unread,
            Some(Self::Read) => FlagFilter::Read,
            Some(Self::Flagged) => FlagFilter::Flagged,
            Some(Self::Unflagged) => FlagFilter::Unflagged,
        })
    }

    /// "unread " and so on, for "No unread messages": nothing without `--only`.
    fn word(only: Option<Self>) -> String {
        only.and_then(|o| o.to_possible_value())
            .map_or_else(String::new, |v| format!("{} ", v.get_name()))
    }
}

pub(crate) async fn ls(client: &MailClient, args: Ls, json: bool) -> Result<()> {
    let account = one_account(client, args.account.as_deref()).await?;
    let Some(name) = args.folder else {
        let folders = client.list_folders(account.id).await?;
        if json {
            json::print_list::<JsonFolder, _>("folders", &folders)?;
        } else if folders.is_empty() {
            outln!("No folders yet. Run `catmail sync` first.")?;
        } else {
            print_folders(&folders)?;
            if account.all_mail_hidden() {
                outln!("\n{ALL_MAIL_HIDDEN}")?;
            }
        }
        return Ok(());
    };
    let folder = client
        .find_folder(account.id, &name)
        .await?
        .with_context(|| format!("no folder `{name}` in {}", account.email))?;
    let query = ListQuery {
        limit: args.limit,
        after: cursor_after(client, args.after).await?,
        filter: Only::filter(args.only),
    };
    let messages = client.list_messages(folder.id, query).await?;
    if json {
        print_messages(&messages)
    } else if messages.is_empty() {
        let only = Only::word(args.only);
        outln!("No {only}messages in {}.", printable(&folder.name))?;
        Ok(())
    } else {
        let folders = account_folders(client, [account.id]).await?;
        print_message_table(&messages, &folders, Some(folder.id))?;
        Ok(())
    }
}

pub(crate) async fn search(client: &MailClient, args: Search, json: bool) -> Result<()> {
    let mut query = SearchQuery::new(args.query.join(" "));
    query.limit = args.limit;
    query.after = cursor_after(client, args.after).await?;
    query.filter = Only::filter(args.only);
    if let Some(name) = &args.folder {
        let account = one_account(client, args.account.as_deref()).await?;
        let folder = client
            .find_folder(account.id, name)
            .await?
            .with_context(|| format!("no folder `{name}` in {}", account.email))?;
        query.accounts = Some(vec![account.id]);
        query.folder = Some(folder.id);
    } else if let Some(selector) = &args.account {
        query.accounts = Some(vec![resolve_account(client, selector).await?.id]);
    }
    if args.server {
        query.found_on_server = client.search_server(query.clone()).await?;
    }
    let messages = client.search(query).await?;
    if json {
        return print_messages(&messages);
    }
    if messages.is_empty() {
        outln!("No {}matches.", Only::word(args.only))?;
        return Ok(());
    }
    let accounts: BTreeSet<AccountId> = messages.iter().map(|m| m.account_id).collect();
    let folders = account_folders(client, accounts).await?;
    print_message_table(&messages, &folders, None)?;
    Ok(())
}

fn print_messages(messages: &[MessageSummary]) -> Result<()> {
    json::print_list::<JsonSummary, _>("messages", messages)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Cmd {
        #[command(flatten)]
        ls: Ls,
    }

    fn only(args: &[&str]) -> Result<Option<Only>, clap::Error> {
        let args = ["ls", "INBOX"].iter().chain(args);
        Ok(Cmd::try_parse_from(args)?.ls.only)
    }

    #[test]
    fn only_lists_the_messages_with_some_flags() {
        let filter = |args: &[&str]| Only::filter(only(args).unwrap()).flags;
        assert_eq!(filter(&[]), FlagFilter::All);
        assert_eq!(filter(&["--only", "unread"]), FlagFilter::Unread);
        assert_eq!(filter(&["--only", "read"]), FlagFilter::Read);
        assert_eq!(filter(&["--only", "flagged"]), FlagFilter::Flagged);
        assert_eq!(filter(&["--only", "unflagged"]), FlagFilter::Unflagged);
        assert!(only(&["--only", "starred"]).is_err());

        assert_eq!(Only::word(None), "");
        assert_eq!(Only::word(Some(Only::Unflagged)), "unflagged ");
    }
}
