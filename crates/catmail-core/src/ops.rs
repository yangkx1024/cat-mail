//! Changes to messages (flags, moves, deletions): made locally at once, queued, and
//! replayed on the server folder by folder.
//!
//! Replay opens a folder with SELECT and runs its ops only if the UIDVALIDITY is still
//! the one they were recorded under; otherwise they wait for the sync that sees the new
//! generation and turns them into conflicts. A refusal (NO) fails the ops concerned and
//! undoes them locally; a lost connection leaves them pending. Every op is safe to run
//! twice (a UID that is gone is skipped by the server), so an answer lost to a dropped
//! connection needs no care beyond running it again.
//!
//! Replay of an account is serialized with its sync by the sync permit. A sync replays
//! everything first, so that it sees the server with the changes made, moves into
//! folders it syncs included. Changed drafts are uploaded right before (`draft_sync`),
//! so the copies they replace are deleted in the same replay.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use catmail_proto::imap::{CopyUid, Session};
use catmail_store::{CopyCheckpoint, Moved, ReplayFolder, ReplayKind, ReplayOp, Touched};
use catmail_types::{
    AccountId, Event, Flags, FolderId, MessageId, PendingOp, PendingOpId, ReplayReport,
};

use crate::connection::with_session;
use crate::engine::{Activity, Engine, now};
use crate::{Error, Result, draft_sync};

/// Ops per command; UID sets of scattered messages get long.
const BATCH: usize = 500;

pub(crate) async fn set_flags(
    engine: Arc<Engine>,
    messages: Vec<MessageId>,
    add: Flags,
    remove: Flags,
) -> Result<Vec<AccountId>> {
    change(&engine, move |db| {
        db.ops().set_flags(&messages, add, remove, now())
    })
    .await
}

pub(crate) async fn move_messages(
    engine: Arc<Engine>,
    messages: Vec<MessageId>,
    to: FolderId,
) -> Result<Vec<AccountId>> {
    change(&engine, move |db| {
        db.ops().move_messages(&messages, to, now())
    })
    .await
}

pub(crate) async fn delete_messages(
    engine: Arc<Engine>,
    messages: Vec<MessageId>,
    permanently: bool,
) -> Result<Vec<AccountId>> {
    change(&engine, move |db| {
        db.ops().delete_messages(&messages, permanently, now())
    })
    .await
}

