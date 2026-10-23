//! Sending: `send` and `outbox`. The command line doesn't send in the background, so
//! each of these works through the outbox there and then.

use std::collections::BTreeMap;

use anyhow::Result;
use catmail_core::{
    DraftId, FlushReport, MailClient, OutboxEntry, OutboxId, OutboxStatus, ReportedError,
};
use clap::Subcommand;

use crate::cmd::compose::save_to_server;
use crate::exit::{Exit, Outcome};
use crate::json::{self, JsonOutboxEntry};
use crate::output::{print_outbox, printable};

#[derive(Subcommand)]
pub(crate) enum OutboxCommand {
    /// List messages that are not sent and saved yet
    List,
    /// Send what is queued and save what was sent to Sent, now
    Flush,
    /// Try a failed or unknown send again (an unknown one may arrive twice), or saving
    /// a sent one to Sent
    Retry { id: i64 },
    /// Confirm that a message with an unknown outcome arrived; it is then only saved
    /// to Sent
    MarkSent { id: i64 },
    /// Remove an entry. A message that wasn't sent goes back to the drafts
    Cancel { id: i64 },
}

/// Queues the draft and works through its account's outbox, reporting how this
/// message fared.
pub(crate) async fn send(client: &MailClient, id: DraftId, json: bool) -> Result<()> {
    let queued = client.send_draft(id).await?;
    let account = queued.account_id;
    flush_and_report(client, queued, json).await?;
    // The draft's copy on the server goes now that it is sent.
    save_to_server(client, account).await;
    Ok(())
}

/// Works through the entry's outbox and tells the user how the entry fared.
async fn flush_and_report(client: &MailClient, entry: OutboxEntry, json: bool) -> Result<()> {
    let FlushReport { entries, errors } = client.flush_outbox(entry.account_id).await?;
    let entry = entries
        .into_iter()
        .find(|e| e.id == entry.id)
        .unwrap_or(entry);
    let blocked = errors.get(&entry.identity).filter(|e| blocks(e));
    report(&entry, blocked, json)
}

/// Whether why the flush couldn't send as an identity at all means trying again unchanged
/// won't help: its SMTP server rejected the login, or can't be used as it is set up. Its
/// queued messages are postponed then, as for a server that is down.
fn blocks(error: &ReportedError) -> bool {
    !error.transient
}

pub(crate) async fn outbox(client: &MailClient, cmd: OutboxCommand, json: bool) -> Result<()> {
    match cmd {
        OutboxCommand::List => {
            let entries = client.list_outbox(None).await?;
            if json {
                json::print_list::<JsonOutboxEntry, _>("entries", &entries)?;
            } else {
                print_outbox(&entries);
            }
        }
        OutboxCommand::Flush => {
            // All at once: one unreachable server shouldn't hold up the other accounts.
            let mut flushes = tokio::task::JoinSet::new();
            for account in client.list_accounts().await? {
                let client = client.clone();
                flushes.spawn(async move { (account.id, client.flush_outbox(account.id).await) });
            }
            // One account failing doesn't hide what happened to the others' mail.
            let (mut results, mut errors) = (Vec::new(), Vec::new());
            let mut cant_send = BTreeMap::new();
            for (account, flushed) in flushes.join_all().await {
                match flushed {
                    Ok(FlushReport { entries, errors }) => {
                        results.extend(entries);
                        let blocked = errors.into_iter().filter(|(_, e)| blocks(e));
                        cant_send.extend(blocked.map(|(identity, e)| (identity, (account, e))));
                    }
                    Err(e) => errors.push(e.to_string()),
                }
            }
            for (identity, (account, e)) in &cant_send {
                errors.push(format!(
                    "account {account} can't send as identity {identity}: {}",
                    printable(&e.message)
                ));
            }
            results.sort_by_key(|e| e.id);
            if json {
                json::print_list::<JsonOutboxEntry, _>("entries", &results)?;
            } else if !results.is_empty() {
                print_outbox(&results);
            } else if errors.is_empty() {
                println!("Nothing to send.");
            }
            // Like `catmail send`: an error unless everything went out.
            let failed = (!errors.is_empty()).then_some(Outcome::Failed);
            let unsent = results
                .iter()
                .filter_map(|e| unsent(e.status, cant_send.contains_key(&e.identity)))
                .max();
            if unsent.is_some() {
                errors.push("not everything was sent; `catmail outbox` shows why".into());
            }
            if let Some(outcome) = failed.max(unsent) {
                return Err(Exit::new(outcome, errors.join("\n")).into());
            }
        }
        OutboxCommand::Retry { id } => {
            let entry = client.retry_outbox(OutboxId(id)).await?;
            flush_and_report(client, entry, json).await?;
        }
        OutboxCommand::MarkSent { id } => {
            let entry = client.mark_outbox_sent(OutboxId(id)).await?;
            flush_and_report(client, entry, json).await?;
        }
        OutboxCommand::Cancel { id } => {
            let entry = client.cancel_outbox(OutboxId(id)).await?;
            if json {
                json::print_keyed("entry", &JsonOutboxEntry::from(&entry))?;
            } else if entry.status.went_out() {
                println!("Removed {id}; the message was sent but not saved to Sent.");
            } else if let Some(draft) = entry.draft_id {
                println!("Cancelled {id}; it is draft {draft} again.");
            } else {
                println!("Cancelled {id}.");
            }
        }
    }
    Ok(())
}

