//! IMAP sync of folders and message headers.
//!
//! First changed drafts are uploaded and the queued local changes replayed (see `ops`).
//! Then per folder: EXAMINE, compare UIDVALIDITY, diff UIDs to find new and vanished
//! messages, refresh flags of known messages (only the changed ones with CONDSTORE, else
//! all of them in windows), then fetch new headers newest first in batches. Every batch
//! is its own transaction, so an interrupted sync keeps what it fetched and the next one
//! only diffs again. The folder's cursor (UIDNEXT, HIGHESTMODSEQ) is written last, once
//! the folder matches the server as of the EXAMINE.
//!
//! On Gmail only All Mail, Trash and Spam hold messages (the other mailboxes are labels,
//! see `docs/architecture/labels.md`), and they are synced as one: a message another
//! client moves between them turns up in its new location under a new UID before it is
//! missed in the old one, so its row moves (with its body) instead of being deleted, and
//! only what vanished from one location without turning up in another is deleted, once
//! all three are synced.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::{Duration, Instant};

use catmail_proto::imap::{
    ChangedFlags, FetchedMessage, Items, ListedMailbox, MailboxStatus, Session,
};
use catmail_store::{FolderSyncState, NewMessage, RemoteFolder, ReplayFolder};
use catmail_types::{
    AccountId, AccountMode, Event, Folder, FolderId, FolderKind, FolderRole, FolderSyncReport,
    ReplayReport, SyncReport,
};

use crate::connection::with_session;
use crate::draft_sync::Uploads;
use crate::engine::{Activity, Engine};
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

/// An account's first sync: background sync's, if it is running it, else one of its own.
/// The one joined isn't recorded in the account's status again: it already was, and a
/// newer outcome may have been since.
pub(crate) async fn initial_sync(engine: Arc<Engine>, account_id: AccountId) -> Result<SyncReport> {
    let joined = engine
        .cancellation(account_id)
        .run(async { Ok(engine.first_sync(account_id).await) })
        .await?;
    match joined {
        Some(report) => Ok(report),
        None => sync_account(engine, account_id).await,
    }
}

/// Syncs one folder, after making queued local changes (and settled drafts) on the
/// server, unless it completed a sync less than `max_age` ago. `None` if it didn't sync:
/// the folder was fresh enough or can't hold messages. On Gmail that is a sync of the
/// three locations, whichever folder it is, and a label's report is theirs together.
/// Only a sync that went to the server says anything about it in the account's status.
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
            let (current, mode, locations) = engine
                .read(move |db| {
                    let folders = db.folders();
                    let mode = db.accounts().get(account)?.map(|a| a.mode);
                    Ok((folders.get(folder)?, mode, folders.location_ids(account)?))
                })
                .await?;
            let current = current.ok_or(Error::FolderNotFound(folder))?;
            if !current.selectable {
                return Ok(None);
            }
            let gmail = mode == Some(AccountMode::Gmail);
            // Checked under the permit: a full sync that held it may just have done this.
            // Kept in memory only, so after a restart the first open syncs. On Gmail a
            // folder is as fresh as the locations its messages are in.
            let fresh = |id| {
                engine
                    .folder_synced(account, id)
                    .is_some_and(|t| t.elapsed() < max_age)
            };
            if (gmail && locations.iter().copied().all(fresh)) || (!gmail && fresh(folder)) {
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
                    if gmail {
                        let reports = sync_locations(
                            &engine,
                            session,
                            account,
                            locations.clone(),
                            &replayed.folders,
                        )
                        .await?;
                        return Ok(report_on(&current, reports));
                    }
                    let state = engine
                        .read(move |db| db.folders().sync_state(folder))
                        .await?
                        .ok_or(Error::FolderNotFound(folder))?;
                    let replayed = replayed.folders.get(&folder);
                    let mut unit = Unit::new(AccountMode::Standard);
                    sync_reported(&engine, session, account, &state, replayed, &mut unit).await
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
    let (start, mut replayed, ids, mode) = permit
        .run(async {
            // Taken holding the permit: a sync completed after this began after it too,
            // and saw the server as it is now or later. One that held the permit before
            // may have seen it as it was before this was asked for.
            let start = Instant::now();
            let listed = session.list().await?;
            let mode = mode_of(session, &listed);
            let remote = remote_folders(listed, mode);
            let reconciled = engine
                .write(move |db| {
                    let mut reconciled = db.folders().reconcile(account_id, &remote)?;
                    // What a message row is changed with the mode (one per location, or
                    // one per X-GM-MSGID): every location starts over.
                    if db.accounts().set_mode(account_id, mode)? {
                        reconciled
                            .touched
                            .merge(db.folders().end_generations(account_id)?);
                        reconciled.changed = true;
                    }
                    Ok(reconciled)
                })
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
            let ids = engine
                .read(move |db| db.folders().location_ids(account_id))
                .await?;
            Ok((start, replayed.folders, ids, mode))
        })
        .await?;

    let mut report = SyncReport {
        account: account_id,
        folders: Vec::new(),
    };
    let mut unit = Unit::new(mode);
    for &id in &ids {
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
                let replayed = replayed.as_ref();
                sync_reported(engine, session, account_id, &current, replayed, &mut unit)
                    .await
                    .map(Some)
            })
            .await?;
        report.folders.extend(folder_report);
    }
    if !unit.vanished.is_empty() {
        let permit = engine.sync_permit(account_id).await;
        let finished = finish(
            engine,
            session,
            account_id,
            &mut unit,
            &ids,
            &mut report.folders,
        );
        permit.run(finished).await?;
    }
    engine.set_synced(account_id);
    Ok(report)
}

