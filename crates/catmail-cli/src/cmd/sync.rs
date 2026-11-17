//! `catmail sync`: folders and message headers from the server, once.

use anyhow::{Result, bail};
use catmail_core::{MailClient, SyncReport};

use crate::cmd::ops::{refused_changes, refused_drafts};
use crate::cmd::resolve_account;
use crate::json::{self, JsonAccountSync};
use crate::output::{ALL_MAIL_HIDDEN, print_sync_report, printable};

pub(crate) async fn sync(client: &MailClient, selector: Option<&str>, json: bool) -> Result<()> {
    let accounts = match selector {
        Some(selector) => vec![resolve_account(client, selector).await?],
        None => client.list_accounts().await?,
    };
    if accounts.is_empty() {
        bail!("no accounts; add one with `catmail account add`");
    }
    let (mut synced, mut reports) = (Vec::new(), Vec::new());
    for account in &accounts {
        if !json {
            println!("Syncing {}…", account.email);
        }
        let result = client.sync_account(account.id).await;
        match &result {
            Ok(report) if !json => print_sync_report(report),
            Ok(_) => {}
            Err(e) => eprintln!("{}: {}", account.email, printable(&e.to_string())),
        }
        // As the sync found the server.
        let account = client
            .get_account(account.id)
            .await?
            .unwrap_or_else(|| account.clone());
        if !json && account.all_mail_hidden() {
            println!("  {ALL_MAIL_HIDDEN}");
        }
        synced.push(JsonAccountSync::new(&account, result.as_ref()));
        reports.extend(result.ok());
    }
    if json {
        json::print_keyed("accounts", &synced)?;
    }
    // What the server refused stays refused when the sync runs again, and a script that
    // relied on `sync` to make its queued changes must hear of it, as from `ops flush`.
    let mut errors = Vec::new();
    let failed = accounts.len() - reports.len();
    if failed > 0 {
        errors.push(format!(
            "{failed} of {} accounts failed to sync",
            accounts.len()
        ));
    }
    let folders = reports
        .iter()
        .flat_map(|r| &r.folders)
        .filter(|f| f.error.is_some())
        .count();
    if folders > 0 {
        errors.push(format!("{folders} folder(s) failed to sync"));
    }
    let changes: u32 = reports.iter().map(SyncReport::changes_refused).sum();
    if changes > 0 {
        errors.push(refused_changes(changes));
    }
    let drafts: u32 = reports.iter().map(|r| r.drafts_failed).sum();
    if drafts > 0 {
        errors.push(refused_drafts(drafts));
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use catmail_core::{Address, DraftBody, DraftContent, Flags};
    use catmail_testkit::{FakeImap, FakeMailbox, ServerState};

    use super::*;
    use crate::exit::{Outcome, outcome};
    use crate::test_support::synced;

    fn fails_with(result: Result<()>, needle: &str) {
        let err = result.unwrap_err();
        assert_eq!(outcome(&err), Outcome::Failed);
        assert!(err.to_string().contains(needle), "{err}");
    }

    /// Each thing the server refused fails the sync, though the account synced.
    #[tokio::test(flavor = "multi_thread")]
    async fn refusals_fail_the_sync() {
        let mut state = ServerState::new("alice", "pw");
        state.inbox().append_simple("one");
        state
            .mailboxes
            .push(FakeMailbox::new("Drafts").with_attribute("\\Drafts"));
        let fake = FakeImap::start(state).await;
        let dir = tempfile::tempdir().unwrap();
        let (client, account) = synced(&fake, dir.path()).await;
        let sync = async || sync(&client, None, true).await;
        sync().await.unwrap();

        // A change the server refuses, undone.
        let inbox = client.find_folder(account, "INBOX").await.unwrap().unwrap();
        let one = client.list_messages(inbox.id, 10, None).await.unwrap()[0].id;
        fake.state().faults.refuse_changes = Some("[CANNOT] Permission denied".into());
        client
            .set_flags(vec![one], Flags::FLAGGED, Flags::empty())
            .await
            .unwrap();
        fails_with(sync().await, "refused 1 change(s)");
        fake.state().faults.refuse_changes = None;
        sync().await.unwrap();

        // A draft the server refuses to store.
        let content = DraftContent {
            subject: "draft".into(),
            to: vec![Address {
                name: None,
                email: "bob@example.com".into(),
            }],
            body: DraftBody::plain("text"),
            ..Default::default()
        };
        fake.state().faults.reject_appends = 1;
        client
            .create_draft(account, None, content, Vec::new())
            .await
            .unwrap();
        fails_with(sync().await, "store 1 draft(s)");
        sync().await.unwrap();

        // A folder the server answers about in a way no parser accepts.
        {
            let mut state = fake.state();
            state.inbox().append_simple("two"); // so the sync asks
            state.faults.garble = Some(" EXISTS".into());
        }
        fails_with(sync().await, "1 folder(s) failed");
        sync().await.unwrap();
    }
}