/// Makes a local change, all of it or nothing, and announces it. Returns the accounts
/// it touched.
async fn change(
    engine: &Arc<Engine>,
    f: impl FnOnce(&catmail_store::Db<'_>) -> catmail_store::Result<Touched> + Send + 'static,
) -> Result<Vec<AccountId>> {
    let touched = engine.write(f).await?;
    Ok(announce(engine, &touched))
}

/// Emits the events for `touched`, and wakes the accounts' background sync to make any
/// ops queued on the server. Returns its accounts.
pub(crate) fn announce(engine: &Engine, touched: &Touched) -> Vec<AccountId> {
    for &(account, folder) in touched {
        engine.emit(Event::MessagesChanged { account, folder });
    }
    let mut accounts: Vec<AccountId> = touched.iter().map(|(a, _)| *a).collect();
    accounts.dedup();
    for &account in &accounts {
        engine.emit(Event::PendingOpsChanged { account });
        // Sync's own conflicts come through here too; that costs a needless look at the
        // queue, rarely.
        engine.wake(account);
    }
    accounts
}

pub(crate) async fn list(
    engine: Arc<Engine>,
    account: Option<AccountId>,
) -> Result<Vec<PendingOp>> {
    engine.read(move |db| db.ops().list(account)).await
}

pub(crate) async fn dismiss(engine: Arc<Engine>, id: PendingOpId) -> Result<()> {
    let account = engine
        .write(move |db| db.ops().dismiss(id))
        .await?
        .ok_or_else(|| Error::InvalidInput(format!("no operation {id}")))?;
    engine.emit(Event::PendingOpsChanged { account });
    Ok(())
}

/// Replays the account's queued changes now, without syncing, on a pooled connection,
/// uploading changed drafts first. Waits for a running sync of the account, which does
/// the same.
pub(crate) async fn replay_account(
    engine: Arc<Engine>,
    account: AccountId,
) -> Result<ReplayReport> {
    let permit = engine.sync_permit(account).await;
    let run = async {
        let needed = engine
            .read(move |db| {
                Ok(!db.ops().pending_folders(account)?.is_empty()
                    || db.drafts().need_upload(account)?)
            })
            .await?;
        let mut report = ReplayReport::default();
        if needed {
            // Safe to run again on a new connection if a reused one turns out dead: what
            // went through is no longer pending, and uploads left unsettled settle first.
            let replayed = with_session(&engine, account, |session| {
                let engine = engine.clone();
                Box::pin(async move { replay_queued(&engine, session, account).await })
            })
            .await;
            engine.report(account, Activity::Sync, &replayed);
            let replayed = replayed?;
            report = replayed.drafts;
            for r in replayed.folders.values() {
                report.applied += r.applied;
                report.failed += r.failed;
            }
        }
        report.pending = engine
            .read(move |db| db.ops().count_pending(account))
            .await?;
        Ok(report)
    };
    permit.run(run).await
}

/// What [`replay_queued`] did.
pub(crate) struct Replayed {
    /// `drafts_uploaded` and `drafts_failed`.
    pub drafts: ReplayReport,
    /// By folder, `applied` and `failed`.
    pub folders: HashMap<FolderId, ReplayReport>,
}

/// Uploads the account's changed drafts, then replays all its queued ops, deleting the
/// copies those drafts replace among them. Call with the account's sync permit.
pub(crate) async fn replay_queued(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
) -> Result<Replayed> {
    let drafts = draft_sync::upload(engine, session, account).await?;
    let pending = engine
        .read(move |db| db.ops().pending_folders(account))
        .await?;
    let folders = replay(engine, session, account, &pending).await?;
    Ok(Replayed { drafts, folders })
}

/// Replays the ops of `folders` on `session`, one folder after the other, and returns
/// what happened in each (`pending` is not filled in). Call with the account's sync
/// permit. A folder the server won't open is skipped; the sync finds out what became of
/// it.
async fn replay(
    engine: &Arc<Engine>,
    session: &mut Session,
    account: AccountId,
    folders: &[ReplayFolder],
) -> Result<HashMap<FolderId, ReplayReport>> {
    let mut touched = Touched::new();
    let mut reports: HashMap<FolderId, ReplayReport> = HashMap::new();
    let mut result = Ok(());
    let mut visited = HashSet::new();
    let mut next = folders.to_vec();
    // Settling a copy can send an op on to the copy's folder (a moved message moved back,
    // or deleted): one more pass for folders that weren't replayed yet.
    'passes: while !next.is_empty() {
        for folder in next {
            visited.insert(folder.id);
            match replay_folder(engine, session, &folder, &mut touched).await {
                Ok(report) => {
                    let total = reports.entry(folder.id).or_default();
                    total.applied += report.applied;
                    total.failed += report.failed;
                }
                Err(Error::Imap(e)) if !e.is_fatal() => {
                    tracing::warn!(folder = %folder.name, error = %e,
                        "can't replay changes in folder");
                }
                Err(e) => {
                    result = Err(e);
                    break 'passes;
                }
            }
        }
        next = match engine
            .read(move |db| db.ops().pending_folders(account))
            .await
        {
            Ok(pending) => pending
                .into_iter()
                .filter(|f| !visited.contains(&f.id))
                .collect(),
            Err(e) => {
                result = Err(e);
                break;
            }
        };
    }
    // Whatever was committed before a failure too.
    if !folders.is_empty() {
        replayed(engine, account, &touched);
    }
    result.map(|()| reports)
}

/// Emits the events for a replay's changes: to the account's queue, and to the messages
/// of the folders in `touched`. Unlike [`announce`] it wakes nobody: the replay is what
/// a wake would ask for.
pub(crate) fn replayed(engine: &Engine, account: AccountId, touched: &Touched) {
    for &(account, folder) in touched {
        engine.emit(Event::MessagesChanged { account, folder });
    }
    engine.emit(Event::PendingOpsChanged { account });
}