/// Gmail if the server has X-GM-EXT-1 and lists All Mail. All Mail can be hidden from
/// IMAP in Gmail's settings, and then the labels' mailboxes are all there is to sync.
pub(crate) fn mode_of(session: &Session, listed: &[ListedMailbox]) -> AccountMode {
    if session.is_gmail()
        && listed
            .iter()
            .any(|m| m.special_use == Some(FolderRole::All))
    {
        AccountMode::Gmail
    } else {
        AccountMode::Standard
    }
}

/// How locations sync: each on its own, or on Gmail the three as one (see the module
/// docs), what vanished from one deleted only once all are synced ([`finish`]).
struct Unit {
    gmail: bool,
    /// What vanished from the locations synced so far and isn't deleted yet: (location,
    /// its UIDVALIDITY, UIDs).
    vanished: Vec<(FolderId, u32, Vec<u32>)>,
    /// Every location synced so far stored all it has. If not, one that vanished may be
    /// where it went, not stored: nothing is deleted, the next sync finds it again.
    complete: bool,
}

impl Unit {
    fn new(mode: AccountMode) -> Self {
        Self {
            gmail: mode == AccountMode::Gmail,
            vanished: Vec::new(),
            complete: true,
        }
    }

    /// What FETCH asks for: on Gmail, X-GM-MSGID, X-GM-THRID and X-GM-LABELS too.
    fn items(&self) -> Items {
        if self.gmail {
            Items::Gmail
        } else {
            Items::Standard
        }
    }

    /// Notes that `uids` vanished from `folder` under `uidvalidity`, and deletes them at
    /// once unless the unit spans several locations.
    async fn vanish(
        &mut self,
        engine: &Arc<Engine>,
        account: AccountId,
        (folder, uidvalidity, uids): (FolderId, u32, Vec<u32>),
        report: &mut FolderSyncReport,
    ) -> Result<()> {
        self.vanished.push((folder, uidvalidity, uids));
        if self.gmail {
            return Ok(());
        }
        self.delete_vanished(engine, account, std::slice::from_mut(report))
            .await
    }

