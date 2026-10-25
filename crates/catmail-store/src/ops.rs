//! Local changes to messages and the queue that carries them to the server; see the
//! `pending_ops` schema.
//!
//! Every change is made to the local rows at once and queued in the same transaction,
//! one op per message. Replay asks for a folder's ops, makes them on the server, and
//! reports back with [`applied`](PendingOps::applied), [`moved`](PendingOps::moved) or
//! [`failed`](PendingOps::failed). The user may keep changing a message while its op is
//! being replayed (moving a moved message on, say), so those only settle ops that are
//! still as they were replayed, and point whatever else names the message's old place
//! at its new one.

use std::collections::{BTreeSet, HashMap, hash_map::Entry};

use catmail_types::{
    AccountId, DeleteOutcome, Flags, Folder, FolderId, FolderKind, GmailLabel, MessageId, MovePlan,
    OpKind, OpStatus, PendingOp, PendingOpId, Unmovable,
};
use rusqlite::{Connection, OptionalExtension, Params, Row, params};

use crate::columns::{conversion_error, parse_opt_text, parse_text};
use crate::folders::Folders;
use crate::messages::shown;
use crate::{Error, Result};

/// What a change did, for change events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Touched {
    /// Folders whose messages it changed.
    pub folders: BTreeSet<(AccountId, FolderId)>,
    /// Accounts some of whose queued changes it made fail or turned into conflicts.
    pub refused: BTreeSet<AccountId>,
}

impl Touched {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.folders.is_empty() && self.refused.is_empty()
    }

    /// Adds what `other` did.
    pub fn merge(&mut self, other: Touched) {
        self.folders.extend(other.folders);
        self.refused.extend(other.refused);
    }
}

impl FromIterator<(AccountId, FolderId)> for Touched {
    fn from_iter<I: IntoIterator<Item = (AccountId, FolderId)>>(folders: I) -> Self {
        Self {
            folders: folders.into_iter().collect(),
            refused: BTreeSet::new(),
        }
    }
}

impl<const N: usize> From<[(AccountId, FolderId); N]> for Touched {
    fn from(folders: [(AccountId, FolderId); N]) -> Self {
        folders.into_iter().collect()
    }
}

/// The flags a user may change. `\Deleted` goes with deletion and `\Draft` with drafts.
const USER_FLAGS: Flags =
    Flags::from_bits(Flags::SEEN.bits() | Flags::FLAGGED.bits() | Flags::ANSWERED.bits());

/// Joins a message `m` to the pending move `o` that put it where it is, if it has no
/// UID yet: the server still has it at the move's (folder, UIDVALIDITY, UID).
pub(crate) const MOVING: &str = "LEFT JOIN pending_ops o ON m.uid IS NULL AND o.message_id = m.id
     AND o.kind = 'move' AND o.status = 'pending'";

/// Whether op `o` is a label change waiting for an earlier pending move of its message.
const WAITING: &str = "(o.kind = 'label' AND EXISTS (
         SELECT 1 FROM pending_ops w
         WHERE w.folder_id = o.folder_id AND w.uidvalidity = o.uidvalidity AND w.uid = o.uid
           AND w.kind = 'move' AND w.status = 'pending' AND w.id < o.id))";

/// An op `o` of account `?1` (any account if NULL) that failed or conflicted.
const SETTLED: &str = "o.status <> 'pending' AND (?1 IS NULL OR o.account_id = ?1)";

/// A queued op, as replay needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayOp {
    pub id: PendingOpId,
    /// The generation `uid` is of: the op is made only if the server reports it.
    pub uidvalidity: u32,
    pub uid: u32,
    pub kind: ReplayKind,
    /// For a move on a server without MOVE: where an earlier COPY may have put it.
    pub copy: Option<CopyCheckpoint>,
    /// With `copy`: the copy's UID there, if the COPY was answered with COPYUID.
    pub copy_uid: Option<u32>,
    /// For a label change: the earlier changes of the same label of the message, oldest
    /// first, which this one makes moot. They are settled with it.
    pub superseded: Vec<ReplayOp>,
}

impl ReplayOp {
    /// The ops settled when this one is: those it supersedes, oldest first, then itself.
    pub fn settled(&self) -> impl Iterator<Item = &ReplayOp> {
        self.superseded.iter().chain(std::iter::once(self))
    }
}

/// The target of a COPY as it was right before the COPY was sent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CopyCheckpoint {
    pub folder: FolderId,
    /// `None` if the folder is gone since.
    pub remote_name: Option<String>,
    pub uidvalidity: u32,
    /// The copy, if made, has a UID from here on.
    pub uidnext: u32,
}

/// [`OpKind`], with the target's name on the server for a move, and the label as
/// X-GM-LABELS names it for a label change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayKind {
    Flags {
        add: Flags,
        remove: Flags,
    },
    Move {
        to: FolderId,
        remote_name: String,
    },
    Delete,
    Label {
        label: FolderId,
        name: GmailLabel,
        add: bool,
    },
}

#[cfg(test)]
impl ReplayKind {
    fn op_kind(&self) -> OpKind {
        match *self {
            Self::Flags { add, remove } => OpKind::Flags { add, remove },
            Self::Move { to, .. } => OpKind::Move { to },
            Self::Delete => OpKind::Delete,
            Self::Label { label, add, .. } => OpKind::Label { label, add },
        }
    }
}

/// A folder with ops to replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayFolder {
    pub id: FolderId,
    pub name: String,
    pub remote_name: String,
    /// Those [`pending`](PendingOps::pending) gives now.
    pub ops: Vec<PendingOpId>,
}

/// What the server said a move did (COPYUID): the target's UIDVALIDITY, and the new
/// UID of each old one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub uidvalidity: u32,
    pub uids: HashMap<u32, u32>,
}

const COLUMNS: &str = "o.id, o.account_id, o.folder_id, o.message_id, o.kind, o.flags_add,
    o.flags_remove, o.target_folder_id, o.label_add, o.status, o.attempts, o.last_error,
    o.created_at, m.subject";

/// A message as a local change sees it.
struct Local {
    id: MessageId,
    account: AccountId,
    folder: FolderId,
    flags: Flags,
    /// Where the server has it: (folder, UIDVALIDITY, UID).
    server: Option<(FolderId, u32, u32)>,
    /// The pending move that put it here, if it has no UID yet.
    moving: Option<PendingOpId>,
}

pub struct PendingOps<'c> {
    conn: &'c Connection,
}