/// Runs the folder's pending ops on `session`, recording the folders whose messages
/// changed in `touched`.
async fn replay_folder(
    engine: &Arc<Engine>,
    session: &mut Session,
    folder: &ReplayFolder,
    touched: &mut Touched,
) -> Result<ReplayReport> {
    let id = folder.id;
    let ops = engine.read(move |db| db.ops().pending(id)).await?;
    if ops.is_empty() {
        return Ok(ReplayReport::default());
    }
    let status = session.select(&folder.remote_name).await?;
    if Some(status.uidvalidity) != folder.uidvalidity {
        // Not under the generation they name. The sync of this folder starts the new
        // one, which turns them into conflicts.
        return Ok(ReplayReport::default());
    }
    let mut done = Settle {
        engine,
        folder: id,
        uidvalidity: status.uidvalidity,
        touched,
        report: ReplayReport::default(),
    };
    // An earlier COPY may have made a copy, whatever the op was turned into since (moved
    // on, back, or deleted): settle that first, so no copy is left behind.
    let checkpointed: Vec<(ReplayOp, CopyCheckpoint)> = ops
        .iter()
        .filter_map(|op| Some((op.clone(), op.copy.clone()?)))
        .collect();
    let ops = if checkpointed.is_empty() {
        ops
    } else {
        if !settle_copies(&mut done, session, folder, checkpointed).await? {
            // Rebuilt on the server meanwhile: the rest waits for the sync.
            return Ok(done.report);
        }
        engine.read(move |db| db.ops().pending(id)).await?
    };
    for batch in batches(&ops) {
        let uids: Vec<u32> = batch.iter().map(|op| op.uid).collect();
        let outcome = match &batch[0].kind {
            ReplayKind::Flags { add, remove } => session
                .uid_store(&uids, *add, *remove)
                .await
                .map(|()| Outcome::Done),
            ReplayKind::Delete => session.uid_delete(&uids).await.map(|()| Outcome::Done),
            &ReplayKind::Move {
                to,
                ref remote_name,
            } => {
                if to == id {
                    // Moved back before it left: settled locally.
                    Ok(Outcome::Moved(to, None))
                } else if session.can_move() {
                    session
                        .uid_move(&uids, remote_name)
                        .await
                        .map(|copied| Outcome::Moved(to, copied.map(moved)))
                } else {
                    copy_move(&mut done, session, batch, to, remote_name).await?;
                    continue;
                }
            }
        };
        done.outcome(batch, outcome).await?;
    }
    Ok(done.report)
}

/// What became of a batch of ops on the server.
enum Outcome {
    /// Made.
    Done,
    /// Moved to the folder, at the new UIDs if the server said.
    Moved(FolderId, Option<Moved>),
    /// Refused; the text says why.
    Refused(String),
}

/// Records what became of ops of one folder.
struct Settle<'a> {
    engine: &'a Arc<Engine>,
    folder: FolderId,
    uidvalidity: u32,
    touched: &'a mut Touched,
    report: ReplayReport,
}

impl Settle<'_> {
    /// Records the outcome, or, for a lost connection, the attempt; that error is then
    /// returned.
    async fn outcome(
        &mut self,
        ops: &[ReplayOp],
        outcome: catmail_proto::imap::Result<Outcome>,
    ) -> Result<()> {
        let outcome = match outcome {
            Ok(outcome) => outcome,
            // Refused; the connection is fine.
            Err(e) if !e.is_fatal() => Outcome::Refused(e.to_string()),
            Err(e) => {
                let (ops, error) = (ops.to_vec(), e.to_string());
                if let Err(record) = self
                    .engine
                    .write(move |db| db.ops().attempted(&ops, &error))
                    .await
                {
                    tracing::warn!(error = %record, "failed to record the attempt");
                }
                return Err(e.into());
            }
        };
        self.settle(ops, outcome).await
    }

    async fn settle(&mut self, ops: &[ReplayOp], outcome: Outcome) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let (id, uidvalidity, n) = (self.folder, self.uidvalidity, ops.len() as u32);
        let ops = ops.to_vec();
        let changed = match outcome {
            Outcome::Done => {
                self.report.applied += n;
                self.engine.write(move |db| db.ops().applied(&ops)).await?;
                return Ok(());
            }
            Outcome::Moved(to, moved) => {
                self.report.applied += n;
                self.engine
                    .write(move |db| db.ops().moved(id, uidvalidity, to, &ops, moved.as_ref()))
                    .await?
            }
            Outcome::Refused(error) => {
                tracing::warn!(folder = %id, %error, "server refused changes");
                self.report.failed += n;
                self.engine
                    .write(move |db| db.ops().failed(id, &ops, &error))
                    .await?
            }
        };
        self.touched.extend(changed);
        Ok(())
    }
}