/// How a command ends for a message whose send ended in `status`, sent as an identity
/// that [`blocks`] or not; `None` if it went out.
fn unsent(status: OutboxStatus, blocked: bool) -> Option<Outcome> {
    Some(match status {
        _ if status.went_out() => return None,
        OutboxStatus::Failed => Outcome::Failed,
        OutboxStatus::Unknown => Outcome::Unknown,
        // Still to be sent, but not before the identity is fixed.
        _ if blocked => Outcome::Failed,
        _ => Outcome::Queued,
    })
}

/// Tells the user how a send went and what they can do; an error unless it went out.
/// `blocked` is why its identity can't send, if that [`blocks`].
fn report(entry: &OutboxEntry, blocked: Option<&ReportedError>, json: bool) -> Result<()> {
    if json {
        json::print_keyed("entry", &JsonOutboxEntry::from(entry))?;
    }
    let id = entry.id;
    let error = printable(entry.last_error.as_deref().unwrap_or("unknown error"));
    let message = match entry.status {
        OutboxStatus::Done => {
            if !json {
                println!("Sent.");
            }
            return Ok(());
        }
        OutboxStatus::Accepted => {
            eprintln!(
                "Sent, but saving a copy to Sent failed: {}\n\
                 `catmail outbox flush` tries again.",
                error
            );
            return Ok(());
        }
        OutboxStatus::Queued | OutboxStatus::Sending if let Some(e) = blocked => format!(
            "can't send: {}\nThe message waits in the outbox; `catmail outbox flush` sends it \
             once the account works, `catmail outbox cancel {id}` makes it a draft again.",
            printable(&e.message)
        ),
        OutboxStatus::Queued | OutboxStatus::Sending => format!(
            "not sent yet: {error}\n`catmail outbox flush` tries again; \
             `catmail outbox cancel {id}` makes it a draft again."
        ),
        OutboxStatus::Failed => format!(
            "the server refused the message: {error}\n\
             `catmail outbox cancel {id}` makes it a draft again to fix it; \
             `catmail outbox retry {id}` sends it as it is."
        ),
        OutboxStatus::Unknown => format!(
            "the connection was lost after the message was handed over, so it may or may \
             not have been delivered: {error}\n\
             Check with a recipient, then `catmail outbox mark-sent {id}` if it arrived, \
             `catmail outbox retry {id}` to send it again (it may arrive twice), or \
             `catmail outbox cancel {id}`."
        ),
    };
    let outcome =
        unsent(entry.status, blocked.is_some()).expect("only messages that didn't go out get here");
    Err(Exit::new(outcome, message).into())
}

#[cfg(test)]
mod tests {
    use catmail_core::{Address, DraftContent};
    use catmail_testkit::{FakeImap, FakeSmtp, ServerState, SmtpState};

    use super::*;
    use crate::exit::outcome;
    use crate::test_support::synced_with_smtp;