impl<'c> PendingOps<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Sets `add` and clears `remove` (`\Seen`, `\Flagged`, `\Answered`) on the
    /// messages, and queues what actually changed.
    pub fn set_flags(
        &self,
        messages: &[MessageId],
        add: Flags,
        remove: Flags,
        now: i64,
    ) -> Result<Touched> {
        if !USER_FLAGS.contains(add | remove) {
            return Err(Error::Invalid(
                "only \\Seen, \\Flagged and \\Answered can be changed".into(),
            ));
        }
        if !add.intersection(remove).is_empty() {
            return Err(Error::Invalid(
                "a flag can't be both set and cleared".into(),
            ));
        }
        let mut touched = Touched::new();
        for &id in messages {
            let m = self.local(id)?;
            let (add, remove) = (add.difference(m.flags), remove.intersection(m.flags));
            if add.is_empty() && remove.is_empty() {
                continue;
            }
            // Starred shows it before or after.
            let flagged = (m.flags | add).contains(Flags::FLAGGED);
            self.touch_shown(id, flagged, &mut touched)?;
            self.conn
                .prepare_cached("UPDATE messages SET flags = ?2 WHERE id = ?1")?
                .execute(params![id.0, m.flags.apply(add, remove).bits()])?;
            // A message moved here locally is still where its move says, and the flag
            // change goes there; it is replayed before the move.
            self.queue(&m, OpKind::Flags { add, remove }, now)?;
            touched.folders.insert((m.account, m.folder));
        }
        Ok(touched)
    }

    /// Moves the messages, looked at in `shown_in` (see [`MovePlan::of`]), to `to`, a
    /// folder of their account. On a standard account they show there at once, without
    /// a UID until the move is made on the server; on Gmail a move between labels
    /// changes their labels.
    pub fn move_messages(
        &self,
        messages: &[MessageId],
        shown_in: Option<FolderId>,
        to: FolderId,
        now: i64,
    ) -> Result<Touched> {
        let mut touched = Touched::new();
        let mut folders = HashMap::new();
        for &id in messages {
            let m = self.local(id)?;
            let folders = self.folders_of(&mut folders, m.account)?;
            let plan = MovePlan::of(folders, m.folder, shown_in, to).map_err(|e| {
                Error::Invalid(match e {
                    Unmovable::NotFound => {
                        format!("folder {to} not found in message {id}'s account")
                    }
                    Unmovable::HoldsNoMessages => format!("folder {to} can't hold messages"),
                    Unmovable::Starred => "messages are starred, not moved to Starred".into(),
                })
            })?;
            self.carry_out(&m, plan, now, &mut touched)?;
        }
        Ok(touched)
    }

    /// Archives the messages ([`MovePlan::archive`]): moves them to their account's
    /// Archive folder, those not in an Archive folder already, or on Gmail takes
    /// `\Inbox` off. Fails, changing nothing, if a standard account has no Archive
    /// folder.
    pub fn archive_messages(&self, messages: &[MessageId], now: i64) -> Result<Touched> {
        let mut touched = Touched::new();
        let mut folders = HashMap::new();
        for &id in messages {
            let m = self.local(id)?;
            let folders = self.folders_of(&mut folders, m.account)?;
            if !MovePlan::can_archive(folders) {
                return Err(Error::Invalid("the account has no Archive folder".into()));
            }
            if let Some(plan) = MovePlan::archive(folders, m.folder) {
                self.carry_out(&m, plan, now, &mut touched)?;
            }
        }
        Ok(touched)
    }

    /// Adds the Gmail labels `add` to the messages and takes `remove` off, queueing what
    /// actually changed. Starred is no label: it is `\Flagged`.
    pub fn label_messages(
        &self,
        messages: &[MessageId],
        add: &[FolderId],
        remove: &[FolderId],
        now: i64,
    ) -> Result<Touched> {
        if add.iter().any(|label| remove.contains(label)) {
            return Err(Error::Invalid(
                "a label can't be both added and taken off".into(),
            ));
        }
        let mut touched = Touched::new();
        let mut folders = HashMap::new();
        for &id in messages {
            let m = self.local(id)?;
            for &label in add.iter().chain(remove) {
                match self
                    .folders_of(&mut folders, m.account)?
                    .iter()
                    .find(|f| f.id == label)
                {
                    Some(f) if f.is_label() => {}
                    Some(f) if f.kind == FolderKind::Label => {
                        return Err(Error::Invalid(format!(
                            "{} is \\Flagged, not a label: star the messages instead",
                            f.name
                        )));
                    }
                    Some(f) => return Err(Error::Invalid(format!("{} is not a label", f.name))),
                    None => {
                        return Err(Error::Invalid(format!(
                            "folder {label} not found in message {id}'s account"
                        )));
                    }
                }
            }
            let mut changed = false;
            for &label in remove {
                changed |= self.label_one(&m, m.folder, label, false, now, &mut touched)?;
            }
            for &label in add {
                changed |= self.label_one(&m, m.folder, label, true, now, &mut touched)?;
            }
            if changed {
                self.touch_shown(m.id, false, &mut touched)?;
            }
        }
        Ok(touched)
    }

    /// Moves the message, then changes its labels, as `plan` says: changed after the move,
    /// they are made after it (see `pending`), and undone with it if it is refused.
    fn carry_out(&self, m: &Local, plan: MovePlan, now: i64, touched: &mut Touched) -> Result<()> {
        if let Some(to) = plan.to {
            self.move_one(m, to, now, touched)?;
        }
        let folder = plan.to.unwrap_or(m.folder);
        let mut changed = false;
        for label in plan.remove {
            changed |= self.label_one(m, folder, label, false, now, touched)?;
        }
        if let Some(label) = plan.add {
            changed |= self.label_one(m, folder, label, true, now, touched)?;
        }
        // A move noted them already.
        if changed && plan.to.is_none() {
            self.touch_shown(m.id, false, touched)?;
        }
        Ok(())
    }

    /// Adds `label` to the message, now in `folder`, or takes it off, and queues that if it
    /// changed; returns whether it did. The caller notes the other labels that list the
    /// message ([`touch_shown`](Self::touch_shown)), once for all its changes. The op goes
    /// where the server has the message; made after a move of it
    /// (`m` is as it was before), it waits for the move and follows it (see `pending`).
    fn label_one(
        &self,
        m: &Local,
        folder: FolderId,
        label: FolderId,
        add: bool,
        now: i64,
        touched: &mut Touched,
    ) -> Result<bool> {
        if !self.set_label(m.id, label, add)? {
            return Ok(false);
        }
        touched
            .folders
            .extend([(m.account, label), (m.account, folder)]);
        self.queue(m, OpKind::Label { label, add }, now)?;
        Ok(true)
    }

    /// Adds `label` to message `row` or takes it off. Returns whether that changed it.
    fn set_label(&self, row: MessageId, label: FolderId, add: bool) -> Result<bool> {
        let changed = if add {
            self.conn
                .prepare_cached(
                    "INSERT OR IGNORE INTO message_labels (message_id, folder_id, account_id,
                                                           list_date)
                     SELECT id, ?2, account_id, list_date FROM messages WHERE id = ?1",
                )?
                .execute(params![row.0, label.0])?
        } else {
            self.conn
                .prepare_cached(
                    "DELETE FROM message_labels WHERE message_id = ?1 AND folder_id = ?2",
                )?
                .execute(params![row.0, label.0])?
        };
        Ok(changed > 0)
    }

    /// Moves the messages to their account's Trash, or deletes them for good if they are
    /// in Trash already or `permanently` is set ([`DeleteOutcome`]). Without a Trash
    /// folder, only permanent deletion is possible.
    pub fn delete_messages(
        &self,
        messages: &[MessageId],
        permanently: bool,
        now: i64,
    ) -> Result<Touched> {
        let mut touched = Touched::new();
        let mut folders = HashMap::new();
        for &id in messages {
            let m = self.local(id)?;
            touched.folders.insert((m.account, m.folder));
            let folders = self.folders_of(&mut folders, m.account)?;
            let outcome = if permanently {
                DeleteOutcome::Permanent
            } else {
                DeleteOutcome::of(folders, m.folder)
            };
            match outcome {
                DeleteOutcome::ToTrash(trash) => {
                    // On Gmail it loses INBOX there too.
                    let plan = MovePlan::of(folders, m.folder, None, trash).map_err(|_| {
                        Error::Invalid("the Trash folder can't hold messages".into())
                    })?;
                    self.carry_out(&m, plan, now, &mut touched)?;
                    continue;
                }
                DeleteOutcome::NoTrash => {
                    return Err(Error::Invalid(
                        "the account has no Trash folder; delete permanently instead".into(),
                    ));
                }
                DeleteOutcome::Permanent => {}
            }
            match m.moving {
                // Never made it to the target: delete it where the server still has it.
                Some(op) => {
                    self.conn
                        .prepare_cached(
                            "UPDATE pending_ops SET kind = 'delete', target_folder_id = NULL
                             WHERE id = ?1",
                        )?
                        .execute([op.0])?;
                }
                None => self.queue(&m, OpKind::Delete, now)?,
            }
            self.touch_shown(id, false, &mut touched)?;
            self.conn
                .prepare_cached("DELETE FROM messages WHERE id = ?1")?
                .execute([id.0])?;
        }
        Ok(touched)
    }

    /// Deletes for good the message the server has at `uid` in `folder`, under
    /// `uidvalidity`, whether or not a sync has stored it: a draft's copy that a newer
    /// one replaces, say. Its row goes too, if it is of that generation. Left alone if
    /// the user moved or deleted it already. Returns the folder if an op was queued,
    /// even if no row changed: announcing it gets the op replayed.
    pub(crate) fn delete_on_server(
        &self,
        account: AccountId,
        folder: FolderId,
        uidvalidity: u32,
        uid: u32,
        now: i64,
    ) -> Result<Touched> {
        let taken: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pending_ops
                            WHERE folder_id = ?1 AND uidvalidity = ?2 AND uid = ?3
                              AND kind IN ('move', 'delete') AND status = 'pending')",
            params![folder.0, uidvalidity, uid],
            |r| r.get(0),
        )?;
        if taken {
            return Ok(Touched::new());
        }
        let row: Option<i64> = self
            .conn
            .query_row(
                "SELECT m.id FROM messages m JOIN folders f ON f.id = m.folder_id
                 WHERE m.folder_id = ?1 AND m.uid = ?3 AND f.uidvalidity = ?2",
                params![folder.0, uidvalidity, uid],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(row) = row {
            return self.delete_messages(&[MessageId(row)], true, now);
        }
        self.insert(
            account,
            (folder, uidvalidity, uid),
            OpKind::Delete,
            None,
            now,
        )?;
        Ok(Touched::from([(account, folder)]))
    }

    /// The account's folders, which the rules about special folders apply to, listed
    /// once per account.
    fn folders_of<'a>(
        &self,
        cache: &'a mut HashMap<AccountId, Vec<Folder>>,
        account: AccountId,
    ) -> Result<&'a [Folder]> {
        Ok(match cache.entry(account) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(Folders::new(self.conn).list(account)?),
        })
    }

    /// Moves the message to `to`, recording in `touched` the folders that lost and got
    /// it: on Gmail, a label shows it no more, or again, as it leaves or enters All Mail.
    fn move_one(&self, m: &Local, to: FolderId, now: i64, touched: &mut Touched) -> Result<()> {
        if m.folder == to {
            return Ok(());
        }
        touched
            .folders
            .extend([(m.account, m.folder), (m.account, to)]);
        // Its labels and Starred show it no more, or again, as it leaves or enters All
        // Mail; they are the same either side of the move.
        self.touch_shown(m.id, false, touched)?;
        match m.moving {
            // Still on its way: send it to the new place instead. Back to where the server
            // has it, the op stays, and is resolved locally at replay; it may be in flight.
            Some(op) => {
                self.conn
                    .prepare_cached("UPDATE pending_ops SET target_folder_id = ?2 WHERE id = ?1")?
                    .execute(params![op.0, to.0])?;
            }
            None => self.queue(m, OpKind::Move { to }, now)?,
        }
        // MODSEQ is per mailbox.
        self.conn
            .prepare_cached(
                "UPDATE messages SET folder_id = ?2, uid = NULL, modseq = NULL WHERE id = ?1",
            )?
            .execute(params![m.id.0, to.0])?;
        Ok(())
    }

    /// Queues `kind` for the message, where the server has it.
    fn queue(&self, m: &Local, kind: OpKind, now: i64) -> Result<()> {
        // Every row has a UID or a pending move; the repository keeps it so.
        let server = m.server.ok_or_else(|| {
            Error::Conflict(format!(
                "message {} is not on the server any more; sync and try again",
                m.id
            ))
        })?;
        self.insert(m.account, server, kind, Some(m.id), now)
    }

    /// Queues `kind` for the message the server has at `(folder, UIDVALIDITY, UID)`,
    /// shown locally as `message` if there is such a row.
    fn insert(
        &self,
        account: AccountId,
        (folder, uidvalidity, uid): (FolderId, u32, u32),
        kind: OpKind,
        message: Option<MessageId>,
        now: i64,
    ) -> Result<()> {
        let none = Flags::empty();
        let (name, add, remove, target, label_add) = match kind {
            OpKind::Flags { add, remove } => ("flags", add, remove, None, None),
            OpKind::Move { to } => ("move", none, none, Some(to.0), None),
            OpKind::Delete => ("delete", none, none, None, None),
            OpKind::Label { label, add } => ("label", none, none, Some(label.0), Some(add)),
        };
        self.conn
            .prepare_cached(
                "INSERT INTO pending_ops (account_id, folder_id, uidvalidity, uid, kind, flags_add,
                                          flags_remove, target_folder_id, label_add, message_id,
                                          created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?
            .execute(params![
                account.0,
                folder.0,
                uidvalidity,
                uid,
                name,
                add.bits(),
                remove.bits(),
                target,
                label_add,
                message.map(|m| m.0),
                now
            ])?;
        Ok(())
    }

    /// Records in `touched` the folders that show the message besides its location
    /// ([`shown`]), before a change to it.
    fn touch_shown(&self, id: MessageId, flagged: bool, touched: &mut Touched) -> Result<()> {
        touched.folders.extend(shown(self.conn, id.0, flagged)?);
        Ok(())
    }

    fn local(&self, id: MessageId) -> Result<Local> {
        self.conn
            .prepare_cached(&format!(
                "SELECT m.account_id, m.folder_id, m.uid, m.flags, f.uidvalidity,
                        o.id AS op, o.folder_id AS op_folder, o.uidvalidity AS op_uidvalidity,
                        o.uid AS op_uid
                 FROM messages m
                 JOIN folders f ON f.id = m.folder_id
                 {MOVING}
                 WHERE m.id = ?1"
            ))?
            .query_row([id.0], |r| {
                let (uid, uidvalidity): (Option<u32>, Option<u32>) = (r.get(2)?, r.get(4)?);
                let op: Option<i64> = r.get("op")?;
                let server = match (uid, uidvalidity, op) {
                    (Some(uid), Some(uidvalidity), _) => {
                        Some((FolderId(r.get(1)?), uidvalidity, uid))
                    }
                    (None, _, Some(_)) => Some((
                        FolderId(r.get("op_folder")?),
                        r.get("op_uidvalidity")?,
                        r.get("op_uid")?,
                    )),
                    _ => None,
                };
                Ok(Local {
                    id,
                    account: AccountId(r.get(0)?),
                    folder: FolderId(r.get(1)?),
                    flags: Flags::from_bits(r.get(3)?),
                    server,
                    moving: op.map(PendingOpId),
                })
            })
            .optional()?
            .ok_or(Error::MessageNotFound(id))
    }

    /// The account's queued changes: pending ones and those that failed or conflicted,
    /// oldest first; every account's with `None`.
    pub fn list(&self, account: Option<AccountId>) -> Result<Vec<PendingOp>> {
        self.select("?1 IS NULL OR o.account_id = ?1", account)
    }

    /// The ops `o` matching `filter`, whose `?1` is `account`, oldest first.
    fn select(&self, filter: &str, account: Option<AccountId>) -> Result<Vec<PendingOp>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM pending_ops o LEFT JOIN messages m ON m.id = o.message_id
             WHERE {filter} ORDER BY o.id"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([account.map(|a| a.0)], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Forgets a failed or conflicting op. Returns its account, or `None` if there is no
    /// such op. A pending one can't be dismissed: its change is already made locally.
    pub fn dismiss(&self, id: PendingOpId) -> Result<Option<AccountId>> {
        let found: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT account_id, status FROM pending_ops WHERE id = ?1",
                [id.0],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match found {
            None => Ok(None),
            Some((_, status)) if status == OpStatus::Pending.as_str() => Err(Error::Conflict(
                format!("operation {id} is still pending; it is made at the next sync"),
            )),
            Some((account, _)) => {
                self.conn
                    .execute("DELETE FROM pending_ops WHERE id = ?1", [id.0])?;
                Ok(Some(AccountId(account)))
            }
        }
    }

    /// Whether the account (any account with `None`) has failed or conflicting ops.
    pub fn has_settled(&self, account: Option<AccountId>) -> Result<bool> {
        let sql = format!("SELECT EXISTS (SELECT 1 FROM pending_ops o WHERE {SETTLED})");
        Ok(self
            .conn
            .prepare_cached(&sql)?
            .query_row([account.map(|a| a.0)], |r| r.get(0))?)
    }

    /// Forgets every failed or conflicting op of the account (of every account with
    /// `None`), and returns them, oldest first.
    pub fn dismiss_settled(&self, account: Option<AccountId>) -> Result<Vec<PendingOp>> {
        let settled = self.select(SETTLED, account)?;
        self.conn
            .prepare_cached(&format!("DELETE FROM pending_ops AS o WHERE {SETTLED}"))?
            .execute([account.map(|a| a.0)])?;
        Ok(settled)
    }

    pub fn count_pending(&self, account: AccountId) -> Result<u32> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM pending_ops WHERE account_id = ?1 AND status = 'pending'",
            [account.0],
            |r| r.get(0),
        )?)
    }

    /// Folders of `account` with ops to replay, INBOX first.
    pub fn pending_folders(&self, account: AccountId) -> Result<Vec<ReplayFolder>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT f.id, f.name, f.remote_name, o.id FROM pending_ops o
             JOIN folders f ON f.id = o.folder_id
             WHERE o.account_id = ?1 AND o.status = 'pending' AND NOT {WAITING}
             ORDER BY f.remote_name <> 'INBOX', f.id, o.id"
        ))?;
        let mut folders: Vec<ReplayFolder> = Vec::new();
        let mut rows = stmt.query([account.0])?;
        while let Some(r) = rows.next()? {
            let (id, op) = (FolderId(r.get(0)?), PendingOpId(r.get(3)?));
            match folders.last_mut() {
                Some(last) if last.id == id => last.ops.push(op),
                _ => folders.push(ReplayFolder {
                    id,
                    name: r.get(1)?,
                    remote_name: r.get(2)?,
                    ops: vec![op],
                }),
            }
        }
        Ok(folders)
    }

    /// The folder's ops in replay order, grouped by what they do so that one command can
    /// make each group: flag changes, label changes, then moves and deletes. Only the
    /// order of one message's flag changes, or changes to one of its labels, matters, so
    /// they go in rounds: every message's first change, then every second one, and so on.
    /// A message has at most one pending move or delete (moving or deleting it again
    /// rewrites that op). A label change made after a pending move of its message isn't
    /// here: it waits for the move, and follows the message to its new place.
    ///
    /// Of a message's changes to one label, only the last is made: here they all come
    /// before any move of it, so nothing changes the label on the server between them,
    /// and adding or taking off a label ends the same whatever was done before. The rest
    /// go along ([`ReplayOp::superseded`]). So a restore out of Trash cancelled before it
    /// was made never adds `\Inbox` to a message in Trash.
    pub fn pending(&self, folder: FolderId) -> Result<Vec<ReplayOp>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT o.id, o.uidvalidity, o.uid, o.kind, o.flags_add, o.flags_remove,
                    o.target_folder_id, o.label_add, t.remote_name, t.name, t.role,
                    o.copy_folder_id, c.remote_name AS copy_remote_name, o.copy_uidvalidity,
                    o.copy_uidnext, o.copy_uid
             FROM pending_ops o
             LEFT JOIN folders t ON t.id = o.target_folder_id
             LEFT JOIN folders c ON c.id = o.copy_folder_id
             WHERE o.folder_id = ?1 AND o.status = 'pending' AND NOT {WAITING}
             ORDER BY o.kind <> 'flags', o.kind <> 'label',
                      row_number() OVER (PARTITION BY o.kind, o.uid, o.target_folder_id
                                         ORDER BY o.id),
                      o.kind, o.flags_add, o.flags_remove, o.label_add, o.target_folder_id, o.id"
        ))?;
        let rows = stmt.query_map([folder.0], |r| {
            Ok(ReplayOp {
                id: PendingOpId(r.get("id")?),
                uidvalidity: r.get("uidvalidity")?,
                uid: r.get("uid")?,
                kind: match kind(r)? {
                    OpKind::Flags { add, remove } => ReplayKind::Flags { add, remove },
                    // Ops go with their target folder, so it is there.
                    OpKind::Move { to } => ReplayKind::Move {
                        to,
                        remote_name: r.get("remote_name")?,
                    },
                    OpKind::Delete => ReplayKind::Delete,
                    OpKind::Label { label, add } => ReplayKind::Label {
                        label,
                        name: gmail_label(r)?,
                        add,
                    },
                },
                copy: match r.get::<_, Option<i64>>("copy_folder_id")? {
                    Some(folder) => Some(CopyCheckpoint {
                        folder: FolderId(folder),
                        remote_name: r.get("copy_remote_name")?,
                        uidvalidity: r.get("copy_uidvalidity")?,
                        uidnext: r.get("copy_uidnext")?,
                    }),
                    None => None,
                },
                copy_uid: r.get("copy_uid")?,
                superseded: Vec::new(),
            })
        })?;
        let ops: Vec<ReplayOp> = rows.collect::<rusqlite::Result<_>>()?;
        Ok(supersede(ops))
    }

    /// The server made these flag changes or deletions.
    pub fn applied(&self, ops: &[ReplayOp]) -> Result<()> {
        let mut delete = self
            .conn
            .prepare_cached("DELETE FROM pending_ops WHERE id = ?1 AND status = 'pending'")?;
        for op in ops {
            delete.execute([op.id.0])?;
        }
        Ok(())
    }

    /// The server moved these messages from `folder` (at `uidvalidity`) to `to`, and said
    /// where they ended up if `moved` is set. A move to its own folder is settled here
    /// without asking the server.
    ///
    /// A message whose op is unchanged keeps its row, which gets the new UID; without
    /// one the row is dropped, and the next sync of `to` fetches the message. Ops that
    /// still name the old place (made while the move was in flight, including this op
    /// if it was changed meanwhile) follow the message to its new one, or become
    /// conflicts if that isn't known. Returns the folders whose messages this changed.
    pub fn moved(
        &self,
        folder: FolderId,
        uidvalidity: u32,
        to: FolderId,
        ops: &[ReplayOp],
        moved: Option<&Moved>,
    ) -> Result<Touched> {
        let mut touched = Touched::new();
        let current: Option<u32> = self
            .conn
            .query_row(
                "SELECT uidvalidity FROM folders WHERE id = ?1",
                [to.0],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let moved = moved.filter(|m| Some(m.uidvalidity) == current);
        // (UIDVALIDITY, UID) where the message ended up.
        let place_of = |uid: u32| {
            if to == folder {
                return Some((uidvalidity, uid));
            }
            moved.and_then(|m| Some((m.uidvalidity, *m.uids.get(&uid)?)))
        };
        for op in ops {
            let new_place = place_of(op.uid);
            let row: Option<Option<i64>> = self
                .conn
                .prepare_cached(
                    "DELETE FROM pending_ops
                     WHERE id = ?1 AND status = 'pending' AND kind = 'move'
                       AND target_folder_id = ?2
                     RETURNING message_id",
                )?
                .query_row(params![op.id.0, to.0], |r| r.get(0))
                .optional()?;
            if let Some(Some(row)) = row {
                touched.folders.extend(self.place(
                    MessageId(row),
                    to,
                    new_place.map(|(_, uid)| uid),
                )?);
            }
            match new_place {
                Some((new_uidvalidity, new_uid)) => {
                    self.conn
                        .prepare_cached(
                            "UPDATE pending_ops SET folder_id = ?4, uidvalidity = ?5, uid = ?6,
                                 copy_folder_id = NULL, copy_uidvalidity = NULL,
                                 copy_uidnext = NULL, copy_uid = NULL
                             WHERE folder_id = ?1 AND uidvalidity = ?2 AND uid = ?3
                               AND status = 'pending'",
                        )?
                        .execute(params![
                            folder.0,
                            uidvalidity,
                            op.uid,
                            to.0,
                            new_uidvalidity,
                            new_uid
                        ])?;
                }
                None => touched.merge(self.conflict(
                    "folder_id = ?1 AND uidvalidity = ?2 AND uid = ?3",
                    params![folder.0, uidvalidity, op.uid],
                )?),
            }
        }
        Ok(touched)
    }

    /// The server refused these ops of `folder`. Each is marked failed and its local
    /// change undone: the message's flags (or one of its labels) become what the changes
    /// still pending make of what they were before any of them, a moved message goes
    /// back, and the label changes waiting for its move fail with it. A deleted message
    /// can't be restored here; the next sync fetches it again. A move changed meanwhile
    /// stays pending, for its new target. Returns the folders whose messages this changed.
    pub fn failed(&self, folder: FolderId, ops: &[ReplayOp], error: &str) -> Result<Touched> {
        let mut touched = Touched::new();
        for op in ops {
            // Flag changes and deletes have no target; a move only fails if it still goes
            // where it went when it was refused.
            let target = match op.kind {
                ReplayKind::Move { to, .. } | ReplayKind::Label { label: to, .. } => Some(to),
                _ => None,
            };
            let Some(row) = self.fail(op.id, target, error, &mut touched)? else {
                continue;
            };
            match op.kind {
                ReplayKind::Flags { .. } => {
                    // Starred shows it before or after: its flags as they were, then as
                    // unwound.
                    self.touch_shown(row, false, &mut touched)?;
                    touched.folders.extend(self.unwind_flags(row, op.id)?);
                    self.touch_shown(row, false, &mut touched)?;
                }
                ReplayKind::Label { label, .. } => {
                    touched
                        .folders
                        .extend(self.unwind_label(row, label, op.id)?);
                    self.touch_shown(row, false, &mut touched)?;
                }
                ReplayKind::Move { .. } => {
                    touched.merge(self.restore(row, folder, op.uid)?);
                    self.fail_waiting(folder, op, error, &mut touched)?;
                }
                ReplayKind::Delete => {}
            }
        }
        Ok(touched)
    }

    /// Marks op `id` failed, if it is still pending and goes to `target`, recording its
    /// account in `touched`. Returns its message, if it was marked and has one.
    fn fail(
        &self,
        id: PendingOpId,
        target: Option<FolderId>,
        error: &str,
        touched: &mut Touched,
    ) -> Result<Option<MessageId>> {
        let failed: Option<(i64, Option<i64>)> = self
            .conn
            .prepare_cached(
                "UPDATE pending_ops SET status = 'failed', last_error = ?2
                 WHERE id = ?1 AND status = 'pending' AND target_folder_id IS ?3
                 RETURNING account_id, message_id",
            )?
            .query_row(params![id.0, error, target.map(|f| f.0)], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        Ok(failed.and_then(|(account, row)| {
            touched.refused.insert(AccountId(account));
            row.map(MessageId)
        }))
    }

    /// Fails, with the refused move `moved`, the label changes that wait for it (see
    /// `pending`): they were meant for where it was going.
    fn fail_waiting(
        &self,
        folder: FolderId,
        moved: &ReplayOp,
        error: &str,
        touched: &mut Touched,
    ) -> Result<()> {
        let waiting: Vec<(PendingOpId, FolderId)> = self
            .conn
            .prepare_cached(
                "SELECT id, target_folder_id FROM pending_ops
                 WHERE folder_id = ?1 AND uidvalidity = ?2 AND uid = ?3 AND kind = 'label'
                   AND status = 'pending' AND id > ?4
                 ORDER BY id",
            )?
            .query_map(
                params![folder.0, moved.uidvalidity, moved.uid, moved.id.0],
                |r| Ok((PendingOpId(r.get(0)?), FolderId(r.get(1)?))),
            )?
            .collect::<rusqlite::Result<_>>()?;
        // Oldest first: each undoes what is left after the ones before it.
        for (id, label) in waiting {
            if let Some(row) = self.fail(id, Some(label), error, touched)? {
                touched.folders.extend(self.unwind_label(row, label, id)?);
            }
        }
        Ok(())
    }

    /// Records where a COPY is about to put these messages; see [`CopyCheckpoint`].
    /// Commit it durably: it is what keeps a retry from copying again.
    pub fn before_copy(&self, ops: &[ReplayOp], checkpoint: &CopyCheckpoint) -> Result<()> {
        let mut update = self.conn.prepare_cached(
            "UPDATE pending_ops SET copy_folder_id = ?2, copy_uidvalidity = ?3, copy_uidnext = ?4,
                 copy_uid = NULL
             WHERE id = ?1 AND status = 'pending'",
        )?;
        for op in ops {
            update.execute(params![
                op.id.0,
                checkpoint.folder.0,
                checkpoint.uidvalidity,
                checkpoint.uidnext
            ])?;
        }
        Ok(())
    }

    /// Records where the COPY after [`before_copy`](Self::before_copy) put these
    /// messages, by (op, UID of the copy) under the checkpoint's UIDVALIDITY. Commit it
    /// durably, before the originals are deleted.
    pub fn copied(&self, copies: &[(PendingOpId, u32)]) -> Result<()> {
        let mut update = self.conn.prepare_cached(
            "UPDATE pending_ops SET copy_uid = ?2
             WHERE id = ?1 AND status = 'pending' AND copy_folder_id IS NOT NULL",
        )?;
        for &(op, uid) in copies {
            update.execute(params![op.0, uid])?;
        }
        Ok(())
    }

    /// No earlier COPY made these messages' copies: the checkpoints have done their job.
    pub fn forget_checkpoints(&self, ops: &[ReplayOp]) -> Result<()> {
        let mut update = self.conn.prepare_cached(
            "UPDATE pending_ops
             SET copy_folder_id = NULL, copy_uidvalidity = NULL, copy_uidnext = NULL,
                 copy_uid = NULL
             WHERE id = ?1",
        )?;
        for op in ops {
            update.execute([op.id.0])?;
        }
        Ok(())
    }

    /// Takes the refused flag change `failed` out of the message's flags. Each change
    /// records the bits it actually changed, so undoing the pending ones and `failed`
    /// newest first gives the flags before them all, exactly. The pending ones are then
    /// made again on those, each re-recording what it changes now, so that a later
    /// refusal can be undone exactly as well. Undoing `failed` alone would be wrong when
    /// a later change depends on it (read, then unread again).
    /// Returns the message's folder.
    fn unwind_flags(
        &self,
        row: MessageId,
        failed: PendingOpId,
    ) -> Result<Option<(AccountId, FolderId)>> {
        let current: Option<(u32, (AccountId, FolderId))> = self
            .conn
            .prepare_cached("SELECT flags, account_id, folder_id FROM messages WHERE id = ?1")?
            .query_row([row.0], |r| {
                Ok((r.get(0)?, (AccountId(r.get(1)?), FolderId(r.get(2)?))))
            })
            .optional()?;
        let Some((current, folder)) = current else {
            return Ok(None);
        };
        let changes: Vec<(i64, Flags, Flags)> = self
            .conn
            .prepare_cached(
                "SELECT id, flags_add, flags_remove FROM pending_ops
                 WHERE message_id = ?1 AND kind = 'flags' AND (status = 'pending' OR id = ?2)
                 ORDER BY id",
            )?
            .query_map(params![row.0, failed.0], |r| {
                Ok((
                    r.get(0)?,
                    Flags::from_bits(r.get(1)?),
                    Flags::from_bits(r.get(2)?),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut flags = changes
            .iter()
            .rev()
            .fold(Flags::from_bits(current), |flags, &(_, add, remove)| {
                flags.difference(add) | remove
            });
        let mut rerecord = self.conn.prepare_cached(
            "UPDATE pending_ops SET flags_add = ?2, flags_remove = ?3 WHERE id = ?1",
        )?;
        for &(id, add, remove) in changes.iter().filter(|(id, ..)| *id != failed.0) {
            let (add, remove) = (add.difference(flags), remove.intersection(flags));
            rerecord.execute(params![id, add.bits(), remove.bits()])?;
            flags = flags.apply(add, remove);
        }
        self.conn
            .prepare_cached("UPDATE messages SET flags = ?2 WHERE id = ?1")?
            .execute(params![row.0, flags.bits()])?;
        Ok(Some(folder))
    }

    /// Takes the refused change `failed` to `label` out of the message's labels, as
    /// [`unwind_flags`](Self::unwind_flags) does for flags: each change of the label
    /// changed it, so before them all it was the opposite of the first. The pending ones
    /// are made again on that; one that changes nothing now is dropped. Returns the
    /// label and the message's folder.
    fn unwind_label(
        &self,
        row: MessageId,
        label: FolderId,
        failed: PendingOpId,
    ) -> Result<Vec<(AccountId, FolderId)>> {
        let changes: Vec<(i64, bool)> = self
            .conn
            .prepare_cached(
                "SELECT id, label_add FROM pending_ops
                 WHERE message_id = ?1 AND kind = 'label' AND target_folder_id = ?2
                   AND (status = 'pending' OR id = ?3)
                 ORDER BY id",
            )?
            .query_map(params![row.0, label.0, failed.0], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let Some(&(_, first)) = changes.first() else {
            return Ok(Vec::new());
        };
        let mut has = !first;
        let mut drop = self
            .conn
            .prepare_cached("DELETE FROM pending_ops WHERE id = ?1")?;
        for &(id, add) in changes.iter().filter(|(id, _)| *id != failed.0) {
            if add == has {
                drop.execute([id])?;
            }
            has = add;
        }
        self.set_label(row, label, has)?;
        let folder: Option<(AccountId, FolderId)> = self
            .conn
            .prepare_cached("SELECT account_id, folder_id FROM messages WHERE id = ?1")?
            .query_row([row.0], folder_of)
            .optional()?;
        Ok(folder
            .into_iter()
            .flat_map(|(account, folder)| [(account, folder), (account, label)])
            .collect())
    }

    /// The connection was lost while replaying these; they stay pending.
    pub fn attempted(&self, ops: &[ReplayOp], error: &str) -> Result<()> {
        let mut update = self.conn.prepare_cached(
            "UPDATE pending_ops SET attempts = attempts + 1, last_error = ?2
             WHERE id = ?1 AND status = 'pending'",
        )?;
        for op in ops {
            update.execute(params![op.id.0, error])?;
        }
        Ok(())
    }

    /// Before `folder` is deleted (the server no longer lists it), with its ops and the
    /// moves into it: messages moved out of it locally are gone along with it, messages
    /// moved into it go back to where the server has them.
    /// Returns the other folders whose messages this changed.
    pub(crate) fn forget_folder(&self, folder: FolderId) -> Result<Touched> {
        let mut touched = self.drop_moving_rows("folder_id = ?1", [folder.0])?;
        let moves_in: Vec<(i64, i64, u32)> = self
            .conn
            .prepare(
                "SELECT message_id, folder_id, uid FROM pending_ops
                 WHERE target_folder_id = ?1 AND folder_id <> ?1 AND kind = 'move'
                   AND status = 'pending' AND message_id IS NOT NULL",
            )?
            .query_map([folder.0], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (row, source, uid) in moves_in {
            touched.merge(self.restore(MessageId(row), FolderId(source), uid)?);
        }
        touched.folders.retain(|&(_, f)| f != folder);
        Ok(touched)
    }

    /// Turns the folder's pending ops of other generations than `keep` into conflicts,
    /// dropping the rows of their moves: those UIDs are gone. Sync asks when it finds a
    /// new generation, replay before making them (a label's mailbox, Gmail's Drafts that
    /// holds the copies of drafts, is never synced). Returns the folders whose messages or
    /// ops this changed.
    pub fn end_generation(&self, folder: FolderId, keep: Option<u32>) -> Result<Touched> {
        self.conflict(
            "folder_id = ?1 AND uidvalidity IS NOT ?2",
            params![folder.0, keep],
        )
    }

    /// Marks the pending ops matching `filter` conflicts, dropping the rows of moves
    /// among them, whose messages' place on the server is no longer known. Returns the
    /// folders of those rows and ops, and the ops' accounts as refused.
    fn conflict(&self, filter: &str, params: impl Params + Clone) -> Result<Touched> {
        let mut touched = self.drop_moving_rows(filter, params.clone())?;
        let mut stmt = self.conn.prepare_cached(&format!(
            "UPDATE pending_ops SET status = 'conflict'
             WHERE {filter} AND status = 'pending'
             RETURNING account_id, folder_id"
        ))?;
        for row in stmt.query_map(params, folder_of)? {
            let (account, folder) = row?;
            touched.folders.insert((account, folder));
            touched.refused.insert(account);
        }
        Ok(touched)
    }

    /// Deletes the rows, still without a UID, of the pending moves matching `filter`.
    /// Returns their folders.
    fn drop_moving_rows(&self, filter: &str, params: impl Params) -> Result<Touched> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "DELETE FROM messages WHERE uid IS NULL AND id IN (
                 SELECT message_id FROM pending_ops
                 WHERE {filter} AND kind = 'move' AND status = 'pending')
             RETURNING account_id, folder_id"
        ))?;
        let rows = stmt.query_map(params, folder_of)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Deletes a message moved locally whose place on the server isn't known; the next
    /// sync fetches it where it is. Returns its folder, if it was there.
    fn drop_unplaced(&self, row: MessageId) -> Result<Option<(AccountId, FolderId)>> {
        Ok(self
            .conn
            .prepare_cached(
                "DELETE FROM messages WHERE id = ?1 AND uid IS NULL
                 RETURNING account_id, folder_id",
            )?
            .query_row([row.0], folder_of)
            .optional()?)
    }

    /// Gives a message moved to `folder` its UID there, or drops its row if that isn't
    /// known. Should a sync have stored the message under that UID already, that row
    /// wins. Returns the row's folder, if it was there.
    fn place(
        &self,
        row: MessageId,
        folder: FolderId,
        uid: Option<u32>,
    ) -> Result<Option<(AccountId, FolderId)>> {
        if let Some(uid) = uid {
            let placed = self
                .conn
                .prepare_cached(
                    "UPDATE OR IGNORE messages SET uid = ?3
                     WHERE id = ?1 AND folder_id = ?2 AND uid IS NULL
                     RETURNING account_id, folder_id",
                )?
                .query_row(params![row.0, folder.0, uid], folder_of)
                .optional()?;
            if placed.is_some() {
                return Ok(placed);
            }
        }
        self.drop_unplaced(row)
    }

    /// Puts a message moved locally back where the server has it, `uid` in `folder`.
    /// Returns the folders that lost and got it.
    fn restore(&self, row: MessageId, folder: FolderId, uid: u32) -> Result<Touched> {
        let Some(moved_to) = self
            .conn
            .prepare_cached(
                "SELECT account_id, folder_id FROM messages WHERE id = ?1 AND uid IS NULL",
            )?
            .query_row([row.0], folder_of)
            .optional()?
        else {
            return Ok(Touched::new());
        };
        let mut touched = Touched::new();
        self.touch_shown(row, false, &mut touched)?;
        let restored = self
            .conn
            .prepare_cached(
                "UPDATE OR IGNORE messages SET folder_id = ?2, uid = ?3
                 WHERE id = ?1 AND uid IS NULL
                 RETURNING account_id, folder_id",
            )?
            .query_row(params![row.0, folder.0, uid], folder_of)
            .optional()?;
        if restored.is_none() {
            self.drop_unplaced(row)?;
        }
        touched
            .folders
            .extend([Some(moved_to), restored].into_iter().flatten());
        Ok(touched)
    }
}

fn folder_of(r: &Row<'_>) -> rusqlite::Result<(AccountId, FolderId)> {
    Ok((AccountId(r.get(0)?), FolderId(r.get(1)?)))
}

fn kind(r: &Row<'_>) -> rusqlite::Result<OpKind> {
    let kind: String = r.get("kind")?;
    Ok(match kind.as_str() {
        "flags" => OpKind::Flags {
            add: Flags::from_bits(r.get("flags_add")?),
            remove: Flags::from_bits(r.get("flags_remove")?),
        },
        "move" => OpKind::Move {
            to: FolderId(r.get("target_folder_id")?),
        },
        "delete" => OpKind::Delete,
        "label" => OpKind::Label {
            label: FolderId(r.get("target_folder_id")?),
            add: r.get("label_add")?,
        },
        other => {
            let error = format!("unknown operation kind `{other}`");
            return Err(conversion_error(r, "kind", error.into()));
        }
    })
}

/// Folds every message's earlier changes to a label into its last one (see
/// [`PendingOps::pending`]), keeping the order of what is left.
fn supersede(ops: Vec<ReplayOp>) -> Vec<ReplayOp> {
    let key = |op: &ReplayOp| match op.kind {
        ReplayKind::Label { label, .. } => Some((op.uid, label)),
        _ => None,
    };
    let mut last = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let Some(key) = key(op) {
            last.insert(key, i);
        }
    }
    let mut earlier: HashMap<(u32, FolderId), Vec<ReplayOp>> = HashMap::new();
    let mut kept = Vec::with_capacity(ops.len());
    for (i, mut op) in ops.into_iter().enumerate() {
        match key(&op) {
            Some(key) if last[&key] != i => earlier.entry(key).or_default().push(op),
            Some(key) => {
                op.superseded = earlier.remove(&key).unwrap_or_default();
                kept.push(op);
            }
            None => kept.push(op),
        }
    }
    kept
}

/// The label of a label op as X-GM-LABELS names it, from its folder's `role`,
/// `remote_name` and `name`.
fn gmail_label(r: &Row<'_>) -> rusqlite::Result<GmailLabel> {
    Ok(GmailLabel::for_mailbox(
        parse_opt_text(r, "role")?,
        r.get("remote_name")?,
        r.get("name")?,
    ))
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<PendingOp> {
    Ok(PendingOp {
        id: PendingOpId(r.get("id")?),
        account_id: AccountId(r.get("account_id")?),
        folder_id: FolderId(r.get("folder_id")?),
        message_id: r.get::<_, Option<i64>>("message_id")?.map(MessageId),
        subject: r.get("subject")?,
        kind: kind(r)?,
        status: parse_text(r, "status")?,
        attempts: r.get("attempts")?,
        last_error: r.get("last_error")?,
        created_at: r.get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use catmail_types::{FlagState, Folder, FolderRole};

    use super::*;
    use crate::Db;
    use crate::folders::RemoteFolder;
    use crate::messages::NewMessage;
    use crate::test_util::{self, TestStore};

    struct Fixture {
        store: TestStore,
        account: AccountId,
        inbox: FolderId,
        archive: FolderId,
        trash: FolderId,
    }

    fn folder(name: &str, role: Option<FolderRole>) -> RemoteFolder {
        RemoteFolder {
            remote_name: name.into(),
            name: name.into(),
            delimiter: Some('/'),
            role,
            kind: catmail_types::FolderKind::Location,
            selectable: true,
        }
    }

    /// INBOX with UIDs 1..=3 (1 is read), an empty Archive and Trash, all synced.
    fn setup() -> Fixture {
        let store = test_util::store();
        let account = test_util::account(&store, "a@example.com");
        let folders: Vec<Folder> = store
            .write(|db| {
                db.folders().reconcile(
                    account,
                    &[
                        folder("INBOX", Some(FolderRole::Inbox)),
                        folder("Archive", Some(FolderRole::Archive)),
                        folder("Trash", Some(FolderRole::Trash)),
                    ],
                )?;
                let folders = db.folders().list(account)?;
                for f in &folders {
                    db.folders().reset_uidvalidity(f.id, 7)?;
                }
                let messages: Vec<NewMessage> = (1..=3)
                    .map(|uid| NewMessage {
                        state: FlagState {
                            uid,
                            flags: if uid == 1 {
                                Flags::SEEN
                            } else {
                                Flags::empty()
                            },
                            ..Default::default()
                        },
                        subject: Some(format!("m{uid}")),
                        ..Default::default()
                    })
                    .collect();
                db.messages().upsert(account, folders[0].id, &messages)?;
                Ok(folders)
            })
            .unwrap();
        let id = |name: &str| folders.iter().find(|f| f.name == name).unwrap().id;
        Fixture {
            store,
            account,
            inbox: id("INBOX"),
            archive: id("Archive"),
            trash: id("Trash"),
        }
    }

    impl Fixture {
        fn write<T>(&self, f: impl FnOnce(&Db<'_>) -> Result<T>) -> T {
            self.store.write(f).unwrap()
        }

        fn message(&self, folder: FolderId, uid: u32) -> MessageId {
            self.store
                .read(|db| {
                    Ok(db.conn.query_row(
                        "SELECT id FROM messages WHERE folder_id = ?1 AND uid IS ?2",
                        params![folder.0, uid],
                        |r| Ok(MessageId(r.get(0)?)),
                    )?)
                })
                .unwrap()
        }

        /// (folder, uid, flags) of the row, if it exists.
        fn row(&self, id: MessageId) -> Option<(FolderId, Option<u32>, Flags)> {
            self.store
                .read(|db| {
                    Ok(db
                        .conn
                        .query_row(
                            "SELECT folder_id, uid, flags FROM messages WHERE id = ?1",
                            [id.0],
                            |r| Ok((FolderId(r.get(0)?), r.get(1)?, Flags::from_bits(r.get(2)?))),
                        )
                        .optional()?)
                })
                .unwrap()
        }

        fn counts(&self, folder: FolderId) -> (u32, u32) {
            let f = self
                .store
                .read(|db| db.folders().get(folder))
                .unwrap()
                .unwrap();
            (f.total_count, f.unread_count)
        }

        fn pending(&self, folder: FolderId) -> Vec<ReplayOp> {
            self.store.read(|db| db.ops().pending(folder)).unwrap()
        }

        fn ops(&self) -> Vec<PendingOp> {
            self.store.read(|db| db.ops().list(None)).unwrap()
        }
    }

    #[test]
    fn deleting_by_server_location_leaves_what_the_user_moved() {
        let f = setup();
        let (inbox, account) = (f.inbox, f.account);
        let m2 = f.message(inbox, 2);
        f.write(|db| db.ops().move_messages(&[m2], None, f.archive, 1));
        let delete = |uid| f.write(|db| db.ops().delete_on_server(account, inbox, 7, uid, 2));

        assert!(delete(2).is_empty(), "moved by the user meanwhile");
        // A row of that generation goes with it; a UID never synced is queued all the same.
        assert_eq!(delete(3), Touched::from([(account, inbox)]));
        assert_eq!(f.row(f.message(inbox, 1)).map(|r| r.1), Some(Some(1)));
        assert_eq!(delete(9), Touched::from([(account, inbox)]));
        assert_eq!(f.counts(inbox), (1, 0));
        let kinds: Vec<(u32, ReplayKind)> = f
            .pending(inbox)
            .into_iter()
            .map(|op| (op.uid, op.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                (3, ReplayKind::Delete),
                (9, ReplayKind::Delete),
                (
                    2,
                    ReplayKind::Move {
                        to: f.archive,
                        remote_name: "Archive".into()
                    }
                ),
            ]
        );
    }

    fn moved(uidvalidity: u32, uids: &[(u32, u32)]) -> Moved {
        Moved {
            uidvalidity,
            uids: uids.iter().copied().collect(),
        }
    }

    #[test]
    fn flag_changes_are_queued_for_what_changed() {
        let f = setup();
        let (m1, m2) = (f.message(f.inbox, 1), f.message(f.inbox, 2));
        let touched = f.write(|db| {
            db.ops()
                .set_flags(&[m1, m2], Flags::SEEN | Flags::FLAGGED, Flags::empty(), 0)
        });
        assert_eq!(touched, Touched::from([(f.account, f.inbox)]));
        assert_eq!(f.row(m2).unwrap().2, Flags::SEEN | Flags::FLAGGED);
        assert_eq!(f.counts(f.inbox), (3, 1));
        let kinds: Vec<OpKind> = f
            .pending(f.inbox)
            .into_iter()
            .map(|op| op.kind.op_kind())
            .collect();
        // m1 was read already.
        let flagged = |add| OpKind::Flags {
            add,
            remove: Flags::empty(),
        };
        assert_eq!(
            kinds,
            [
                flagged(Flags::FLAGGED),
                flagged(Flags::SEEN | Flags::FLAGGED)
            ]
        );
        // Nothing left to change: nothing queued.
        f.write(|db| db.ops().set_flags(&[m1], Flags::SEEN, Flags::empty(), 0));
        assert_eq!(f.pending(f.inbox).len(), 2);
        assert!(matches!(
            f.store.write(|db| db.ops().set_flags(
                &[MessageId(999)],
                Flags::SEEN,
                Flags::empty(),
                0
            )),
            Err(Error::MessageNotFound(MessageId(999)))
        ));
    }

    #[test]
    fn sync_keeps_unreplayed_flag_changes() {
        let f = setup();
        let m2 = f.message(f.inbox, 2);
        f.write(|db| db.ops().set_flags(&[m2], Flags::SEEN, Flags::empty(), 0));
        // The server still says unread, and someone flagged it.
        let server = FlagState {
            uid: 2,
            flags: Flags::FLAGGED,
            ..Default::default()
        };
        f.write(|db| {
            db.messages()
                .update_flags(f.inbox, std::slice::from_ref(&server))
        });
        assert_eq!(f.row(m2).unwrap().2, Flags::SEEN | Flags::FLAGGED);

        let ops = f.pending(f.inbox);
        f.write(|db| db.ops().applied(&ops));
        assert!(f.ops().is_empty());
        f.write(|db| {
            db.messages()
                .update_flags(f.inbox, std::slice::from_ref(&server))
        });
        assert_eq!(f.row(m2).unwrap().2, Flags::FLAGGED);
    }

    #[test]
    fn repeated_batch_keeps_unreplayed_flag_changes() {
        let f = setup();
        let (m1, m2) = (f.message(f.inbox, 1), f.message(f.inbox, 2));
        f.write(|db| db.ops().set_flags(&[m2], Flags::SEEN, Flags::empty(), 0));
        // An interrupted sync fetches the same batch again; someone flagged both.
        let again: Vec<_> = [1, 2]
            .map(|uid| NewMessage {
                state: FlagState {
                    uid,
                    flags: Flags::FLAGGED,
                    ..Default::default()
                },
                ..Default::default()
            })
            .into();
        assert_eq!(
            f.write(|db| db.messages().upsert(f.account, f.inbox, &again))
                .added,
            0
        );
        assert_eq!(f.row(m1).unwrap().2, Flags::FLAGGED);
        assert_eq!(f.row(m2).unwrap().2, Flags::SEEN | Flags::FLAGGED);
    }

    #[test]
    fn refused_flag_changes_are_undone_unless_changed_again() {
        let f = setup();
        let (m1, m2) = (f.message(f.inbox, 1), f.message(f.inbox, 2));
        f.write(|db| {
            db.ops()
                .set_flags(&[m1, m2], Flags::FLAGGED, Flags::SEEN, 0)?;
            db.ops().set_flags(&[m2], Flags::empty(), Flags::FLAGGED, 0)
        });
        let ops = f.pending(f.inbox);
        let touched = f.write(|db| db.ops().failed(f.inbox, &ops[..2], "NO read-only"));
        assert_eq!(touched.refused, BTreeSet::from([f.account]));
        // m1 back to read; m2's later change is still pending, so it keeps it.
        assert_eq!(f.row(m1).unwrap().2, Flags::SEEN);
        assert_eq!(f.row(m2).unwrap().2, Flags::empty());
        let statuses: Vec<_> = f.ops().into_iter().map(|op| op.status).collect();
        assert_eq!(
            statuses,
            [OpStatus::Failed, OpStatus::Failed, OpStatus::Pending]
        );
        assert_eq!(f.ops()[0].last_error.as_deref(), Some("NO read-only"));
    }

    #[test]
    fn refused_changes_in_a_row_restore_where_they_started() {
        let f = setup();
        let m2 = f.message(f.inbox, 2);
        // Unread: read, then unread again, both refused.
        f.write(|db| {
            db.ops().set_flags(&[m2], Flags::SEEN, Flags::empty(), 0)?;
            db.ops().set_flags(&[m2], Flags::empty(), Flags::SEEN, 0)
        });
        let ops = f.pending(f.inbox);
        f.write(|db| {
            db.ops().failed(f.inbox, &ops[..1], "NO")?;
            db.ops().failed(f.inbox, &ops[1..], "NO")
        });
        assert_eq!(f.row(m2).unwrap().2, Flags::empty());

        // Read and flagged, then unflagged; only the first is refused.
        f.write(|db| {
            db.ops()
                .set_flags(&[m2], Flags::SEEN | Flags::FLAGGED, Flags::empty(), 0)?;
            db.ops().set_flags(&[m2], Flags::empty(), Flags::FLAGGED, 0)
        });
        let ops = f.pending(f.inbox);
        f.write(|db| db.ops().failed(f.inbox, &ops[..1], "NO"));
        assert_eq!(f.row(m2).unwrap().2, Flags::empty());
    }

    #[test]
    fn moved_messages_show_in_the_target_until_the_server_moves_them() {
        let f = setup();
        let m2 = f.message(f.inbox, 2);
        let touched = f.write(|db| db.ops().move_messages(&[m2], None, f.archive, 0));
        assert_eq!(
            touched,
            Touched::from([(f.account, f.inbox), (f.account, f.archive)])
        );
        assert_eq!(f.row(m2), Some((f.archive, None, Flags::empty())));
        assert_eq!((f.counts(f.inbox), f.counts(f.archive)), ((2, 1), (1, 1)));

        // Downloads still find it where the server has it.
        let location = f
            .store
            .read(|db| db.messages().location(m2))
            .unwrap()
            .unwrap();
        assert_eq!(
            (location.folder, location.remote_name.as_str(), location.uid),
            (f.archive, "INBOX", 2)
        );
        // Sync neither brings it back to INBOX nor counts it in Archive.
        let again = NewMessage {
            state: FlagState {
                uid: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            f.write(|db| db.messages().upsert(f.account, f.inbox, &[again]))
                .added,
            0
        );
        let states = f
            .store
            .read(|db| db.folders().sync_states(f.account))
            .unwrap();
        let archive = states.iter().find(|s| s.id == f.archive).unwrap();
        assert_eq!((archive.message_count, archive.max_uid), (0, 0));

        let ops = f.pending(f.inbox);
        f.write(|db| {
            db.ops()
                .moved(f.inbox, 7, f.archive, &ops, Some(&moved(7, &[(2, 40)])))
        });
        assert_eq!(f.row(m2), Some((f.archive, Some(40), Flags::empty())));
        assert!(f.ops().is_empty());
    }

    #[test]
    fn moves_without_new_uids_leave_it_to_the_next_sync() {
        let f = setup();
        let (m2, m3) = (f.message(f.inbox, 2), f.message(f.inbox, 3));
        f.write(|db| db.ops().move_messages(&[m2, m3], None, f.archive, 0));
        let ops = f.pending(f.inbox);
        // The server answered without COPYUID, or for another UIDVALIDITY.
        f.write(|db| {
            db.ops().moved(f.inbox, 7, f.archive, &ops[..1], None)?;
            db.ops()
                .moved(f.inbox, 7, f.archive, &ops[1..], Some(&moved(8, &[(3, 9)])))
        });
        assert_eq!((f.row(m2), f.row(m3)), (None, None));
        assert!(f.ops().is_empty());
        assert_eq!(f.counts(f.archive), (0, 0));
    }

    #[test]
    fn a_message_moved_on_while_in_flight_follows_it() {
        let f = setup();
        let m2 = f.message(f.inbox, 2);
        f.write(|db| db.ops().move_messages(&[m2], None, f.archive, 0));
        let in_flight = f.pending(f.inbox);
        // While the server moves it to Archive, the user moves it on to Trash, and marks
        // it read.
        f.write(|db| {
            db.ops().move_messages(&[m2], None, f.trash, 0)?;
            db.ops().set_flags(&[m2], Flags::SEEN, Flags::empty(), 0)
        });
        f.write(|db| {
            db.ops().moved(
                f.inbox,
                7,
                f.archive,
                &in_flight,
                Some(&moved(7, &[(2, 40)])),
            )
        });
        assert_eq!(f.row(m2), Some((f.trash, None, Flags::SEEN)));
        assert!(f.pending(f.inbox).is_empty());
        let next = f.pending(f.archive);
        let kinds: Vec<(u32, OpKind)> = next.iter().map(|op| (op.uid, op.kind.op_kind())).collect();
        assert_eq!(
            kinds,
            [
                (
                    40,
                    OpKind::Flags {
                        add: Flags::SEEN,
                        remove: Flags::empty()
                    }
                ),
                (40, OpKind::Move { to: f.trash }),
            ]
        );
        // Downloads look for it in Archive now.
        let location = f
            .store
            .read(|db| db.messages().location(m2))
            .unwrap()
            .unwrap();
        assert_eq!(
            (location.remote_name.as_str(), location.uid),
            ("Archive", 40)
        );
    }

    #[test]
    fn moving_back_is_settled_locally() {
        let f = setup();
        let m2 = f.message(f.inbox, 2);
        f.write(|db| {
            db.ops().move_messages(&[m2], None, f.archive, 0)?;
            db.ops().move_messages(&[m2], None, f.inbox, 0)
        });
        assert_eq!(f.row(m2), Some((f.inbox, None, Flags::empty())));
        let ops = f.pending(f.inbox);
        assert_eq!(ops[0].kind.op_kind(), OpKind::Move { to: f.inbox });
        // What replay does for a move to its own folder.
        f.write(|db| db.ops().moved(f.inbox, 7, f.inbox, &ops, None));
        assert_eq!(f.row(m2), Some((f.inbox, Some(2), Flags::empty())));
        assert!(f.ops().is_empty());
        assert_eq!(f.counts(f.inbox), (3, 2));
    }

    #[test]
    fn refused_moves_go_back() {
        let f = setup();
        let m2 = f.message(f.inbox, 2);
        f.write(|db| db.ops().move_messages(&[m2], None, f.archive, 0));
        let ops = f.pending(f.inbox);
        f.write(|db| db.ops().failed(f.inbox, &ops, "NO [TRYCREATE]"));
        assert_eq!(f.row(m2), Some((f.inbox, Some(2), Flags::empty())));
        assert_eq!(f.ops()[0].status, OpStatus::Failed);
        assert_eq!((f.counts(f.inbox), f.counts(f.archive)), ((3, 2), (0, 0)));

        // One that was sent elsewhere meanwhile waits for its new target instead.
        let m3 = f.message(f.inbox, 3);
        f.write(|db| db.ops().move_messages(&[m3], None, f.archive, 0));
        let ops = f.pending(f.inbox);
        f.write(|db| {
            db.ops().move_messages(&[m3], None, f.trash, 0)?;
            db.ops().failed(f.inbox, &ops, "NO")
        });
        assert_eq!(f.row(m3), Some((f.trash, None, Flags::empty())));
        assert_eq!(
            f.pending(f.inbox)[0].kind.op_kind(),
            OpKind::Move { to: f.trash }
        );
    }

    #[test]
    fn flag_changes_replay_in_rounds() {
        let f = setup();
        let (m1, m2, m3) = (
            f.message(f.inbox, 1),
            f.message(f.inbox, 2),
            f.message(f.inbox, 3),
        );
        let (seen, flagged, none) = (Flags::SEEN, Flags::FLAGGED, Flags::empty());
        // Interleaved, as a user going through the list would; 1 starts out read.
        f.write(|db| {
            db.ops().set_flags(&[m1], none, seen, 0)?;
            db.ops().set_flags(&[m2], flagged, none, 0)?;
            db.ops().set_flags(&[m3], flagged, none, 0)?;
            db.ops().set_flags(&[m1], seen, none, 0)?;
            db.ops().set_flags(&[m2], seen, none, 0)
        });
        let order: Vec<(u32, OpKind)> = f
            .pending(f.inbox)
            .iter()
            .map(|op| (op.uid, op.kind.op_kind()))
            .collect();
        let change = |add, remove| OpKind::Flags { add, remove };
        assert_eq!(
            order,
            [
                // Every message's first change, grouped by what it does...
                (1, change(none, seen)),
                (2, change(flagged, none)),
                (3, change(flagged, none)),
                // ...then the second ones, each after its message's first.
                (1, change(seen, none)),
                (2, change(seen, none)),
            ]
        );
    }

    #[test]
    fn moves_replay_grouped_by_target() {
        let f = setup();
        let (m1, m2, m3) = (
            f.message(f.inbox, 1),
            f.message(f.inbox, 2),
            f.message(f.inbox, 3),
        );
        // Archive, Trash, Archive, as a user triaging would.
        f.write(|db| {
            db.ops().move_messages(&[m1], None, f.archive, 0)?;
            db.ops().delete_messages(&[m2], false, 0)?;
            db.ops().move_messages(&[m3], None, f.archive, 0)
        });
        let order: Vec<(u32, OpKind)> = f
            .pending(f.inbox)
            .iter()
            .map(|op| (op.uid, op.kind.op_kind()))
            .collect();
        let to = |to| OpKind::Move { to };
        assert_eq!(
            order,
            [(1, to(f.archive)), (3, to(f.archive)), (2, to(f.trash))]
        );
    }

    #[test]
    fn deleting_moves_to_trash_and_deletes_from_it() {
        let f = setup();
        let (m1, m2, m3) = (
            f.message(f.inbox, 1),
            f.message(f.inbox, 2),
            f.message(f.inbox, 3),
        );
        f.write(|db| db.ops().delete_messages(&[m2], false, 0));
        assert_eq!(f.row(m2), Some((f.trash, None, Flags::empty())));
        // In Trash (even before it got there on the server): for good.
        f.write(|db| db.ops().delete_messages(&[m2], false, 0));
        assert_eq!(f.row(m2), None);
        f.write(|db| db.ops().delete_messages(&[m3], true, 0));
        assert_eq!(f.row(m3), None);
        let kinds: Vec<(u32, OpKind)> = f
            .pending(f.inbox)
            .iter()
            .map(|op| (op.uid, op.kind.op_kind()))
            .collect();
        assert_eq!(kinds, [(2, OpKind::Delete), (3, OpKind::Delete)]);
        assert_eq!(f.counts(f.trash), (0, 0));

        // Without a Trash folder, only permanently.
        f.write(|db| {
            db.conn.execute("UPDATE folders SET role = NULL", [])?;
            Ok(())
        });
        assert!(matches!(
            f.store
                .write(|db| db.ops().delete_messages(&[m1], false, 0)),
            Err(Error::Invalid(_))
        ));
        assert!(f.row(m1).is_some());
    }

    #[test]
    fn archiving_moves_what_is_not_archived_yet() {
        let f = setup();
        let (m1, m2) = (f.message(f.inbox, 1), f.message(f.inbox, 2));
        f.write(|db| db.ops().archive_messages(&[m1], 0));
        assert_eq!(f.row(m1), Some((f.archive, None, Flags::SEEN)));
        // Already there: nothing to do, no op.
        let touched = f.write(|db| db.ops().archive_messages(&[m1], 0));
        assert!(touched.is_empty());
        assert_eq!(f.pending(f.inbox).len(), 1);

        f.write(|db| {
            db.conn
                .execute("UPDATE folders SET role = NULL WHERE role = 'archive'", [])?;
            Ok(())
        });
        assert!(matches!(
            f.store.write(|db| db.ops().archive_messages(&[m2], 0)),
            Err(Error::Invalid(_))
        ));
        assert_eq!(f.row(m2).map(|(folder, ..)| folder), Some(f.inbox));
    }

    #[test]
    fn a_new_generation_turns_ops_into_conflicts() {
        let f = setup();
        let (m1, m2) = (f.message(f.inbox, 1), f.message(f.inbox, 2));
        f.write(|db| {
            db.ops()
                .set_flags(&[m1], Flags::FLAGGED, Flags::empty(), 0)?;
            db.ops().move_messages(&[m2], None, f.archive, 0)
        });
        let touched = f.write(|db| db.folders().reset_uidvalidity(f.inbox, 8));
        assert_eq!(touched.refused, BTreeSet::from([f.account]));
        // The moved row named an old UID too.
        assert_eq!(f.row(m2), None);
        assert!(f.ops().iter().all(|op| op.status == OpStatus::Conflict));
        assert!(f.pending(f.inbox).is_empty());
        assert_eq!(f.counts(f.archive), (0, 0));
    }

    #[test]
    fn a_new_generation_keeps_messages_on_their_way_in() {
        let f = setup();
        let m2 = f.message(f.inbox, 2);
        f.write(|db| db.ops().move_messages(&[m2], None, f.archive, 0));
        // The target is rebuilt: the move names INBOX's UID, which still holds.
        let touched = f.write(|db| db.folders().reset_uidvalidity(f.archive, 8));
        assert!(touched.refused.is_empty());
        assert_eq!(f.row(m2), Some((f.archive, None, Flags::empty())));
        assert_eq!(f.counts(f.archive), (1, 1));
        let ops = f.ops();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].status, OpStatus::Pending);
        assert_eq!(ops[0].message_id, Some(m2));
    }

    #[test]
    fn deleted_folders_take_their_ops_along() {
        let f = setup();
        let (m2, m3) = (f.message(f.inbox, 2), f.message(f.inbox, 3));
        f.write(|db| {
            db.ops().move_messages(&[m2], None, f.archive, 0)?;
            db.ops().move_messages(&[m3], None, f.trash, 0)
        });
        // Archive is gone from the server: m2 goes back to INBOX, where the server has it.
        f.write(|db| {
            db.folders().reconcile(
                f.account,
                &[
                    folder("INBOX", Some(FolderRole::Inbox)),
                    folder("Trash", Some(FolderRole::Trash)),
                ],
            )
        });
        assert_eq!(f.row(m2), Some((f.inbox, Some(2), Flags::empty())));
        // INBOX is gone: so is m3, moved out of it.
        f.write(|db| {
            db.folders()
                .reconcile(f.account, &[folder("Trash", Some(FolderRole::Trash))])
        });
        assert_eq!(f.row(m3), None);
        assert!(f.ops().is_empty());
        assert_eq!(f.counts(f.trash), (0, 0));
    }

    #[test]
    fn moves_stay_within_the_account() {
        let f = setup();
        let other = test_util::account(&f.store, "b@example.com");
        let theirs = f.write(|db| {
            db.folders()
                .reconcile(other, &[folder("INBOX", Some(FolderRole::Inbox))])?;
            Ok(db.folders().list(other)?[0].id)
        });
        let m1 = f.message(f.inbox, 1);
        assert!(matches!(
            f.store
                .write(|db| db.ops().move_messages(&[m1], None, theirs, 0)),
            Err(Error::Invalid(_))
        ));
        f.write(|db| {
            db.conn.execute(
                "UPDATE folders SET selectable = 0 WHERE id = ?1",
                [f.archive.0],
            )?;
            Ok(())
        });
        assert!(matches!(
            f.store
                .write(|db| db.ops().move_messages(&[m1], None, f.archive, 0)),
            Err(Error::Invalid(_))
        ));
        assert_eq!(f.row(m1).unwrap().0, f.inbox);
    }

    /// The Gmail account of [`test_util::gmail`], its folders named without `[Gmail]/`.
    /// Message 1 is in All Mail, in INBOX and Important, message 2 in Trash.
    struct Gmail {
        store: TestStore,
        account: AccountId,
        f: HashMap<&'static str, FolderId>,
    }

    fn gmail() -> Gmail {
        let store = test_util::store();
        let (account, f) = test_util::gmail(&store);
        let f: HashMap<_, _> = f
            .into_iter()
            .map(|(name, id)| (name.trim_start_matches("[Gmail]/"), id))
            .collect();
        let message = |uid: u32, labels: Vec<GmailLabel>| NewMessage {
            state: FlagState {
                uid,
                labels: Some(labels),
                ..Default::default()
            },
            subject: Some(format!("m{uid}")),
            gm_msgid: Some(100 + uid as u64),
            ..Default::default()
        };
        store
            .write(|db| {
                let labels = [FolderRole::Inbox, FolderRole::Important].map(GmailLabel::System);
                db.messages()
                    .upsert(account, f["All Mail"], &[message(1, labels.into())])?;
                db.messages()
                    .upsert(account, f["Trash"], &[message(2, Vec::new())])
            })
            .unwrap();
        Gmail { store, account, f }
    }

    impl Gmail {
        fn write<T>(&self, f: impl FnOnce(&Db<'_>) -> Result<T>) -> T {
            self.store.write(f).unwrap()
        }

        fn message(&self, n: u32) -> MessageId {
            self.store
                .read(|db| {
                    Ok(db.conn.query_row(
                        "SELECT id FROM messages WHERE gm_msgid = ?1",
                        [100 + n],
                        |r| Ok(MessageId(r.get(0)?)),
                    )?)
                })
                .unwrap()
        }

        /// The message's location and UID, and its labels by name.
        fn state(&self, id: MessageId) -> (&'static str, Option<u32>, Vec<&'static str>) {
            let name = |id: i64| *self.f.iter().find(|(_, f)| f.0 == id).unwrap().0;
            self.store
                .read(|db| {
                    let (folder, uid): (i64, Option<u32>) = db.conn.query_row(
                        "SELECT folder_id, uid FROM messages WHERE id = ?1",
                        [id.0],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    let mut labels: Vec<&str> = db
                        .conn
                        .prepare("SELECT folder_id FROM message_labels WHERE message_id = ?1")?
                        .query_map([id.0], |r| r.get(0))?
                        .map(|f| f.map(name))
                        .collect::<rusqlite::Result<_>>()?;
                    labels.sort();
                    Ok((name(folder), uid, labels))
                })
                .unwrap()
        }

        fn replay_ops(&self, folder: &str) -> Vec<ReplayOp> {
            self.store
                .read(|db| db.ops().pending(self.f[folder]))
                .unwrap()
        }

        /// The folder's ops to replay: (UID, kind).
        fn pending(&self, folder: &str) -> Vec<(u32, OpKind)> {
            self.replay_ops(folder)
                .into_iter()
                .map(|op| (op.uid, op.kind.op_kind()))
                .collect()
        }

        fn label(&self, name: &str, add: bool) -> OpKind {
            OpKind::Label {
                label: self.f[name],
                add,
            }
        }

        /// What `Folders` counts for the label: (total, unread).
        fn count(&self, name: &str) -> (u32, u32) {
            let f = self
                .store
                .read(|db| db.folders().get(self.f[name]))
                .unwrap()
                .unwrap();
            (f.total_count, f.unread_count)
        }
    }

    #[test]
    fn moves_between_gmail_labels_change_labels() {
        let g = gmail();
        let (m1, f) = (g.message(1), &g.f);
        let touched = g.write(|db| {
            db.ops()
                .move_messages(&[m1], Some(f["INBOX"]), f["Work"], 0)
        });
        let account = g.account;
        // Important lists it too, with its labels.
        assert_eq!(
            touched,
            Touched::from([
                (account, f["INBOX"]),
                (account, f["Work"]),
                (account, f["Important"]),
                (account, f["All Mail"])
            ])
        );
        // It stays where it is on the server.
        assert_eq!(
            g.state(m1),
            ("All Mail", Some(1), vec!["Important", "Work"])
        );
        assert_eq!((g.count("INBOX"), g.count("Work")), ((0, 0), (1, 1)));

        // Archived: out of INBOX, which it isn't in any more.
        assert!(g.write(|db| db.ops().archive_messages(&[m1], 0)).is_empty());
        // Starred is \Flagged, and a location is no label.
        let refused = [
            g.store
                .write(|db| db.ops().move_messages(&[m1], None, f["Starred"], 0)),
            g.store
                .write(|db| db.ops().label_messages(&[m1], &[f["Starred"]], &[], 0)),
            g.store
                .write(|db| db.ops().label_messages(&[m1], &[f["All Mail"]], &[], 0)),
        ];
        for refused in refused {
            assert!(matches!(refused, Err(Error::Invalid(_))), "{refused:?}");
        }

        g.write(|db| {
            db.ops().set_flags(&[m1], Flags::SEEN, Flags::empty(), 0)?;
            db.ops()
                .label_messages(&[m1], &[f["INBOX"]], &[f["Important"]], 0)?;
            db.ops().delete_messages(&[m1], false, 0)
        });
        // Flags, then labels (each label's in order), then moves; INBOX had two changes.
        assert_eq!(
            g.pending("All Mail"),
            [
                (
                    1,
                    OpKind::Flags {
                        add: Flags::SEEN,
                        remove: Flags::empty()
                    }
                ),
                // INBOX taken off, then added again: only the last is made.
                (1, g.label("Important", false)),
                (1, g.label("Work", true)),
                (1, g.label("INBOX", true)),
                (1, OpKind::Move { to: f["Trash"] }),
            ]
        );
    }

    #[test]
    fn a_label_change_shows_in_every_label_listing_the_message() {
        let g = gmail();
        let (m1, f) = (g.message(1), &g.f);
        // The message is listed in INBOX and Important, with its labels: they change.
        let touched = g.write(|db| db.ops().label_messages(&[m1], &[f["Work"]], &[], 0));
        let account = g.account;
        assert_eq!(
            touched,
            Touched::from([
                (account, f["INBOX"]),
                (account, f["Work"]),
                (account, f["Important"]),
                (account, f["All Mail"])
            ])
        );
        // So does undoing it.
        let ops = g.replay_ops("All Mail");
        let touched = g.write(|db| db.ops().failed(f["All Mail"], &ops, "NO"));
        for label in ["INBOX", "Work", "Important"] {
            assert!(touched.folders.contains(&(account, f[label])), "{label}");
        }
    }

    #[test]
    fn archiving_on_gmail_takes_inbox_off() {
        let g = gmail();
        let (m1, m2) = (g.message(1), g.message(2));
        g.write(|db| db.ops().archive_messages(&[m1, m2], 0));
        assert_eq!(g.state(m1), ("All Mail", Some(1), vec!["Important"]));
        // In Trash: nothing to archive.
        assert_eq!(g.state(m2), ("Trash", Some(2), vec![]));
        assert_eq!(g.pending("All Mail"), [(1, g.label("INBOX", false))]);
        assert!(g.pending("Trash").is_empty());
        assert_eq!(g.count("INBOX"), (0, 0));
    }

    #[test]
    fn refused_label_changes_are_undone_unless_changed_again() {
        let g = gmail();
        let (m1, work) = (g.message(1), g.f["Work"]);
        // Added, taken off, added again: only the last is made, the others along.
        g.write(|db| {
            db.ops().label_messages(&[m1], &[work], &[], 0)?;
            db.ops().label_messages(&[m1], &[], &[work], 0)?;
            db.ops().label_messages(&[m1], &[work], &[], 0)
        });
        let ops = g.replay_ops("All Mail");
        assert_eq!(g.pending("All Mail"), [(1, g.label("Work", true))]);
        assert_eq!(ops[0].superseded.len(), 2);
        // The first refused alone (as a lost connection could leave it).
        let first = &ops[0].superseded[..1];
        let touched = g.write(|db| db.ops().failed(g.f["All Mail"], first, "NO"));
        assert_eq!(touched.refused, BTreeSet::from([g.account]));
        assert!(touched.folders.contains(&(g.account, work)));
        // Without it, taking it off changes nothing: only adding it again is left.
        let ops = g.replay_ops("All Mail");
        assert_eq!(g.pending("All Mail"), [(1, g.label("Work", true))]);
        assert!(ops[0].superseded.is_empty());
        assert_eq!(g.state(m1).2, ["INBOX", "Important", "Work"]);
        // Refused too: as before them all.
        g.write(|db| db.ops().failed(g.f["All Mail"], &ops, "NO"));
        assert_eq!(g.state(m1).2, ["INBOX", "Important"]);
        assert_eq!(g.count("Work"), (0, 0));
    }

    #[test]
    fn a_label_change_waits_for_the_move_out_of_trash() {
        let g = gmail();
        let (m2, f) = (g.message(2), &g.f);
        g.write(|db| {
            db.ops()
                .move_messages(&[m2], Some(f["Trash"]), f["INBOX"], 0)
        });
        assert_eq!(g.state(m2), ("All Mail", None, vec!["INBOX"]));
        assert_eq!(g.count("INBOX"), (2, 2));
        // Gmail takes labels in All Mail: the label waits for the move.
        assert_eq!(
            g.pending("Trash"),
            [(2, OpKind::Move { to: f["All Mail"] })]
        );
        let ops = g.replay_ops("Trash");
        g.write(|db| {
            db.ops().moved(
                f["Trash"],
                1,
                f["All Mail"],
                &ops,
                Some(&moved(1, &[(2, 50)])),
            )
        });
        assert_eq!(g.state(m2), ("All Mail", Some(50), vec!["INBOX"]));
        assert_eq!(g.pending("All Mail"), [(50, g.label("INBOX", true))]);

        // Refused, the move takes the label changes along: INBOX, which Trash took off,
        // and Work.
        let m1 = g.message(1);
        g.write(|db| {
            db.ops().delete_messages(&[m1], false, 0)?;
            db.ops()
                .move_messages(&[m1], Some(f["Trash"]), f["Work"], 0)
        });
        assert_eq!(g.state(m1), ("All Mail", None, vec!["Important", "Work"]));
        let ops = g.replay_ops("All Mail");
        let moving: Vec<ReplayOp> = ops.into_iter().filter(|op| op.uid == 1).collect();
        assert_eq!(moving.len(), 1, "{moving:?}");
        g.write(|db| db.ops().failed(f["All Mail"], &moving, "NO"));
        assert_eq!(
            g.state(m1),
            ("All Mail", Some(1), vec!["INBOX", "Important"])
        );
        let failed = g
            .store
            .read(|db| db.ops().list(None))
            .unwrap()
            .into_iter()
            .filter(|op| op.status == OpStatus::Failed)
            .count();
        assert_eq!(failed, 3);
    }

    #[test]
    fn moved_to_trash_or_spam_loses_the_labels_gmail_takes_off() {
        let g = gmail();
        let f = &g.f;
        let m1 = g.message(1);
        g.write(|db| db.ops().label_messages(&[m1], &[f["Work"]], &[], 0));
        // At once, and queued after the move, where Gmail has made them already: then a
        // move back while this one is under way knows INBOX is to be added again, and a
        // refusal undoes them with it.
        let made = g.replay_ops("All Mail");
        g.write(|db| db.ops().applied(&made));
        g.write(|db| db.ops().move_messages(&[m1], None, f["Trash"], 0));
        assert_eq!(g.state(m1).2, ["Important", "Work"]);
        let ops = g.replay_ops("All Mail");
        assert!(matches!(
            ops[..],
            [ReplayOp {
                kind: ReplayKind::Move { .. },
                ..
            }]
        ));
        g.write(|db| {
            db.ops().moved(
                f["All Mail"],
                1,
                f["Trash"],
                &ops,
                Some(&moved(1, &[(1, 11)])),
            )
        });
        assert_eq!(g.pending("Trash"), [(11, g.label("INBOX", false))]);

        // Spam takes Important off too.
        g.write(|db| db.ops().move_messages(&[m1], None, f["Spam"], 0));
        assert_eq!(g.state(m1).2, ["Work"]);
        assert_eq!(g.count("Work"), (0, 0));
    }

    #[test]
    fn sync_keeps_unreplayed_label_changes() {
        let g = gmail();
        let m1 = g.message(1);
        g.write(|db| db.ops().archive_messages(&[m1], 0));
        // The sync's flag refresh still sees \Inbox: the removal isn't replayed yet.
        let server = FlagState {
            uid: 1,
            labels: Some(
                [FolderRole::Inbox, FolderRole::Important]
                    .map(GmailLabel::System)
                    .into(),
            ),
            ..Default::default()
        };
        g.write(|db| db.messages().update_flags(g.f["All Mail"], &[server]));
        assert_eq!(g.state(m1).2, ["Important"]);
    }

    #[test]
    fn moved_back_while_the_move_to_trash_is_made_it_gets_inbox_again() {
        let g = gmail();
        let (m1, f) = (g.message(1), &g.f);
        g.write(|db| db.ops().delete_messages(&[m1], false, 0));
        let in_flight = g.replay_ops("All Mail");
        g.write(|db| {
            db.ops()
                .move_messages(&[m1], Some(f["Trash"]), f["INBOX"], 0)
        });
        g.write(|db| {
            db.ops().moved(
                f["All Mail"],
                1,
                f["Trash"],
                &in_flight,
                Some(&moved(1, &[(1, 50)])),
            )
        });
        let back = g.replay_ops("Trash");
        g.write(|db| {
            db.ops().moved(
                f["Trash"],
                1,
                f["All Mail"],
                &back,
                Some(&moved(1, &[(50, 60)])),
            )
        });
        // Gmail took \Inbox off on the way into Trash: it must be added again.
        assert!(
            g.pending("All Mail")
                .contains(&(60, g.label("INBOX", true))),
            "{:?}",
            g.pending("All Mail")
        );
    }

    #[test]
    fn a_restore_cancelled_before_it_is_made_adds_no_inbox_in_trash() {
        let g = gmail();
        let (m2, f) = (g.message(2), &g.f);
        g.write(|db| {
            db.ops()
                .move_messages(&[m2], Some(f["Trash"]), f["INBOX"], 0)
        });
        g.write(|db| db.ops().delete_messages(&[m2], false, 0));
        let ops = g.replay_ops("Trash");
        g.write(|db| db.ops().moved(f["Trash"], 1, f["Trash"], &ops, None));
        // Only the last change to INBOX is made, with the first along.
        assert_eq!(g.pending("Trash"), [(2, g.label("INBOX", false))]);
        assert_eq!(g.replay_ops("Trash")[0].settled().count(), 2);
    }

    #[test]
    fn only_settled_ops_are_dismissed() {
        let f = setup();
        let m2 = f.message(f.inbox, 2);
        f.write(|db| db.ops().set_flags(&[m2], Flags::SEEN, Flags::empty(), 0));
        let op = f.ops()[0].id;
        assert!(matches!(
            f.store.write(|db| db.ops().dismiss(op)),
            Err(Error::Conflict(_))
        ));
        let ops = f.pending(f.inbox);
        f.write(|db| db.ops().failed(f.inbox, &ops, "NO"));
        assert_eq!(f.write(|db| db.ops().dismiss(op)), Some(f.account));
        assert_eq!(f.write(|db| db.ops().dismiss(op)), None);
    }

    #[test]
    fn settled_ops_are_dismissed_together_and_pending_ones_kept() {
        let f = setup();
        let (m2, m3) = (f.message(f.inbox, 2), f.message(f.inbox, 3));
        f.write(|db| db.ops().set_flags(&[m2], Flags::SEEN, Flags::empty(), 0));
        let ops = f.pending(f.inbox);
        f.write(|db| db.ops().failed(f.inbox, &ops, "NO"));
        f.write(|db| db.ops().set_flags(&[m3], Flags::SEEN, Flags::empty(), 0));

        let other = AccountId(f.account.0 + 1);
        assert!(!f.write(|db| db.ops().has_settled(Some(other))));
        assert!(
            f.write(|db| db.ops().dismiss_settled(Some(other)))
                .is_empty()
        );
        assert!(f.write(|db| db.ops().has_settled(None)));
        let dismissed = f.write(|db| db.ops().dismiss_settled(None));
        assert_eq!(dismissed.len(), 1);
        assert_eq!(dismissed[0].status, OpStatus::Failed);
        let left = f.ops();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].status, OpStatus::Pending);
        assert!(!f.write(|db| db.ops().has_settled(Some(f.account))));
    }
}
