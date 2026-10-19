//! Read-only IMAP sync of folders and message headers.
//!
//! Per folder: EXAMINE, compare UIDVALIDITY, diff UIDs to find new and vanished
//! messages, refresh flags of known messages (only the changed ones with CONDSTORE),
//! then fetch new headers newest first in batches. Every batch is its own transaction,
//! so an interrupted sync keeps what it fetched and the next one only diffs again. The
//! folder's cursor (UIDNEXT, HIGHESTMODSEQ) is written last, once the folder matches
//! the server as of the EXAMINE.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use catmail_proto::imap::{FetchedMessage, ListedMailbox, MailboxStatus, Session};
use catmail_store::{FolderSyncState, NewMessage, RemoteFolder};
use catmail_types::{AccountId, Event, FolderId, FolderRole, FolderSyncReport, SyncReport};

use crate::engine::{Engine, blocking};
use crate::{Error, Result, auth};

/// Per network read or write, not per command.
const IO_TIMEOUT: Duration = Duration::from_secs(60);

/// New messages per UID FETCH, and per transaction.
const FETCH_BATCH: usize = 200;

pub(crate) async fn sync_account(engine: Arc<Engine>, account_id: AccountId) -> Result<SyncReport> {
    let mut permit = engine.sync_permit(account_id).await;
    tokio::select! {
        result = run(&engine, account_id) => result,
        () = permit.cancelled() => Err(Error::Cancelled),
    }
}

async fn run(engine: &Arc<Engine>, account_id: AccountId) -> Result<SyncReport> {
    let (account, secret_key) = engine
        .read(move |db| {
            let accounts = db.accounts();
            Ok(accounts
                .get(account_id)?
                .zip(accounts.secret_key(account_id)?))
        })
        .await?
        .ok_or_else(|| Error::AccountNotFound(account_id.to_string()))?;
    let credentials = {
        let (engine, account) = (engine.clone(), account.clone());
        blocking(move || auth::credentials(&account, &secret_key, engine.secrets.as_ref())).await?
    };

    let mut session = Session::connect(&account.imap, &engine.tls()?, IO_TIMEOUT).await?;
    session.login(&credentials).await?;

    let remote = remote_folders(session.list().await?);
    if engine
        .write(move |db| db.folders().reconcile(account_id, &remote))
        .await?
    {
        engine.emit(Event::FoldersChanged {
            account: account_id,
        });
    }

    let folders = engine
        .read(move |db| db.folders().sync_states(account_id))
        .await?;
    let mut report = SyncReport {
        account: account_id,
        folders: Vec::new(),
    };
    for folder in folders.into_iter().filter(|f| f.selectable) {
        let mut folder_report = FolderSyncReport::new(folder.id, &folder.name);
        match sync_folder(
            engine,
            &mut session,
            account_id,
            &folder,
            &mut folder_report,
        )
        .await
        {
            Ok(()) => {}
            // The server refused something about this folder (it may have been deleted
            // since LIST); the others can still sync.
            Err(Error::Imap(e)) if !e.is_fatal() => {
                tracing::warn!(folder = %folder.name, error = %e, "folder sync failed");
                folder_report.error = Some(e.to_string());
            }
            Err(e) => return Err(e),
        }
        report.folders.push(folder_report);
    }
    session.logout().await;
    Ok(report)
}

