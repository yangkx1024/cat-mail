//! `catmail mark`, `mv`, `archive`, `rm` and `ops`: changes to messages, made locally and then on
//! the server at once, like `catmail send`. What can't be made on the server waits in
//! the queue for the next `catmail sync` or `catmail ops flush`.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use catmail_core::{
    AccountId, Flags, FolderId, MailClient, MessageId, OpKind, OpStatus, PendingOp, PendingOpId,
    ReplayReport,
};
use clap::{ArgGroup, Args, Subcommand};

use crate::folder_names;
use crate::output::{print_table, truncate};

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).multiple(true)))]
pub(crate) struct Mark {
    /// Message ids, as shown by `catmail ls`
    #[arg(required = true)]
    ids: Vec<i64>,
    #[arg(long, group = "change", conflicts_with = "unread")]
    read: bool,
    #[arg(long, group = "change")]
    unread: bool,
    /// Flag (star) the messages
    #[arg(long, group = "change", conflicts_with = "unflag")]
    flag: bool,
    #[arg(long, group = "change")]
    unflag: bool,
}

#[derive(Args)]
pub(crate) struct Move {
    /// Message ids, as shown by `catmail ls`
    #[arg(required = true)]
    ids: Vec<i64>,
    /// Folder name (e.g. Archive), in the messages' account
    #[arg(long)]
    to: String,
}

#[derive(Args)]
pub(crate) struct Archive {
    /// Message ids, as shown by `catmail ls`
    #[arg(required = true)]
    ids: Vec<i64>,
}

#[derive(Args)]
pub(crate) struct Remove {
    /// Message ids, as shown by `catmail ls`
    #[arg(required = true)]
    ids: Vec<i64>,
    /// Delete for good instead of moving to Trash (messages in Trash always are)
    #[arg(long)]
    permanent: bool,
}

#[derive(Subcommand)]
pub(crate) enum OpsCommand {
    /// List changes not made on the server yet, and those it refused
    List,
    /// Make the queued changes on the server now
    Flush,
    /// Forget refused or conflicting changes
    Dismiss {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
}

pub(crate) async fn mark(client: &MailClient, args: Mark, json: bool) -> Result<()> {
    let (mut add, mut remove) = (Flags::empty(), Flags::empty());
    for (on, off, flag) in [
        (args.read, args.unread, Flags::SEEN),
        (args.flag, args.unflag, Flags::FLAGGED),
    ] {
        if on {
            add.insert(flag);
        }
        if off {
            remove.insert(flag);
        }
    }
    let accounts = client
        .set_flags(message_ids(&args.ids), add, remove)
        .await?;
    replay(client, accounts, json).await
}

pub(crate) async fn move_messages(client: &MailClient, args: Move, json: bool) -> Result<()> {
    let messages = by_account(client, &args.ids).await?;
    // Resolve every target first, so an unknown name in one account changes nothing.
    let mut moves = Vec::new();
    for (account, ids) in messages {
        let folder = client
            .find_folder(account, &args.to)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no folder `{}` in account {account}", args.to))?;
        moves.push((account, ids, folder.id));
    }
    for (_, ids, folder) in &moves {
        client.move_messages(ids.clone(), *folder).await?;
    }
    replay(client, moves.into_iter().map(|(account, ..)| account), json).await
}

pub(crate) async fn archive(client: &MailClient, args: Archive, json: bool) -> Result<()> {
    let accounts = client.archive_messages(message_ids(&args.ids)).await?;
    replay(client, accounts, json).await
}

pub(crate) async fn remove(client: &MailClient, args: Remove, json: bool) -> Result<()> {
    let accounts = client
        .delete_messages(message_ids(&args.ids), args.permanent)
        .await?;
    replay(client, accounts, json).await
}

pub(crate) async fn ops(client: &MailClient, cmd: OpsCommand, json: bool) -> Result<()> {
    match cmd {
        OpsCommand::List => {
            let ops = client.list_pending(None).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&ops)?);
            } else {
                print_ops(client, &ops).await?;
            }
            Ok(())
        }
        OpsCommand::Flush => {
            let accounts = client.list_accounts().await?;
            replay(client, accounts.into_iter().map(|a| a.id), json).await
        }
        OpsCommand::Dismiss { ids } => {
            for id in ids {
                client.dismiss_pending(PendingOpId(id)).await?;
            }
            Ok(())
        }
    }
}

fn message_ids(ids: &[i64]) -> Vec<MessageId> {
    ids.iter().copied().map(MessageId).collect()
}

/// The messages grouped by account, which each name their target folder in; fails if
/// one doesn't exist.
async fn by_account(
    client: &MailClient,
    ids: &[i64],
) -> Result<BTreeMap<AccountId, Vec<MessageId>>> {
    let mut out: BTreeMap<AccountId, Vec<MessageId>> = BTreeMap::new();
    for &id in ids {
        let message = client
            .get_message(MessageId(id))
            .await?
            .ok_or_else(|| anyhow::anyhow!("message {id} not found"))?;
        out.entry(message.summary.account_id)
            .or_default()
            .push(message.summary.id);
    }
    Ok(out)
}

