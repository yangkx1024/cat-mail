//! `catmail sync`: folders and message headers from the server, once.

use anyhow::{Result, bail};
use catmail_core::MailClient;

use crate::cmd::resolve_account;
use crate::json::{self, JsonAccountSync, JsonError};
use crate::output::{print_sync_report, printable};

pub(crate) async fn sync(client: &MailClient, selector: Option<&str>, json: bool) -> Result<()> {
    let accounts = match selector {
        Some(selector) => vec![resolve_account(client, selector).await?],
        None => client.list_accounts().await?,
    };
    if accounts.is_empty() {
        bail!("no accounts; add one with `catmail account add`");
    }
    let mut synced = Vec::new();
    for account in &accounts {
        if !json {
            println!("Syncing {}…", account.email);
        }
        let result = match client.sync_account(account.id).await {
            Ok(report) => {
                if !json {
                    print_sync_report(&report);
                }
                Ok(report)
            }
            Err(e) => {
                eprintln!("{}: {}", account.email, printable(&e.to_string()));
                Err(JsonError::new(e.kind(), e.to_string()))
            }
        };
        synced.push(JsonAccountSync::new(account, result));
    }
    if json {
        json::print_keyed("accounts", &synced)?;
    }
    let failed = synced.iter().filter(|a| a.failed()).count();
    if failed > 0 {
        bail!("{failed} of {} accounts failed to sync", accounts.len());
    }
    Ok(())
}