    #[tokio::test(flavor = "multi_thread")]
    async fn exit_code_says_how_a_send_ended() {
        let fake = FakeImap::start(ServerState::new("alice", "pw")).await;
        let smtp = FakeSmtp::start(SmtpState::new("alice", "pw")).await;
        let dir = tempfile::tempdir().unwrap();
        let (client, account) = synced_with_smtp(&fake, smtp.port(), dir.path()).await;
        let send_to = |to: &str| {
            let content = DraftContent {
                subject: "hi".into(),
                to: vec![Address {
                    name: None,
                    email: to.into(),
                }],
                ..DraftContent::default()
            };
            let client = client.clone();
            async move {
                let draft = client.create_draft(account, content).await.unwrap();
                send(&client, draft.id, false).await
            }
        };

        send_to("b@example.com").await.unwrap();

        smtp.state().faults.drop_after_data = 1;
        let err = send_to("b@example.com").await.unwrap_err();
        assert_eq!(outcome(&err), Outcome::Unknown, "{err:#}");

        smtp.state()
            .faults
            .reject_recipients
            .push(("nobody@example.com".into(), 550));
        let err = send_to("nobody@example.com").await.unwrap_err();
        assert_eq!(outcome(&err), Outcome::Failed, "{err:#}");

        smtp.state().faults.unavailable = 100;
        let err = send_to("b@example.com").await.unwrap_err();
        assert_eq!(outcome(&err), Outcome::Queued, "{err:#}");

        // A flush tries only the queued one: the others wait for the user.
        let err = outbox(&client, OutboxCommand::Flush, false)
            .await
            .unwrap_err();
        assert_eq!(outcome(&err), Outcome::Queued, "{err:#}");
        smtp.state().faults.unavailable = 0;
        outbox(&client, OutboxCommand::Flush, false).await.unwrap();

        // A login the server rejects postpones the message too, but trying again won't
        // help until the account is fixed.
        smtp.state().faults.reject_login = true;
        let err = send_to("b@example.com").await.unwrap_err();
        assert_eq!(outcome(&err), Outcome::Failed, "{err:#}");
        assert!(err.to_string().contains("can't send"), "{err:#}");
        let err = outbox(&client, OutboxCommand::Flush, false)
            .await
            .unwrap_err();
        assert_eq!(outcome(&err), Outcome::Failed, "{err:#}");
        smtp.state().faults.reject_login = false;
        outbox(&client, OutboxCommand::Flush, false).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_flush_says_fix_it_when_one_identity_needs_fixing() {
        let fake = FakeImap::start(ServerState::new("alice", "pw")).await;
        let smtp = FakeSmtp::start(SmtpState::new("alice", "pw")).await;
        let service = FakeSmtp::start(SmtpState::new("resend", "key")).await;
        let dir = tempfile::tempdir().unwrap();
        let (client, account) = synced_with_smtp(&fake, smtp.port(), dir.path()).await;
        let identity = client
            .add_identity(
                account,
                catmail_core::NewIdentity {
                    email: "news@example.org".into(),
                    display_name: None,
                    username: "resend".into(),
                    smtp: catmail_core::ServerConfig {
                        host: "127.0.0.1".into(),
                        port: service.port(),
                        security: catmail_core::Security::Plain,
                    },
                },
                "key".into(),
            )
            .await
            .unwrap();
        let queue = |identity| {
            let content = DraftContent {
                subject: "hi".into(),
                to: vec![Address {
                    name: None,
                    email: "b@example.com".into(),
                }],
                identity,
                ..DraftContent::default()
            };
            let client = client.clone();
            async move {
                let draft = client.create_draft(account, content).await.unwrap();
                client.send_draft(draft.id).await.unwrap();
            }
        };

        // The service rejects the login, then the account's own server is down for now:
        // trying again unchanged won't send everything.
        service.state().faults.reject_login = true;
        smtp.state().faults.unavailable = 100;
        queue(Some(identity.id)).await;
        queue(None).await;
        let err = outbox(&client, OutboxCommand::Flush, false)
            .await
            .unwrap_err();
        assert_eq!(outcome(&err), Outcome::Failed, "{err:#}");
        assert!(
            err.to_string()
                .contains(&format!("can't send as identity {}", identity.id)),
            "{err:#}"
        );
    }
}