/// Makes the accounts' queued changes on the server, all at once. Fails unless every
/// change was made, saying why.
async fn replay(
    client: &MailClient,
    accounts: impl IntoIterator<Item = AccountId>,
    json: bool,
) -> Result<()> {
    let mut replays = tokio::task::JoinSet::new();
    for account in accounts {
        let client = client.clone();
        replays.spawn(async move { (account, client.replay_pending(account).await) });
    }
    let (mut results, mut errors): (Vec<(AccountId, ReplayReport)>, _) = (Vec::new(), Vec::new());
    for (account, replayed) in replays.join_all().await {
        match replayed {
            Ok(report) => results.push((account, report)),
            Err(e) => errors.push(format!("account {account}: {e}")),
        }
    }
    results.sort_by_key(|(account, _)| *account);
    if json {
        let results: Vec<_> = results
            .iter()
            .map(|(account, r)| {
                serde_json::json!({
                    "account": account,
                    "applied": r.applied,
                    "failed": r.failed,
                    "pending": r.pending,
                    "drafts_uploaded": r.drafts_uploaded,
                    "drafts_failed": r.drafts_failed,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&results)?);
    }
    let failed: u32 = results.iter().map(|(_, r)| r.failed).sum();
    let pending: u32 = results.iter().map(|(_, r)| r.pending).sum();
    if failed > 0 {
        errors.push(format!(
            "the server refused {failed} change(s), which were undone; `catmail ops` shows why"
        ));
    }
    let drafts_failed: u32 = results.iter().map(|(_, r)| r.drafts_failed).sum();
    if drafts_failed > 0 {
        errors.push(refused_drafts(drafts_failed));
    }
    if !errors.is_empty() || pending > 0 {
        errors.push(format!(
            "changes not made on the server yet are kept, and made by {NEXT_FLUSH}"
        ));
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }
    Ok(())
}

/// What makes on the server whatever couldn't be made there yet.
pub(crate) const NEXT_FLUSH: &str = "the next `catmail sync` or `catmail ops flush`";

pub(crate) fn refused_drafts(count: u32) -> String {
    format!(
        "the server refused to store {count} draft(s) in its Drafts folder; \
         `catmail draft show` says why"
    )
}

async fn print_ops(client: &MailClient, ops: &[PendingOp]) -> Result<()> {
    if ops.is_empty() {
        println!("No changes waiting for the server.");
        return Ok(());
    }
    let accounts: BTreeSet<AccountId> = ops.iter().map(|op| op.account_id).collect();
    let folders = folder_names(client, accounts).await?;
    let name = |id: FolderId| folders.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let rows = ops
        .iter()
        .map(|op| {
            vec![
                op.id.to_string(),
                op.status.to_string(),
                change(op.kind, &name),
                name(op.folder_id),
                truncate(op.subject.as_deref().unwrap_or_default(), 30),
                match op.status {
                    OpStatus::Conflict => "folder was rebuilt on the server".into(),
                    _ => op.last_error.clone().unwrap_or_default(),
                },
            ]
        })
        .collect();
    print_table(
        &["ID", "STATUS", "CHANGE", "FOLDER", "SUBJECT", "LAST ERROR"],
        rows,
    );
    Ok(())
}

fn change(kind: OpKind, folder: &impl Fn(FolderId) -> String) -> String {
    match kind {
        OpKind::Flags { add, remove } => {
            let names = [
                (Flags::SEEN, "read"),
                (Flags::FLAGGED, "flagged"),
                (Flags::ANSWERED, "answered"),
            ];
            let mut parts = Vec::new();
            for (flag, name) in names {
                if add.contains(flag) {
                    parts.push(format!("+{name}"));
                }
                if remove.contains(flag) {
                    parts.push(format!("-{name}"));
                }
            }
            parts.join(" ")
        }
        OpKind::Move { to } => format!("move to {}", folder(to)),
        OpKind::Delete => "delete".into(),
    }
}

#[cfg(test)]
mod tests {
    use catmail_testkit::{FakeImap, FakeMailbox, ServerState};

    use super::*;
    use crate::test_support::synced;

    #[tokio::test(flavor = "multi_thread")]
    async fn changes_reach_the_server_or_stay_queued() {
        let mut state = ServerState::new("alice", "pw");
        state.inbox().append_simple("one");
        state.inbox().append_simple("two");
        state.mailboxes.push(FakeMailbox::new("Archive"));
        let fake = FakeImap::start(state).await;
        let dir = tempfile::tempdir().unwrap();
        let (client, account) = synced(&fake, dir.path()).await;
        let inbox = client.find_folder(account, "INBOX").await.unwrap().unwrap();
        let ids: Vec<i64> = client
            .list_messages(inbox.id, 10, None)
            .await
            .unwrap()
            .iter()
            .map(|m| m.id.0)
            .collect();

        let read = |ids: Vec<i64>| Mark {
            ids,
            read: true,
            unread: false,
            flag: false,
            unflag: false,
        };
        mark(&client, read(ids.clone()), false).await.unwrap();
        assert!(
            fake.state()
                .inbox()
                .messages
                .iter()
                .all(|m| m.flags == ["\\Seen"])
        );

        // An unknown folder changes nothing.
        let to = |to: &str| Move {
            ids: ids.clone(),
            to: to.into(),
        };
        let err = move_messages(&client, to("Nowhere"), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Nowhere"), "{err}");
        assert_eq!(
            client
                .list_messages(inbox.id, 10, None)
                .await
                .unwrap()
                .len(),
            2
        );

        // Without the server, the change is kept for later, and the command says so.
        fake.state().drop_open_connections();
        drop(fake);
        let err = move_messages(&client, to("Archive"), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("catmail ops flush"), "{err}");
        assert!(
            client
                .list_messages(inbox.id, 10, None)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(client.list_pending(Some(account)).await.unwrap().len(), 2);
    }
}