    /// Deletes the rows of what vanished, still where it vanished from (not moved to
    /// another location since), adding them to the location's report. Not if the
    /// location started a new UIDVALIDITY since: the UIDs name other messages. On Gmail,
    /// nothing if the unit isn't complete. Call with the account's sync permit.
    async fn delete_vanished(
        &mut self,
        engine: &Arc<Engine>,
        account: AccountId,
        reports: &mut [FolderSyncReport],
    ) -> Result<()> {
        let vanished = std::mem::take(&mut self.vanished);
        // On its own, a location has nowhere else for them to be.
        if self.gmail && !self.complete {
            tracing::debug!(%account, "not all locations synced; deleting nothing");
            return Ok(());
        }
        for (folder, uidvalidity, uids) in vanished {
            let (removed, labels) = engine
                .write(move |db| {
                    let messages = db.messages();
                    let removed = messages.delete_uids_of(folder, uidvalidity, &uids)?;
                    Ok((removed as u32, messages.touched_labels()))
                })
                .await?;
            if removed == 0 {
                continue;
            }
            if let Some(report) = reports.iter_mut().find(|r| r.folder == folder) {
                report.removed += removed;
            }
            emit_changed(engine, account, [folder].into_iter().chain(labels));
        }
        Ok(())
    }
}

/// Looks for new arrivals [`finish`] makes at most before it gives up on settling what
/// vanished.
const RECHECKS: usize = 3;

/// Ends a pass of `unit` over the locations `ids`, whose reports `reports` are: on Gmail,
/// deletes what vanished and didn't turn up in another location. A moved message (one
/// another client restores from Trash to All Mail, say) shows up above its new
/// location's UIDNEXT, but maybe only after that location's turn. So the locations are
/// looked at again for new arrivals, which moves such a row, until a look finds nothing
/// new anywhere: what vanished and is still where it vanished from is then nowhere, and
/// is deleted. If the looks don't settle within [`RECHECKS`], nothing is; the next sync
/// finds it missing again. Call with the account's sync permit.
async fn finish(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    unit: &mut Unit,
    ids: &[FolderId],
    reports: &mut [FolderSyncReport],
) -> Result<()> {
    for _ in 0..RECHECKS {
        // Those that turned up elsewhere have moved: nothing to look for.
        let vanished = std::mem::take(&mut unit.vanished);
        unit.vanished = engine
            .read(move |db| {
                let messages = db.messages();
                let mut stored = Vec::new();
                for (folder, uidvalidity, uids) in vanished {
                    let uids = messages.stored(folder, &uids)?;
                    if !uids.is_empty() {
                        stored.push((folder, uidvalidity, uids));
                    }
                }
                Ok(stored)
            })
            .await?;
        if unit.vanished.is_empty() || !unit.complete {
            break;
        }
        let mut arrived = false;
        for &id in ids {
            match arrivals(engine, session, account, id, unit, reports).await {
                Ok(some) => arrived |= some,
                Err(e) if e.is_refusal() => {
                    tracing::warn!(folder = %id, error = %e, "folder sync failed");
                    unit.complete = false;
                }
                Err(e) => return Err(e),
            }
        }
        if !arrived {
            return unit.delete_vanished(engine, account, reports).await;
        }
    }
    if !unit.vanished.is_empty() {
        tracing::debug!(%account, "messages vanished and others keep arriving; left for the next sync");
        unit.vanished.clear();
    }
    Ok(())
}