fn moved(copied: CopyUid) -> Moved {
    Moved {
        uidvalidity: copied.uidvalidity,
        uids: copied.uids.into_iter().collect(),
    }
}

/// A move on a server without MOVE: COPY, then delete the original. A COPY repeated after
/// its answer was lost would make a second copy, so the target's UIDNEXT is committed
/// before each COPY (see [`CopyCheckpoint`]); [`settle_copies`] looks for the copy from
/// it before anything else is done with the op.
async fn copy_move(
    done: &mut Settle<'_>,
    session: &mut Session,
    ops: &[ReplayOp],
    to: FolderId,
    target: &str,
) -> Result<()> {
    let checkpoint = match session.status(target).await {
        Ok((uidvalidity, uidnext)) => CopyCheckpoint {
            folder: to,
            remote_name: Some(target.to_owned()),
            uidvalidity,
            uidnext,
        },
        Err(e) => return done.outcome(ops, Err(e)).await,
    };
    let (checkpointed, copy_uidvalidity) = (ops.to_vec(), checkpoint.uidvalidity);
    done.engine
        .write_durable(move |db| db.ops().before_copy(&checkpointed, &checkpoint))
        .await?;
    let uids: Vec<u32> = ops.iter().map(|op| op.uid).collect();
    // A refused COPY copied nothing (RFC 3501), so failing the ops leaves no copy behind.
    let copied = match session.uid_copy(&uids, target).await {
        Ok(copied) => copied,
        Err(e) => return done.outcome(ops, Err(e)).await,
    };
    // Which message is the copy, for when the answer to the delete is lost: the original
    // is gone then, and nothing else tells the copy apart.
    if let Some(copied) = copied
        .as_ref()
        .filter(|c| c.uidvalidity == copy_uidvalidity)
    {
        let by_uid: HashMap<u32, u32> = copied.uids.iter().copied().collect();
        let recorded: Vec<(PendingOpId, u32)> = ops
            .iter()
            .filter_map(|op| Some((op.id, *by_uid.get(&op.uid)?)))
            .collect();
        done.engine
            .write_durable(move |db| db.ops().copied(&recorded))
            .await?;
    }
    let outcome = match session.uid_delete(&uids).await {
        Ok(()) => Ok(Outcome::Moved(to, copied.map(moved))),
        // Copied, but the original stays: the op fails and the message shows in both.
        Err(e) if !e.is_fatal() => Ok(Outcome::Refused(format!(
            "copied, but the original couldn't be deleted: {e}"
        ))),
        // The copy is there; the next try finds it from the checkpoint.
        Err(e) => Err(e),
    };
    done.outcome(ops, outcome).await
}

/// Finds out what earlier COPYs (see [`copy_move`]) did for these ops, whatever they
/// were changed into since. A copy found means the message is in its target now: the
/// original is deleted, and the op follows the message there, as for a move in flight
/// (a move back or a delete then undoes the copy). No copy: the op goes on as it is now.
/// If that can't be told, the op fails, leaving the original where it is: never a lost
/// message, never a second copy made on its own. Returns false if the folder was rebuilt
/// on the server meanwhile.
async fn settle_copies(
    done: &mut Settle<'_>,
    session: &mut Session,
    folder: &ReplayFolder,
    ops: Vec<(ReplayOp, CopyCheckpoint)>,
) -> Result<bool> {
    let all: Vec<ReplayOp> = ops.iter().map(|(op, _)| op.clone()).collect();
    let found = match find_copies(session, folder, done.uidvalidity, ops).await {
        Ok(Some(found)) => found,
        Ok(None) => return Ok(false),
        Err(e) => return done.outcome(&all, Err(e)).await.map(|()| true),
    };
    let absent = found.absent;
    done.engine
        .write(move |db| db.ops().forget_checkpoints(&absent))
        .await?;
    let unknown = "can't tell whether an earlier copy was made: the target folder was rebuilt \
                   or deleted on the server since. The message was left where it was; move it \
                   again if it isn't in the target";
    done.settle(&found.unknown, Outcome::Refused(unknown.into()))
        .await?;
    let lost = "the message was being moved when the connection dropped, and its copy in the \
                target folder can't be told apart from other messages there: check that folder";
    done.settle(&found.lost, Outcome::Refused(lost.into()))
        .await?;
    // Copied, or deleted after the copy was made: what is left is deleting it.
    let settled: Vec<ReplayOp> = found
        .copied
        .iter()
        .flat_map(|c| c.ops.iter().cloned())
        .chain(found.gone.values().flatten().cloned())
        .collect();
    let originals: Vec<u32> = settled.iter().map(|op| op.uid).collect();
    if let Err(e) = session.uid_delete(&originals).await {
        return done.outcome(&settled, Err(e)).await.map(|()| true);
    }
    for copied in found.copied {
        done.settle(&copied.ops, Outcome::Moved(copied.into, Some(copied.moved)))
            .await?;
    }
    for (into, ops) in found.gone {
        done.settle(&ops, Outcome::Moved(into, None)).await?;
    }
    Ok(true)
}

