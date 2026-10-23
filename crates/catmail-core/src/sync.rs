//! IMAP sync of folders and message headers.
//!
//! First changed drafts are uploaded and the queued local changes replayed (see `ops`).
//! Then per folder: EXAMINE, compare UIDVALIDITY, diff UIDs to find new and vanished
//! messages, refresh flags of known messages (only the changed ones with CONDSTORE, else
//! all of them in windows), then fetch new headers newest first in batches. Every batch
//! is its own transaction, so an interrupted sync keeps what it fetched and the next one
//! only diffs again. The folder's cursor (UIDNEXT, HIGHESTMODSEQ) is written last, once
//! the folder matches the server as of the EXAMINE.

use std::collections::HashSet;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::{Duration, Instant};

use catmail_proto::imap::{ChangedFlags, FetchedMessage, ListedMailbox, MailboxStatus, Session};
use catmail_store::{FolderSyncState, NewMessage, RemoteFolder};
use catmail_types::{
    AccountId, Event, FolderId, FolderRole, FolderSyncReport, ReplayReport, SyncReport,
};

use crate::connection::with_session;
use crate::draft_sync::Uploads;
use crate::engine::{Activity, Engine, now};
use crate::{Error, Result, connection, ops};

/// Stored messages per flags FETCH, and per transaction, on a server that can't say
/// which changed: the whole answer is held in memory.
const FLAGS_WINDOW: usize = 5000;

pub(crate) async fn sync_account(engine: Arc<Engine>, account_id: AccountId) -> Result<SyncReport> {
    // `run` takes the permit itself, folder by folder.
    let result = engine
        .cancellation(account_id)
        .run(async {
            let mut session = connection::connect(&engine, account_id).await?;
            let report = run(&engine, &mut session, account_id, Uploads::All).await?;
            session.logout().await;
            Ok(report)
        })
        .await;
    engine.report(account_id, Activity::Sync, &result);
    result
}

/// Syncs one folder, after making queued local changes (and settled drafts) on the
/// server, unless it completed a sync less than `max_age` ago. `None` if it didn't sync:
/// the folder was fresh enough or can't hold messages. Only a sync that went to the
/// server says anything about it in the account's status.
pub(crate) async fn sync_one(
    engine: Arc<Engine>,
    folder: FolderId,
    max_age: Duration,
) -> Result<Option<FolderSyncReport>> {
    let account = engine
        .read(move |db| db.folders().get(folder))
        .await?
        .ok_or(Error::FolderNotFound(folder))?
        .account_id;
    let permit = engine.sync_permit(account).await;
    permit
        .run(async {
            // Checked under the permit: a full sync that held it may just have done this.
            // A clock set back counts as just synced.
            let current = engine
                .read(move |db| db.folders().get(folder))
                .await?
                .ok_or(Error::FolderNotFound(folder))?;
            let fresh = current
                .last_synced_at
                .is_some_and(|t| now() - t < max_age.as_secs() as i64);
            if !current.selectable || fresh {
                return Ok(None);
            }
            // Safe to run again on a new connection if a reused one turns out dead: what
            // was replayed is no longer pending, and the sync resumes from what it stored.
            let synced = with_session(&engine, account, {
                let engine = engine.clone();
                async move |session| {
                    // Local changes first, as for a full sync.
                    let mut replayed = ops::Replayed::default();
                    ops::replay_queued(&engine, session, account, Uploads::Settled, &mut replayed)
                        .await?;
                    let state = engine
                        .read(move |db| db.folders().sync_state(folder))
                        .await?
                        .ok_or(Error::FolderNotFound(folder))?;
                    let replayed = replayed.folders.get(&folder);
                    sync_reported(&engine, session, account, &state, replayed).await
                }
            })
            .await;
            engine.report(account, Activity::Sync, &synced);
            synced.map(Some)
        })
        .await
}

/// Syncs every folder of the account on `session`, making queued local changes and
/// uploading changed drafts (those `uploads` takes) first. Call without the account's
/// sync permit: it is taken for each folder rather than for the whole run, so a sync
/// waiting for it (of the folder the user opened, say) goes in between folders instead
/// of waiting for every one, and a folder it completed meanwhile isn't synced again.
pub(crate) async fn run(
    engine: &Arc<Engine>,
    session: &mut Session,
    account_id: AccountId,
    uploads: Uploads,
) -> Result<SyncReport> {
    let permit = engine.sync_permit(account_id).await;
    let (start, mut replayed, ids) = permit
        .run(async {
            // Taken holding the permit: a sync completed after this began after it too,
            // and saw the server as it is now or later. One that held the permit before
            // may have seen it as it was before this was asked for.
            let start = Instant::now();
            let remote = remote_folders(session.list().await?);
            let reconciled = engine
                .write(move |db| db.folders().reconcile(account_id, &remote))
                .await?;
            if reconciled.changed {
                engine.emit(Event::FoldersChanged {
                    account: account_id,
                });
            }
            ops::announce(engine, &reconciled.touched);

            // Local changes first, so the sync sees the server with them made.
            let mut replayed = ops::Replayed::default();
            ops::replay_queued(engine, session, account_id, uploads, &mut replayed).await?;
            // Only which folders, in which order: their state is read when each one's
            // turn comes.
            let ids: Vec<FolderId> = engine
                .read(move |db| db.folders().sync_states(account_id))
                .await?
                .into_iter()
                .filter(|f| f.selectable)
                .map(|f| f.id)
                .collect();
            Ok((start, replayed.folders, ids))
        })
        .await?;

    let mut report = SyncReport {
        account: account_id,
        folders: Vec::new(),
    };
    for id in ids {
        let replayed = replayed.remove(&id);
        let permit = engine.sync_permit(account_id).await;
        let folder_report = permit
            .run(async {
                // Read now, not above: another sync that went in before this folder may
                // have synced it, started a new UIDVALIDITY, or found it deleted.
                let current = engine.read(move |db| db.folders().sync_state(id)).await?;
                let Some(current) = current.filter(|f| f.selectable) else {
                    return Ok(None);
                };
                if engine
                    .folder_synced(account_id, id)
                    .is_some_and(|t| t > start)
                {
                    return Ok(Some(replay_report(&current, replayed.as_ref())));
                }
                sync_reported(engine, session, account_id, &current, replayed.as_ref())
                    .await
                    .map(Some)
            })
            .await?;
        report.folders.extend(folder_report);
    }
    engine.set_synced(account_id);
    Ok(report)
}

