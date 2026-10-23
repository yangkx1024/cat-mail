//! `catmail watch`: background sync, in the foreground.

use std::collections::HashMap;
use std::future::Future;

use anyhow::{Result, bail};
use catmail_core::{AccountId, Event, MailClient, RecvError};

use crate::json::{self, JsonEvent};
use crate::output::printable;

/// Prints what changes while background sync keeps every account up to date, until
/// `stop` resolves. The client must have been built with background sync.
pub(crate) async fn watch(
    client: &MailClient,
    json: bool,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    // Background sync started with the client, so the first few changes may come before
    // this; they only go unprinted.
    let mut events = client.subscribe();
    let accounts = client.list_accounts().await?;
    if accounts.is_empty() {
        bail!("no accounts; add one with `catmail account add`");
    }
    if !json {
        let emails: Vec<_> = accounts.iter().map(|a| printable(&a.email)).collect();
        eprintln!("Watching {}; press Ctrl-C to stop", emails.join(", "));
    }
    // No account can be added or removed meanwhile: this process holds the data directory.
    let emails: HashMap<AccountId, String> =
        accounts.into_iter().map(|a| (a.id, a.email)).collect();
    let mut stop = std::pin::pin!(stop);
    loop {
        let event = tokio::select! {
            event = events.recv() => event,
            () = &mut stop => return Ok(()),
        };
        let mut batch = match event {
            Ok(event) => vec![event],
            // Only lines are lost; what they would say is in the database.
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return Ok(()),
        };
        // A sync announces each batch of messages it stores: one line per folder for
        // whatever has piled up.
        while let Ok(event) = events.try_recv() {
            if !batch.contains(&event) {
                batch.push(event);
            }
        }
        for event in batch {
            if json {
                json::print_line(&JsonEvent::from(&event))?;
            } else if let Some(line) = describe(client, &emails, &event).await? {
                let time = jiff::Zoned::now().strftime("%H:%M:%S");
                println!("{time} {line}");
            }
        }
    }
}

/// A line about the event, for those worth one.
async fn describe(
    client: &MailClient,
    emails: &HashMap<AccountId, String>,
    event: &Event,
) -> Result<Option<String>> {
    let email = |account| printable(emails.get(&account).map_or("?", String::as_str));
    Ok(match *event {
        Event::MessagesChanged { account, folder } => {
            let Some(folder) = client.get_folder(folder).await? else {
                return Ok(None);
            };
            Some(format!(
                "{} {}: {} messages, {} unread",
                email(account),
                printable(&folder.name),
                folder.total_count,
                folder.unread_count
            ))
        }
        Event::FoldersChanged { account } => Some(format!("{}: folders changed", email(account))),
        // Sending happens in other commands, so only syncing.
        Event::AccountStatusChanged { account } => {
            Some(match client.account_status(account).sync {
                Some(problem) => format!("{}: can't sync: {}", email(account), problem.error),
                None => format!("{}: syncing", email(account)),
            })
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use catmail_testkit::{FakeImap, ServerState};

    use super::*;
    use crate::test_support::synced;

    #[tokio::test(flavor = "multi_thread")]
    async fn describes_changes() {
        let mut state = ServerState::new("alice", "pw");
        state.inbox().append_simple("one");
        state.inbox().append_simple("two");
        state.inbox().set_flags(1, &["\\Seen"]);
        let fake = FakeImap::start(state).await;
        let dir = tempfile::tempdir().unwrap();
        let (client, account) = synced(&fake, dir.path()).await;
        let account = client.get_account(account).await.unwrap().unwrap();
        let inbox = client
            .find_folder(account.id, "INBOX")
            .await
            .unwrap()
            .unwrap();
        let emails = HashMap::from([(account.id, account.email)]);

        let changed = Event::MessagesChanged {
            account: account.id,
            folder: inbox.id,
        };
        assert_eq!(
            describe(&client, &emails, &changed)
                .await
                .unwrap()
                .as_deref(),
            Some("alice@example.com INBOX: 2 messages, 1 unread")
        );
        let sent = Event::OutboxChanged {
            account: account.id,
        };
        assert_eq!(describe(&client, &emails, &sent).await.unwrap(), None);

        fake.state().faults.reject_login = true;
        assert!(client.sync_account(account.id).await.is_err());
        let status = Event::AccountStatusChanged {
            account: account.id,
        };
        let line = describe(&client, &emails, &status).await.unwrap().unwrap();
        assert!(
            line.starts_with("alice@example.com: can't sync: login rejected"),
            "{line}"
        );

        // Returns when told to.
        watch(&client, false, async {}).await.unwrap();
    }
}