/// Copies found in one target.
struct Copied {
    into: FolderId,
    /// Where each op's message is there.
    moved: Moved,
    ops: Vec<ReplayOp>,
}

/// Where earlier COPYs left the messages of ops with a checkpoint.
#[derive(Default)]
struct Copies {
    copied: Vec<Copied>,
    /// Not in the target: no copy was made (or it was removed since).
    absent: Vec<ReplayOp>,
    /// By the target of the copy: the original is gone, and so is the copy (or the op is
    /// still the move that made it, which the next sync of the target catches up with).
    gone: BTreeMap<FolderId, Vec<ReplayOp>>,
    /// The original is gone, and the op was changed since (moved back or on, deleted),
    /// but which message is the copy isn't known: it may be in the target still.
    lost: Vec<ReplayOp>,
    /// The target was rebuilt or deleted since: no telling.
    unknown: Vec<ReplayOp>,
}

/// The originals of ops with one checkpoint.
#[derive(Default)]
struct Target {
    /// Whose COPY was answered with the copy's UID: that UID, and whether the original is
    /// still there.
    recorded: Vec<(ReplayOp, u32, bool)>,
    /// Still there, the answer to their COPY (if one was sent) lost: with their sizes.
    present: Vec<(ReplayOp, u32)>,
}