async fn sync_folder(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    folder: &FolderSyncState,
    report: &mut FolderSyncReport,
) -> Result<()> {
    let id = folder.id;
    let changed = || {
        engine.emit(Event::MessagesChanged {
            account,
            folder: id,
        })
    };
    let status = session.examine(&folder.remote_name).await?;

    let (mut uidnext, mut modseq) = (folder.uidnext, folder.highest_modseq);
    let (mut count, mut max_uid) = (folder.message_count, folder.max_uid);
    if folder.uidvalidity != Some(status.uidvalidity) {
        // UIDs of the old generation now name different messages (or nothing).
        let uidvalidity = status.uidvalidity;
        engine
            .write(move |db| db.folders().reset_uidvalidity(id, uidvalidity))
            .await?;
        report.uidvalidity_changed = folder.uidvalidity.is_some();
        if report.uidvalidity_changed {
            changed();
        }
        (uidnext, modseq, count, max_uid) = (None, None, 0, 0);
    }

    let (new, vanished) = new_and_vanished(engine, session, id, count, uidnext, &status).await?;

    if !vanished.is_empty() {
        report.removed = engine
            .write(move |db| Ok(db.messages().delete_uids(id, &vanished)? as u32))
            .await?;
        changed();
    }

    // Flags of messages we already had. With CONDSTORE only what changed since the last
    // complete sync, and nothing at all if HIGHESTMODSEQ hasn't moved.
    let unchanged = modseq.is_some() && modseq == status.highest_modseq;
    if count > report.removed && !unchanged {
        let since = status.highest_modseq.and(modseq);
        let states = session.fetch_flags(max_uid, since).await?;
        report.updated = engine
            .write(move |db| Ok(db.messages().update_flags(id, &states)? as u32))
            .await?;
        if report.updated > 0 {
            changed();
        }
    }

    // Newest first, so the top of the list fills in before the backlog.
    let mut complete = true;
    for batch in new.rchunks(FETCH_BATCH) {
        let messages: Vec<NewMessage> = session
            .fetch_summaries(batch)
            .await?
            .into_iter()
            .map(new_message)
            .collect();
        // Expunged since the SEARCH, or a response we couldn't parse.
        complete &= messages.len() == batch.len();
        report.added += engine
            .write(move |db| Ok(db.messages().upsert(account, id, &messages)? as u32))
            .await?;
        changed();
    }

    // The cursor promises that everything below it is stored; with a gap, leave the old
    // one so the next sync diffs in full.
    if complete {
        let (uidnext, modseq) = (status.uidnext, status.highest_modseq);
        engine
            .write(move |db| db.folders().set_synced(id, uidnext, modseq))
            .await?;
    }
    Ok(())
}

/// UIDs on the server but not stored, and stored but gone from the server. The `count`
/// stored messages are complete below `uidnext`, the cursor of the last complete sync.
/// Local and remote UID lists are only loaded as far as the answer needs them.
async fn new_and_vanished(
    engine: &Arc<Engine>,
    session: &mut Session,
    id: FolderId,
    count: u32,
    uidnext: Option<u32>,
    status: &MailboxStatus,
) -> Result<(Vec<u32>, Vec<u32>)> {
    // Nothing arrived since the cursor, and the count shows nothing was expunged.
    if uidnext.is_some() && uidnext == status.uidnext && count == status.exists {
        return Ok((Vec::new(), Vec::new()));
    }
    if let Some(uidnext) = uidnext {
        // Only mail at or above the cursor can be new. If it accounts for every message
        // the server has, nothing was expunged either, and the full UID lists (the bulk
        // of a large folder) are never needed.
        let stored = engine
            .read(move |db| db.messages().uids_from(id, uidnext))
            .await?;
        let new = sorted_difference(&session.uid_search_from(uidnext).await?, &stored);
        if count + new.len() as u32 == status.exists {
            return Ok((new, Vec::new()));
        }
    }
    let local = engine.read(move |db| db.messages().uids(id)).await?;
    let server = session.uid_search_all().await?;
    Ok((
        sorted_difference(&server, &local),
        sorted_difference(&local, &server),
    ))
}

/// Elements of `a` not in `b`; both ascending.
fn sorted_difference(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut b = b.iter().peekable();
    a.iter()
        .copied()
        .filter(|x| {
            while b.next_if(|y| *y < x).is_some() {}
            b.peek() != Some(&x)
        })
        .collect()
}