/// Fetches what arrived in location `id` since its cursor, adding it to its report in
/// `reports`. True if anything did, even if it left again. A location without a cursor,
/// or with a new UIDVALIDITY, can't tell what arrived: the unit is then incomplete.
async fn arrivals(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    id: FolderId,
    unit: &mut Unit,
    reports: &mut [FolderSyncReport],
) -> Result<bool> {
    let Some(folder) = engine.read(move |db| db.folders().sync_state(id)).await? else {
        return Ok(false);
    };
    let status = session.examine(&folder.remote_name).await?;
    let Some(uidnext) = folder
        .uidnext
        .filter(|_| status.same_generation(folder.uidvalidity))
    else {
        unit.complete = false;
        return Ok(false);
    };
    if status.uidnext == Some(uidnext) {
        return Ok(false);
    }
    let stored = engine
        .read(move |db| db.messages().uids_from(id, uidnext))
        .await?;
    let new = sorted_difference(&session.uid_search_from(uidnext).await?, &stored);
    let mut scratch = FolderSyncReport::new(id, &folder.name);
    let report = match reports.iter_mut().find(|r| r.folder == id) {
        Some(report) => report,
        None => &mut scratch,
    };
    let complete = fetch_new(engine, session, account, id, &new, unit.items(), report).await?;
    unit.complete &= complete;
    if complete {
        // Flags stay as of the last look at them.
        let (uidnext, modseq) = (status.uidnext, folder.highest_modseq);
        engine
            .write(move |db| db.folders().set_synced(id, uidnext, modseq, false))
            .await?;
    }
    Ok(true)
}

/// On Gmail, syncs the three locations, `ids`, as one, reporting on each as
/// [`sync_reported`] does. Call with the account's sync permit.
async fn sync_locations(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    ids: Vec<FolderId>,
    replayed: &HashMap<FolderId, ReplayReport>,
) -> Result<Vec<FolderSyncReport>> {
    let mut unit = Unit::new(AccountMode::Gmail);
    let mut reports = Vec::new();
    for &id in &ids {
        // Read at its turn: syncing the others may have moved messages in or out.
        let Some(location) = engine.read(move |db| db.folders().sync_state(id)).await? else {
            continue;
        };
        let replayed = replayed.get(&id);
        reports
            .push(sync_reported(engine, session, account, &location, replayed, &mut unit).await?);
    }
    finish(engine, session, account, &mut unit, &ids, &mut reports).await?;
    Ok(reports)
}

/// What a sync of the locations did, as a report on `folder`: its own if it is one of
/// them, else (a label) theirs together.
fn report_on(folder: &Folder, reports: Vec<FolderSyncReport>) -> FolderSyncReport {
    let mut on = FolderSyncReport::new(folder.id, &folder.name);
    for r in reports {
        if r.folder == folder.id {
            return r;
        }
        on.added += r.added;
        on.updated += r.updated;
        on.removed += r.removed;
        on.ops_applied += r.ops_applied;
        on.ops_failed += r.ops_failed;
        on.uidvalidity_changed |= r.uidvalidity_changed;
        on.error = on.error.or(r.error);
    }
    on
}

/// Tells frontends that the messages of these folders changed.
fn emit_changed(engine: &Engine, account: AccountId, folders: impl IntoIterator<Item = FolderId>) {
    for folder in folders.into_iter().collect::<BTreeSet<_>>() {
        engine.emit(Event::MessagesChanged { account, folder });
    }
}

/// Syncs `folder` as part of `unit` and reports how it went, with what replaying its
/// queued changes did. The server refusing something about the folder (it may have been
/// deleted since LIST) goes in the report, so other folders can still sync; only a fatal
/// error fails.
async fn sync_reported(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    folder: &FolderSyncState,
    replayed: Option<&ReplayReport>,
    unit: &mut Unit,
) -> Result<FolderSyncReport> {
    let mut report = replay_report(folder, replayed);
    let all = ChangedFlags::Unknown;
    match sync_folder(engine, session, account, folder, &all, &mut report, unit).await {
        Ok(()) => {}
        Err(e) if e.is_refusal() => {
            tracing::warn!(folder = %folder.name, error = %e, "folder sync failed");
            report.error = Some((&e).into());
            unit.complete = false;
        }
        Err(e) => return Err(e),
    }
    Ok(report)
}

