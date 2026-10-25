//! `catmail mark`, `mv`, `archive`, `rm`, `label` and `ops`: changes to messages, made locally and then on
//! the server at once, like `catmail send`. What can't be made on the server waits in
//! the queue for the next `catmail sync` or `catmail ops flush`.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use catmail_core::{AccountId, Flags, FolderId, MailClient, MessageId, PendingOpId, ReplayReport};
use clap::{ArgGroup, Args, Subcommand};

use crate::cmd::folder_names;
use crate::exit::{Exit, Outcome};
use crate::json::{self, JsonPendingOp, JsonReplay};
use crate::output::print_ops;

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
    /// The folder they are moved out of: on Gmail, a label they lose [default: where
    /// they are]
    #[arg(long)]
    from: Option<String>,
}

#[derive(Args)]
#[command(group(ArgGroup::new("labels").required(true).multiple(true)))]
pub(crate) struct Label {
    /// Message ids, as shown by `catmail ls`
    #[arg(required = true)]
    ids: Vec<i64>,
    /// Gmail label to add, by folder name (e.g. Work)
    #[arg(long, group = "labels")]
    add: Vec<String>,
    /// Gmail label to take off, by folder name (e.g. INBOX)
    #[arg(long, group = "labels")]
    remove: Vec<String>,
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
    // Resolve every name first, so an unknown one in one account changes nothing.
    let mut moves = Vec::new();
    for (account, ids) in messages {
        let to = find_folder(client, account, &args.to).await?;
        let from = match &args.from {
            Some(name) => Some(find_folder(client, account, name).await?),
            None => None,
        };
        moves.push((account, ids, from, to));
    }
    for (_, ids, from, to) in &moves {
        client.move_messages(ids.clone(), *from, *to).await?;
    }
    replay(client, moves.into_iter().map(|(account, ..)| account), json).await
}

pub(crate) async fn label(client: &MailClient, args: Label, json: bool) -> Result<()> {
    let messages = by_account(client, &args.ids).await?;
    let mut changes = Vec::new();
    for (account, ids) in messages {
        let (mut add, mut remove) = (Vec::new(), Vec::new());
        for (names, ids) in [(&args.add, &mut add), (&args.remove, &mut remove)] {
            for name in names {
                ids.push(find_folder(client, account, name).await?);
            }
        }
        changes.push((account, ids, add, remove));
    }
    for (_, ids, add, remove) in &changes {
        client
            .label_messages(ids.clone(), add.clone(), remove.clone())
            .await?;
    }
    replay(
        client,
        changes.into_iter().map(|(account, ..)| account),
        json,
    )
    .await
}

/// The account's folder of that name; fails if there is none.
async fn find_folder(client: &MailClient, account: AccountId, name: &str) -> Result<FolderId> {
    Ok(client
        .find_folder(account, name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no folder `{name}` in account {account}"))?
        .id)
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
                json::print_list::<JsonPendingOp, _>("ops", &ops)?;
            } else {
                let accounts: BTreeSet<AccountId> = ops.iter().map(|op| op.account_id).collect();
                print_ops(&ops, &folder_names(client, accounts).await?);
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
/// change was made, saying why; with [`Outcome::Queued`] if nothing was refused.
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
    let mut outcome = Outcome::Queued;
    for (account, replayed) in replays.join_all().await {
        match replayed {
            Ok(report) => {
                // The server wasn't reached, or not to the end: the changes left wait
                // for the next try. What it refused before is counted below.
                if let Some(e) = &report.error {
                    if !e.transient {
                        outcome = Outcome::Failed;
                    }
                    errors.push(format!("account {account}: {e}"));
                }
                results.push((account, report));
            }
            Err(e) => {
                outcome = Outcome::Failed;
                errors.push(format!("account {account}: {e}"));
            }
        }
    }
    results.sort_by_key(|(account, _)| *account);
    if json {
        let accounts: Vec<_> = results
            .iter()
            .map(|(account, r)| JsonReplay::new(account.0, r))
            .collect();
        json::print_keyed("accounts", &accounts)?;
    }
    let failed: u32 = results.iter().map(|(_, r)| r.failed).sum();
    let pending: u32 = results.iter().map(|(_, r)| r.pending).sum();
    if failed > 0 {
        outcome = Outcome::Failed;
        errors.push(format!(
            "the server refused {failed} change(s), which were undone; `catmail ops` shows why"
        ));
    }
    let drafts_failed: u32 = results.iter().map(|(_, r)| r.drafts_failed).sum();
    if drafts_failed > 0 {
        outcome = Outcome::Failed;
        errors.push(refused_drafts(drafts_failed));
    }
    if !errors.is_empty() || pending > 0 {
        errors.push(format!(
            "changes not made on the server yet are kept, and made by {NEXT_FLUSH}"
        ));
    }
    if !errors.is_empty() {
        return Err(Exit::new(outcome, errors.join("\n")).into());
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

#[cfg(test)]
mod tests {
    use catmail_testkit::{FakeImap, FakeMailbox, ServerState};

    use super::*;
    use crate::test_support::synced;

    /// A client synced with INBOX ("one", "two") and Archive, and the INBOX messages' ids.
    async fn synced_inbox() -> (FakeImap, tempfile::TempDir, MailClient, AccountId, Vec<i64>) {
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
        (fake, dir, client, account, ids)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn changes_reach_the_server_or_stay_queued() {
        let (fake, _dir, client, account, ids) = synced_inbox().await;
        let inbox = client.find_folder(account, "INBOX").await.unwrap().unwrap();

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
            from: None,
        };
        let err = move_messages(&client, to("Nowhere"), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Nowhere"), "{err}");
        assert_eq!(crate::exit::outcome(&err), Outcome::Failed);
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
        assert_eq!(crate::exit::outcome(&err), Outcome::Queued);
        assert!(
            client
                .list_messages(inbox.id, 10, None)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(client.list_pending(Some(account)).await.unwrap().len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refusal_before_a_lost_connection_is_a_failure() {
        let (fake, _dir, client, _, ids) = synced_inbox().await;
        let to_archive = Move {
            ids: ids[..1].to_vec(),
            to: "Archive".into(),
            from: None,
        };
        move_messages(&client, to_archive, false).await.unwrap();

        // INBOX refuses its change, then the connection is lost before Archive's: the
        // refusal is final, so running the command again won't do.
        {
            let mut state = fake.state();
            state.faults.refuse_next_changes = 1;
            state.faults.drop_before_change = true;
            state.drop_open_connections();
        }
        let flag = Mark {
            ids,
            read: false,
            unread: false,
            flag: true,
            unflag: false,
        };
        let err = mark(&client, flag, false).await.unwrap_err();
        assert_eq!(crate::exit::outcome(&err), Outcome::Failed, "{err:#}");
        assert!(err.to_string().contains("refused 1 change"), "{err:#}");
    }
}