/// Syncs `folder` and reports how it went, with what replaying its queued changes did.
/// The server refusing something about the folder (it may have been deleted since LIST)
/// goes in the report, so other folders can still sync; only a fatal error fails.
async fn sync_reported(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    folder: &FolderSyncState,
    replayed: Option<&ReplayReport>,
) -> Result<FolderSyncReport> {
    let mut report = replay_report(folder, replayed);
    let all = ChangedFlags::Unknown;
    match sync_folder(engine, session, account, folder, &all, &mut report).await {
        Ok(()) => {}
        Err(e) if e.is_refusal() => {
            tracing::warn!(folder = %folder.name, error = %e, "folder sync failed");
            report.error = Some((&e).into());
        }
        Err(e) => return Err(e),
    }
    Ok(report)
}

/// A report on `folder` that says what replaying its queued changes did, and nothing else
/// yet.
fn replay_report(folder: &FolderSyncState, replayed: Option<&ReplayReport>) -> FolderSyncReport {
    let mut report = FolderSyncReport::new(folder.id, &folder.name);
    if let Some(r) = replayed {
        (report.ops_applied, report.ops_failed) = (r.applied, r.failed);
    }
    report
}

/// Syncs INBOX alone on `session`, leaving it selected (read-only). Of the flags of
/// stored messages, only those `changed_flags` names are refreshed, besides what
/// CONDSTORE says changed; unless that is all of them, the sync doesn't count as
/// completed. False if the account has no INBOX to sync (or doesn't know of one yet).
/// Call with the account's sync permit.
pub(crate) async fn sync_inbox(
    engine: &Arc<Engine>,
    session: &mut Session,
    account_id: AccountId,
    changed_flags: &ChangedFlags,
) -> Result<bool> {
    let inbox = engine
        .read(move |db| {
            let folders = db.folders();
            match folders.by_role(account_id, FolderRole::Inbox)? {
                Some((id, _)) => folders.sync_state(id),
                None => Ok(None),
            }
        })
        .await?;
    let Some(inbox) = inbox else {
        return Ok(false);
    };
    let mut report = FolderSyncReport::new(inbox.id, &inbox.name);
    sync_folder(
        engine,
        session,
        account_id,
        &inbox,
        changed_flags,
        &mut report,
    )
    .await?;
    Ok(true)
}

async fn sync_folder(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    folder: &FolderSyncState,
    changed_flags: &ChangedFlags,
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
    if !status.same_generation(folder.uidvalidity) {
        // UIDs of the old generation now name different messages (or nothing).
        let uidvalidity = status.uidvalidity;
        let touched = engine
            .write(move |db| db.folders().reset_uidvalidity(id, uidvalidity))
            .await?;
        // Messages moved out of it locally are gone from where they were moved to.
        ops::announce(engine, &touched);
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
    // complete sync, and nothing at all if HIGHESTMODSEQ hasn't moved. Without it, of
    // those the caller says changed: all of them unless it knows which. Never fewer
    // with CONDSTORE: the cursor written below would claim the changes were seen.
    let all_flags = status.highest_modseq.is_some() || *changed_flags == ChangedFlags::Unknown;
    let unchanged = modseq.is_some() && modseq == status.highest_modseq;
    if count > report.removed && !unchanged {
        let since = status.highest_modseq.and(modseq);
        // Only what changed comes back with CHANGEDSINCE; without it, one window of
        // stored messages at a time, so the answer held in memory stays bounded.
        let windows: Vec<Vec<RangeInclusive<u32>>> = match (since, changed_flags) {
            (Some(_), _) => vec![vec![RangeInclusive::new(1, max_uid)]],
            (None, ChangedFlags::Uids(uids)) if !all_flags => uids
                .chunks(FLAGS_WINDOW)
                .map(|w| w.iter().map(|&uid| uid..=uid).collect())
                .collect(),
            (None, _) => engine
                .read(move |db| db.messages().uids(id))
                .await?
                .chunks(FLAGS_WINDOW)
                .map(|w| vec![RangeInclusive::new(w[0], w[w.len() - 1])])
                .collect(),
        };
        for uids in windows {
            let states = session.fetch_flags(&uids, since).await?;
            let updated = engine
                .write(move |db| Ok(db.messages().update_flags(id, &states)? as u32))
                .await?;
            if updated > 0 {
                report.updated += updated;
                changed();
            }
        }
    }

    // Newest first, so the top of the list fills in before the backlog.
    let mut complete = true;
    for batch in new.rchunks(engine.tuning.fetch_batch) {
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
            .write(move |db| db.folders().set_synced(id, uidnext, modseq, all_flags))
            .await?;
        if all_flags {
            engine.set_folder_synced(account, id);
        }
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
pub(crate) fn remote_folders(listed: Vec<ListedMailbox>) -> Vec<RemoteFolder> {
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
        reply_to: h.reply_to,
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