/// Deletes those of `uids` that `folder` no longer has, as a sync would, asking the
/// server about these alone rather than the whole folder. Nothing if the folder has a new
/// UIDVALIDITY: the sync starts the new generation. Call with the account's sync permit.
pub(crate) async fn drop_vanished(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    folder: &ReplayFolder,
    uids: &[u32],
) -> Result<()> {
    let status = session.examine(&folder.remote_name).await?;
    if !status.same_generation(folder.uidvalidity) {
        return Ok(());
    }
    let there = session.uid_search_uids(uids).await?;
    let vanished = sorted_difference(uids, &there);
    if vanished.is_empty() {
        return Ok(());
    }
    let id = folder.id;
    engine
        .write(move |db| db.messages().delete_uids(id, &vanished))
        .await?;
    engine.emit(Event::MessagesChanged {
        account,
        folder: id,
    });
    Ok(())
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
/// completed. On Gmail INBOX is a label: the three locations are synced instead, with
/// all of their flags (CONDSTORE fetches only what changed, labels too), and INBOX is
/// only opened. False if the account has no INBOX to sync (or doesn't know of one yet).
/// Call with the account's sync permit.
pub(crate) async fn sync_inbox(
    engine: &Arc<Engine>,
    session: &mut Session,
    account_id: AccountId,
    changed_flags: &ChangedFlags,
) -> Result<bool> {
    let (inbox, mode, locations) = engine
        .read(move |db| {
            let folders = db.folders();
            let inbox = match folders.by_role(account_id, FolderRole::Inbox)? {
                Some((id, _)) => folders.sync_state(id)?,
                None => None,
            };
            let mode = db.accounts().get(account_id)?.map(|a| a.mode);
            Ok((inbox, mode, folders.location_ids(account_id)?))
        })
        .await?;
    let Some(inbox) = inbox else {
        return Ok(false);
    };
    if mode == Some(AccountMode::Gmail) {
        sync_locations(engine, session, account_id, locations, &HashMap::new()).await?;
        session.examine(&inbox.remote_name).await?;
        return Ok(true);
    }
    let mut report = FolderSyncReport::new(inbox.id, &inbox.name);
    let mut unit = Unit::new(AccountMode::Standard);
    sync_folder(
        engine,
        session,
        account_id,
        &inbox,
        changed_flags,
        &mut report,
        &mut unit,
    )
    .await?;
    Ok(true)
}

/// Syncs one location as part of `unit`.
async fn sync_folder(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    folder: &FolderSyncState,
    changed_flags: &ChangedFlags,
    report: &mut FolderSyncReport,
    unit: &mut Unit,
) -> Result<()> {
    let id = folder.id;
    let (gmail, items) = (unit.gmail, unit.items());
    // On Gmail labels show messages of the location, and Starred the flagged ones.
    let changed =
        |labels: Vec<FolderId>| emit_changed(engine, account, [id].into_iter().chain(labels));
    let status = session.examine(&folder.remote_name).await?;

    let (mut uidnext, mut modseq) = (folder.uidnext, folder.highest_modseq);
    let (mut count, mut max_uid) = (folder.message_count, folder.max_uid);
    if !status.same_generation(folder.uidvalidity) {
        // UIDs of the old generation now name different messages (or nothing).
        let uidvalidity = status.uidvalidity;
        let (touched, labels) = engine
            .write(move |db| {
                let messages = db.messages();
                if gmail {
                    messages.note_folder_shown(id)?;
                }
                let touched = db.folders().reset_uidvalidity(id, uidvalidity)?;
                Ok((touched, messages.touched_labels()))
            })
            .await?;
        // Messages moved out of it locally are gone from where they were moved to.
        ops::announce(engine, &touched);
        report.uidvalidity_changed = folder.uidvalidity.is_some();
        if report.uidvalidity_changed {
            changed(labels);
        }
        (uidnext, modseq, count, max_uid) = (None, None, 0, 0);
    }

    let (new, vanished) = new_and_vanished(engine, session, id, count, uidnext, &status).await?;
    if !vanished.is_empty() {
        let vanished = (id, status.uidvalidity, vanished);
        unit.vanish(engine, account, vanished, report).await?;
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
        for ranges in windows {
            let states = session.fetch_flags(&ranges, since, items).await?;
            let (updated, labels) = engine
                .write(move |db| {
                    let messages = db.messages();
                    let updated = messages.update_flags(id, &states)? as u32;
                    Ok((updated, messages.touched_labels()))
                })
                .await?;
            if updated > 0 {
                report.updated += updated;
                changed(labels);
            }
        }
    }

    let complete = fetch_new(engine, session, account, id, &new, items, report).await?;

    // The cursor promises that everything below it is stored; with a gap, leave the old
    // one so the next sync diffs in full.
    unit.complete &= complete;
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

/// Fetches the messages at `new` in location `id`, which is selected, adding them to
/// `report`: newest first, in batches, so the top of the list fills in before the
/// backlog. False if some weren't stored: expunged since the SEARCH, in a response that
/// couldn't be parsed, or on Gmail held for a local move.
async fn fetch_new(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    id: FolderId,
    new: &[u32],
    items: Items,
    report: &mut FolderSyncReport,
) -> Result<bool> {
    let mut complete = true;
    for batch in new.rchunks(engine.tuning.fetch_batch) {
        let fetched: Vec<NewMessage> = session
            .fetch_summaries(batch, items)
            .await?
            .into_iter()
            .map(new_message)
            .collect();
        complete &= fetched.len() == batch.len();
        let (upserted, mut touched) = engine
            .write(move |db| {
                let messages = db.messages();
                let upserted = messages.upsert(account, id, &fetched)?;
                Ok((upserted, messages.touched_labels()))
            })
            .await?;
        complete &= upserted.held == 0;
        report.added += (upserted.added + upserted.moved) as u32;
        // On Gmail, the labels showing them and the locations they moved from.
        touched.extend(upserted.moved_from);
        emit_changed(engine, account, [id].into_iter().chain(touched));
    }
    Ok(complete)
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

/// Each mailbox's role: INBOX by name, then SPECIAL-USE attributes, then well-known
/// names for roles no folder claimed via SPECIAL-USE. Gmail declares every role it has, so
/// names aren't guessed there: a user label called Trash would otherwise stand in for a
/// Trash hidden from IMAP.
pub(crate) fn roles(listed: &[ListedMailbox], mode: AccountMode) -> Vec<Option<FolderRole>> {
    let claimed: HashSet<FolderRole> = listed.iter().filter_map(|m| m.special_use).collect();
    let mut guessed = HashSet::new();
    listed
        .iter()
        .map(|m| {
            if m.raw_name.eq_ignore_ascii_case("INBOX") {
                Some(FolderRole::Inbox)
            } else if m.special_use.is_some() || mode == AccountMode::Gmail {
                m.special_use
            } else {
                guess_role(m).filter(|r| m.selectable && !claimed.contains(r) && guessed.insert(*r))
            }
        })
        .collect()
}

/// The listed mailboxes with their [`roles`] and kinds: on Gmail the locations are All
/// Mail, Trash and Spam, and every other mailbox that can hold messages is a label.
pub(crate) fn remote_folders(listed: Vec<ListedMailbox>, mode: AccountMode) -> Vec<RemoteFolder> {
    let roles = roles(&listed, mode);
    listed
        .into_iter()
        .zip(roles)
        .map(|(m, role)| {
            let location = matches!(
                role,
                Some(FolderRole::All | FolderRole::Trash | FolderRole::Junk)
            );
            RemoteFolder {
                remote_name: m.raw_name,
                name: m.name,
                delimiter: m.delimiter,
                role,
                kind: if mode == AccountMode::Gmail && m.selectable && !location {
                    FolderKind::Label
                } else {
                    FolderKind::Location
                },
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
        gm_msgid: m.gm_msgid,
        gm_thrid: m.gm_thrid,
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
        let roles: Vec<_> = remote_folders(
            vec![
                listed("INBOX", None),
                listed("Sent Items", None),
                listed("Archive/Sent", None),
                listed("Mail/Spam", None),
                listed("草稿箱", None),
                listed("Trash", None),
                listed("Bin", Some(FolderRole::Trash)),
            ],
            AccountMode::Standard,
        )
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