/// Looks for each message's copy among the target's messages from the checkpoint on.
/// One the COPY's answer named (COPYUID) is that one, if it is still there. Otherwise a
/// copy has the original's raw bytes; sizes are compared first, so only messages that
/// might be one are downloaded, and each copy is claimed once. An original that is gone
/// without such an answer can't be matched safely: nothing else tells its copy apart
/// from another message (with the same Message-ID, say). Leaves `folder` selected again;
/// `None` if it was rebuilt on the server meanwhile.
async fn find_copies(
    session: &mut Session,
    folder: &ReplayFolder,
    uidvalidity: u32,
    ops: Vec<(ReplayOp, CopyCheckpoint)>,
) -> catmail_proto::imap::Result<Option<Copies>> {
    let mut copies = Copies::default();
    let uids: Vec<u32> = ops.iter().map(|(op, _)| op.uid).collect();
    let sizes = session.fetch_sizes(&uids).await?;
    let mut by_target: BTreeMap<CopyCheckpoint, Target> = BTreeMap::new();
    for (op, checkpoint) in ops {
        let size = sizes.get(&op.uid).copied();
        match (op.copy_uid, size) {
            (Some(copy), _) => {
                let target = by_target.entry(checkpoint).or_default();
                target.recorded.push((op, copy, size.is_some()));
            }
            (None, Some(size)) => by_target
                .entry(checkpoint)
                .or_default()
                .present
                .push((op, size)),
            (None, None) => gone_unseen(&mut copies, &checkpoint, op),
        }
    }
    // (checkpoint, originals to compare, candidates with their sizes and bytes, ops whose
    // copies were found, and where)
    let mut targets = Vec::new();
    for (checkpoint, target) in by_target {
        let unknown = match &checkpoint.remote_name {
            Some(name) => match session.examine(name).await {
                Ok(status) => status.uidvalidity != checkpoint.uidvalidity,
                Err(e) if !e.is_fatal() => true,
                Err(e) => return Err(e),
            },
            None => true,
        };
        if unknown {
            for (op, _, present) in target.recorded {
                if present {
                    copies.unknown.push(op);
                } else {
                    gone_unseen(&mut copies, &checkpoint, op);
                }
            }
            copies
                .unknown
                .extend(target.present.into_iter().map(|(op, _)| op));
            continue;
        }
        let arrived: HashSet<u32> = session
            .uid_search_from(checkpoint.uidnext)
            .await?
            .into_iter()
            .collect();
        let (mut found, mut moved) = (Vec::new(), HashMap::new());
        for (op, copy, present) in target.recorded {
            if arrived.contains(&copy) {
                moved.insert(op.uid, copy);
                found.push(op);
            } else if present {
                // Removed since: as if it had never been made.
                copies.absent.push(op);
            } else {
                copies.gone.entry(checkpoint.folder).or_default().push(op);
            }
        }
        let wanted: HashSet<u32> = target.present.iter().map(|(_, size)| *size).collect();
        let mut candidates = Vec::new();
        if !wanted.is_empty() {
            let claimed: HashSet<u32> = moved.values().copied().collect();
            let unclaimed: Vec<u32> = arrived.difference(&claimed).copied().collect();
            for (uid, size) in session.fetch_sizes(&unclaimed).await? {
                if wanted.contains(&size)
                    && let Some(raw) = session.fetch_raw(uid).await?
                {
                    candidates.push((uid, size, raw));
                }
            }
        }
        targets.push((checkpoint, target.present, candidates, found, moved));
    }
    if session.select(&folder.remote_name).await?.uidvalidity != uidvalidity {
        return Ok(None);
    }
    for (checkpoint, present, mut candidates, mut found, mut uids) in targets {
        for (op, size) in present {
            // Nothing of its size arrived: no copy, and no need to download it.
            if !candidates.iter().any(|(_, s, _)| *s == size) {
                copies.absent.push(op);
                continue;
            }
            let Some(raw) = session.fetch_raw(op.uid).await? else {
                gone_unseen(&mut copies, &checkpoint, op);
                continue;
            };
            match candidates.iter().position(|(_, _, c)| *c == raw) {
                Some(i) => {
                    uids.insert(op.uid, candidates.swap_remove(i).0);
                    found.push(op);
                }
                None => copies.absent.push(op),
            }
        }
        if !found.is_empty() {
            copies.copied.push(Copied {
                into: checkpoint.folder,
                moved: Moved {
                    uidvalidity: checkpoint.uidvalidity,
                    uids,
                },
                ops: found,
            });
        }
    }
    Ok(Some(copies))
}

/// Files an op whose original is gone, when its copy can't be looked for. If the op is
/// still the move that made the copy, the next sync of the target finds it there; if it
/// was changed since, the copy may be left behind.
fn gone_unseen(copies: &mut Copies, checkpoint: &CopyCheckpoint, op: ReplayOp) {
    match op.kind {
        ReplayKind::Move { to, .. } if to == checkpoint.folder => {
            copies.gone.entry(checkpoint.folder).or_default().push(op);
        }
        _ => copies.lost.push(op),
    }
}

/// Runs of consecutive ops that one command can make: the same change, at most
/// [`BATCH`] long.
fn batches(ops: &[ReplayOp]) -> impl Iterator<Item = &[ReplayOp]> {
    ops.chunk_by(|a, b| a.kind == b.kind)
        .flat_map(|run| run.chunks(BATCH))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_follow_the_change_and_size() {
        let op = |id: i64, kind: &ReplayKind| ReplayOp {
            id: PendingOpId(id),
            uid: id as u32,
            kind: kind.clone(),
            copy: None,
            copy_uid: None,
        };
        let read = ReplayKind::Flags {
            add: Flags::SEEN,
            remove: Flags::empty(),
        };
        let to = |id: i64| ReplayKind::Move {
            to: FolderId(id),
            remote_name: format!("F{id}"),
        };
        let mut ops: Vec<ReplayOp> = (1..=BATCH as i64 + 1).map(|id| op(id, &read)).collect();
        ops.push(op(1000, &ReplayKind::Delete));
        ops.push(op(1001, &to(1)));
        ops.push(op(1002, &to(2)));
        let sizes: Vec<usize> = batches(&ops).map(<[ReplayOp]>::len).collect();
        assert_eq!(sizes, [BATCH, 1, 1, 1, 1]);
    }
}
