//! Replay's bookkeeping: the ops it asks for, and what it reports back.

use std::collections::HashMap;

use catmail_types::{AccountId, Flags, FolderId, GmailLabel, MessageId, OpKind, PendingOpId};
use rusqlite::{OptionalExtension, Params, Row, params};

use super::{PendingOps, kind};
use crate::Result;
use crate::columns::parse_opt_text;
use crate::messages::shown;
use crate::touched::Touched;

/// Whether op `o` waits for a move of its message (`after_op`) that is still pending.
const WAITING: &str =
    "EXISTS (SELECT 1 FROM pending_ops w WHERE w.id = o.after_op AND w.status = 'pending')";

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

impl PendingOps<'_> {
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
    /// here: it waits for the move (`after_op`), and follows the message to its new place.
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
                let placed = self.place(
                    MessageId(row),
                    to,
                    new_place.map(|(_, uid)| uid),
                    &mut touched,
                )?;
                touched.folders.extend(placed);
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
                    self.fail_waiting(op.id, error, &mut touched)?;
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

    /// Fails, with the refused move `moved`, the label changes that wait for it
    /// (`after_op`): they were meant for where it was going.
    fn fail_waiting(&self, moved: PendingOpId, error: &str, touched: &mut Touched) -> Result<()> {
        let waiting: Vec<(PendingOpId, FolderId)> = self
            .conn
            .prepare_cached(
                "SELECT id, target_folder_id FROM pending_ops
                 WHERE after_op = ?1 AND kind = 'label' AND status = 'pending'
                 ORDER BY id",
            )?
            .query_map([moved.0], |r| {
                Ok((PendingOpId(r.get(0)?), FolderId(r.get(1)?)))
            })?
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
        touched.shown.retain(|&(_, f)| f != folder);
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
    /// Returns their folders, and the lists that showed them.
    fn drop_moving_rows(&self, filter: &str, params: impl Params) -> Result<Touched> {
        let rows: Vec<MessageId> = self
            .conn
            .prepare_cached(&format!(
                "SELECT id FROM messages WHERE uid IS NULL AND id IN (
                     SELECT message_id FROM pending_ops
                     WHERE {filter} AND kind = 'move' AND status = 'pending')"
            ))?
            .query_map(params, |r| Ok(MessageId(r.get(0)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut touched = Touched::new();
        for row in rows {
            let dropped = self.drop_unplaced(row, &mut touched)?;
            touched.folders.extend(dropped);
        }
        Ok(touched)
    }

    /// Deletes a message moved locally whose place on the server isn't known; the next
    /// sync fetches it where it is. Records the lists that showed it in `touched`.
    /// Returns its folder, if it was there.
    fn drop_unplaced(
        &self,
        row: MessageId,
        touched: &mut Touched,
    ) -> Result<Option<(AccountId, FolderId)>> {
        // Its labels go with the row, without a change of their own to announce it.
        let shown = shown(self.conn, row.0, false)?;
        let dropped = self
            .conn
            .prepare_cached(
                "DELETE FROM messages WHERE id = ?1 AND uid IS NULL
                 RETURNING account_id, folder_id",
            )?
            .query_row([row.0], folder_of)
            .optional()?;
        if dropped.is_some() {
            touched.shown.extend(shown);
        }
        Ok(dropped)
    }

    /// Gives a message moved to `folder` its UID there, or drops its row if that isn't
    /// known. Should a sync have stored the message under that UID already, that row
    /// wins. Returns the row's folder, if it was there.
    fn place(
        &self,
        row: MessageId,
        folder: FolderId,
        uid: Option<u32>,
        touched: &mut Touched,
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
        self.drop_unplaced(row, touched)
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
            self.drop_unplaced(row, &mut touched)?;
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use catmail_types::{DeleteMode, FolderRole, OpStatus};

    use super::*;
    use crate::ops::fixtures::*;

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
            db.ops().delete_messages(&[m2], DeleteMode::ToTrash, 0)?;
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
    fn a_new_generation_turns_ops_into_conflicts() {
        let f = setup();
        let (m1, m2) = (f.message(f.inbox, 1), f.message(f.inbox, 2));
        f.write(|db| {
            db.ops()
                .set_flags(&[m1], Flags::FLAGGED, Flags::empty(), 0)?;
            db.ops().move_messages(&[m2], None, f.archive, 0)
        });
        let touched = f.write(|db| db.reset_uidvalidity(f.inbox, 8));
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
        let touched = f.write(|db| db.reset_uidvalidity(f.archive, 8));
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
            db.reconcile_folders(
                f.account,
                &[
                    folder("INBOX", Some(FolderRole::Inbox)),
                    folder("Trash", Some(FolderRole::Trash)),
                ],
            )
        });
        assert_eq!(f.row(m2), Some((f.inbox, Some(2), Flags::empty())));
        // INBOX is gone: so is m3, moved out of it.
        f.write(|db| db.reconcile_folders(f.account, &[folder("Trash", Some(FolderRole::Trash))]));
        assert_eq!(f.row(m3), None);
        assert!(f.ops().is_empty());
        assert_eq!(f.counts(f.trash), (0, 0));
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
            db.ops().delete_messages(&[m1], DeleteMode::ToTrash, 0)?;
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
    fn moved_back_while_the_move_to_trash_is_made_it_gets_inbox_again() {
        let g = gmail();
        let (m1, f) = (g.message(1), &g.f);
        g.write(|db| db.ops().delete_messages(&[m1], DeleteMode::ToTrash, 0));
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
    fn dropped_rows_change_the_lists_that_showed_them() {
        let gone = |g: &Gmail, m| {
            g.store
                .read(|db| db.messages().location(m))
                .unwrap()
                .is_none()
        };
        let shown = |g: &Gmail, names: &[&str]| -> BTreeSet<_> {
            names.iter().map(|name| (g.account, g.f[name])).collect()
        };
        // Starred and Important, on its way to Trash; the server answers without COPYUID.
        let g = gmail();
        let (m1, f) = (g.message(1), &g.f);
        g.write(|db| {
            db.ops()
                .set_flags(&[m1], Flags::FLAGGED, Flags::empty(), 0)?;
            db.ops().delete_messages(&[m1], DeleteMode::ToTrash, 0)
        });
        let ops: Vec<ReplayOp> = g
            .replay_ops("All Mail")
            .into_iter()
            .filter(|op| matches!(op.kind, ReplayKind::Move { .. }))
            .collect();
        let touched = g.write(|db| db.ops().moved(f["All Mail"], 1, f["Trash"], &ops, None));
        assert!(gone(&g, m1));
        assert_eq!(touched.shown, shown(&g, &["Important", "Starred"]));

        // A new generation of the folder it is moving out of drops it the same way.
        let g = gmail();
        let (m1, f) = (g.message(1), &g.f);
        g.write(|db| db.ops().delete_messages(&[m1], DeleteMode::ToTrash, 0));
        let touched = g.write(|db| db.ops().end_generation(f["All Mail"], Some(2)));
        assert!(gone(&g, m1));
        assert_eq!(touched.shown, shown(&g, &["Important"]));
    }

    #[test]
    fn a_restore_cancelled_before_it_is_made_adds_no_inbox_in_trash() {
        let g = gmail();
        let (m2, f) = (g.message(2), &g.f);
        g.write(|db| {
            db.ops()
                .move_messages(&[m2], Some(f["Trash"]), f["INBOX"], 0)
        });
        g.write(|db| db.ops().delete_messages(&[m2], DeleteMode::ToTrash, 0));
        let ops = g.replay_ops("Trash");
        g.write(|db| db.ops().moved(f["Trash"], 1, f["Trash"], &ops, None));
        // Only the last change to INBOX is made, with the first along.
        assert_eq!(g.pending("Trash"), [(2, g.label("INBOX", false))]);
        assert_eq!(g.replay_ops("Trash")[0].settled().count(), 2);
    }
}