/// Assigns roles: INBOX by name, then SPECIAL-USE attributes, then well-known names for
/// roles no folder claimed via SPECIAL-USE.
fn remote_folders(listed: Vec<ListedMailbox>) -> Vec<RemoteFolder> {
    let claimed: HashSet<FolderRole> = listed.iter().filter_map(|m| m.special_use).collect();
    let mut guessed = HashSet::new();
    listed
        .into_iter()
        .map(|m| {
            let role = if m.raw_name.eq_ignore_ascii_case("INBOX") {
                Some(FolderRole::Inbox)
            } else if m.special_use.is_some() {
                m.special_use
            } else {
                guess_role(&m)
                    .filter(|r| m.selectable && !claimed.contains(r) && guessed.insert(*r))
            };
            RemoteFolder {
                remote_name: m.raw_name,
                name: m.name,
                delimiter: m.delimiter,
                role,
                selectable: m.selectable,
            }
        })
        .collect()
}

fn guess_role(mailbox: &ListedMailbox) -> Option<FolderRole> {
    let leaf = match mailbox.delimiter {
        Some(d) => mailbox.name.rsplit(d).next().unwrap_or(&mailbox.name),
        None => &mailbox.name,
    };
    Some(match leaf.to_lowercase().as_str() {
        "sent" | "sent items" | "sent messages" | "sent mail" | "已发送" | "已发送邮件" => {
            FolderRole::Sent
        }
        "drafts" | "draft" | "草稿" | "草稿箱" => FolderRole::Drafts,
        "trash" | "deleted items" | "deleted messages" | "bin" | "已删除" | "已删除邮件" => {
            FolderRole::Trash
        }
        "junk" | "spam" | "junk e-mail" | "junk email" | "垃圾邮件" => FolderRole::Junk,
        "archive" | "archives" | "归档" => FolderRole::Archive,
        _ => return None,
    })
}

fn new_message(m: FetchedMessage) -> NewMessage {
    let h = m.headers;
    NewMessage {
        state: m.state,
        internal_date: m.internal_date,
        size: m.size,
        message_id: h.message_id,
        in_reply_to: h.in_reply_to.into_iter().next(),
        references: h.references,
        subject: h.subject,
        from: h.from,
        to: h.to,
        cc: h.cc,
        date: h.date,
        has_attachments: m.has_attachments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(name: &str, special_use: Option<FolderRole>) -> ListedMailbox {
        ListedMailbox {
            raw_name: name.into(),
            name: name.into(),
            delimiter: Some('/'),
            selectable: true,
            special_use,
        }
    }

    #[test]
    fn sorted_difference_works() {
        assert_eq!(
            sorted_difference(&[1, 2, 4, 7, 9], &[2, 3, 7, 10]),
            [1, 4, 9]
        );
        assert_eq!(sorted_difference(&[], &[1]), [] as [u32; 0]);
        assert_eq!(sorted_difference(&[1, 2], &[]), [1, 2]);
    }

    #[test]
    fn assigns_roles() {
        let roles: Vec<_> = remote_folders(vec![
            listed("INBOX", None),
            listed("Sent Items", None),
            listed("Archive/Sent", None),
            listed("Mail/Spam", None),
            listed("草稿箱", None),
            listed("Trash", None),
            listed("Bin", Some(FolderRole::Trash)),
        ])
        .into_iter()
        .map(|f| (f.remote_name, f.role))
        .collect();
        assert_eq!(
            roles,
            [
                ("INBOX".into(), Some(FolderRole::Inbox)),
                ("Sent Items".into(), Some(FolderRole::Sent)),
                ("Archive/Sent".into(), None), // only the first guess per role
                ("Mail/Spam".into(), Some(FolderRole::Junk)),
                ("草稿箱".into(), Some(FolderRole::Drafts)),
                ("Trash".into(), None), // SPECIAL-USE wins over names
                ("Bin".into(), Some(FolderRole::Trash)),
            ]
        );
    }
}
