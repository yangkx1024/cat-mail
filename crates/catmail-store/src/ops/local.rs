//! Local changes to messages, and the ops that queue them.

use std::collections::{HashMap, hash_map::Entry};

use catmail_types::{
    AccountId, DeleteMode, DeleteOutcome, Flags, Folder, FolderId, FolderKind, MessageId, MovePlan,
    OpKind, PendingOpId, Unmovable,
};
use rusqlite::{OptionalExtension, params};

use super::PendingOps;
use crate::folders::Folders;
use crate::messages::MOVING;
use crate::touched::Touched;
use crate::{Error, Result};

/// The flags a user may change. `\Deleted` goes with deletion and `\Draft` with drafts.
const USER_FLAGS: Flags =
    Flags::from_bits(Flags::SEEN.bits() | Flags::FLAGGED.bits() | Flags::ANSWERED.bits());

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

impl PendingOps<'_> {
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
            touched
                .ops
                .insert(self.queue(&m, OpKind::Flags { add, remove }, now)?);
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
    /// actually changed. Starred is no label: it is `\Flagged`. INBOX added to a message in
    /// Trash or Spam moves it to INBOX ([`MovePlan::labelling`]).
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
            let mut m = self.local(id)?;
            let account_folders = self.folders_of(&mut folders, m.account)?;
            for &label in add.iter().chain(remove) {
                match account_folders.iter().find(|f| f.id == label) {
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
            // Given `\Inbox` in Trash or Spam, Gmail takes it out to INBOX: move it there,
            // and change its other labels after the move, as it is then.
            if let Some(plan) = add
                .iter()
                .find_map(|&label| MovePlan::labelling(account_folders, m.folder, label))
            {
                self.carry_out(&m, plan, now, &mut touched)?;
                m = self.local(id)?;
            }
            let mut changed = false;
            for &label in remove {
                changed |= self.label_one(&m, label, false, now, &mut touched)?;
            }
            for &label in add {
                changed |= self.label_one(&m, label, true, now, &mut touched)?;
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
        let moved;
        let m = match plan.to {
            Some(to) => {
                self.move_one(m, to, now, touched)?;
                moved = self.local(m.id)?;
                &moved
            }
            None => m,
        };
        let mut changed = false;
        for label in plan.remove {
            changed |= self.label_one(m, label, false, now, touched)?;
        }
        if let Some(label) = plan.add {
            changed |= self.label_one(m, label, true, now, touched)?;
        }
        // A move noted them already.
        if changed && plan.to.is_none() {
            self.touch_shown(m.id, false, touched)?;
        }
        Ok(())
    }

    /// Adds `label` to the message or takes it off, and queues that if it changed; returns
    /// whether it did. The caller notes the other labels that list the message
    /// ([`touch_shown`](Self::touch_shown)), once for all its changes. The op goes where
    /// the server has the message; while a move of it is pending, it waits for the move
    /// (`after_op`) and follows it (see `pending`).
    fn label_one(
        &self,
        m: &Local,
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
            .extend([(m.account, label), (m.account, m.folder)]);
        touched
            .ops
            .insert(self.queue(m, OpKind::Label { label, add }, now)?);
        Ok(true)
    }

    /// Moves each message to its account's Trash, or deletes it for good, as `mode` and
    /// its [`DeleteOutcome`] say. Refused, all of it, if `mode` leaves a message nowhere
    /// to go but for good.
    pub fn delete_messages(
        &self,
        messages: &[MessageId],
        mode: DeleteMode,
        now: i64,
    ) -> Result<Touched> {
        let mut touched = Touched::new();
        let mut folders = HashMap::new();
        for &id in messages {
            let m = self.local(id)?;
            touched.folders.insert((m.account, m.folder));
            if mode != DeleteMode::Permanent {
                let folders = self.folders_of(&mut folders, m.account)?;
                match DeleteOutcome::of(folders, m.folder) {
                    DeleteOutcome::ToTrash(trash) => {
                        // On Gmail it loses INBOX there too.
                        let plan = MovePlan::of(folders, m.folder, None, trash).map_err(|_| {
                            Error::Invalid("the Trash folder can't hold messages".into())
                        })?;
                        self.carry_out(&m, plan, now, &mut touched)?;
                        continue;
                    }
                    DeleteOutcome::NoTrash if mode == DeleteMode::ToTrash => {
                        return Err(Error::Invalid(
                            "the account has no Trash folder; delete permanently instead".into(),
                        ));
                    }
                    DeleteOutcome::NoTrash if mode == DeleteMode::OnlyToTrash => {
                        return Err(Error::Invalid("the account has no Trash folder".into()));
                    }
                    DeleteOutcome::Permanent if mode == DeleteMode::OnlyToTrash => {
                        return Err(Error::Invalid(format!(
                            "message {} is in Trash already",
                            id.0
                        )));
                    }
                    DeleteOutcome::NoTrash | DeleteOutcome::Permanent => {}
                }
            }
            let op = match m.moving {
                // Never made it to the target: delete it where the server still has it.
                // What waited for the move was meant for where it was going, and goes: on
                // Gmail, `+\Inbox` in Trash would take it out of Trash before the delete.
                Some(op) => {
                    self.conn
                        .prepare_cached(
                            "UPDATE pending_ops SET kind = 'delete', target_folder_id = NULL
                             WHERE id = ?1",
                        )?
                        .execute([op.0])?;
                    self.conn
                        .prepare_cached("DELETE FROM pending_ops WHERE after_op = ?1")?
                        .execute([op.0])?;
                    op
                }
                None => self.queue(&m, OpKind::Delete, now)?,
            };
            touched.ops.insert(op);
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
            return self.delete_messages(&[MessageId(row)], DeleteMode::Permanent, now);
        }
        self.insert(
            account,
            (folder, uidvalidity, uid),
            OpKind::Delete,
            None,
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
        let op = match m.moving {
            // Still on its way: send it to the new place instead. Back to where the server
            // has it, the op stays, and is resolved locally at replay; it may be in flight.
            Some(op) => {
                self.conn
                    .prepare_cached("UPDATE pending_ops SET target_folder_id = ?2 WHERE id = ?1")?
                    .execute(params![op.0, to.0])?;
                op
            }
            None => self.queue(m, OpKind::Move { to }, now)?,
        };
        touched.ops.insert(op);
        // MODSEQ is per mailbox.
        self.conn
            .prepare_cached(
                "UPDATE messages SET folder_id = ?2, uid = NULL, modseq = NULL WHERE id = ?1",
            )?
            .execute(params![m.id.0, to.0])?;
        Ok(())
    }

    /// Queues `kind` for the message, where the server has it.
    fn queue(&self, m: &Local, kind: OpKind, now: i64) -> Result<PendingOpId> {
        // Every row has a UID or a pending move; the repository keeps it so.
        let server = m.server.ok_or_else(|| {
            Error::Conflict(format!(
                "message {} is not on the server any more; sync and try again",
                m.id
            ))
        })?;
        // A label change made while the message is moving waits for the move; a flag
        // change is made before it, where the message still is.
        let after = match kind {
            OpKind::Label { .. } => m.moving,
            _ => None,
        };
        self.insert(m.account, server, kind, Some(m.id), after, now)
    }

    /// Queues `kind` for the message the server has at `(folder, UIDVALIDITY, UID)`,
    /// shown locally as `message` if there is such a row, after the move `after`.
    fn insert(
        &self,
        account: AccountId,
        (folder, uidvalidity, uid): (FolderId, u32, u32),
        kind: OpKind,
        message: Option<MessageId>,
        after: Option<PendingOpId>,
        now: i64,
    ) -> Result<PendingOpId> {
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
                                          after_op, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                after.map(|op| op.0),
                now
            ])?;
        Ok(PendingOpId(self.conn.last_insert_rowid()))
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
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use catmail_types::{FlagState, FolderRole, GmailLabel, OpStatus};

    use super::*;
    use crate::messages::NewMessage;
    use crate::ops::fixtures::*;
    use crate::ops::{ReplayKind, ReplayOp};
    use crate::test_util;

    #[test]
    fn deleting_by_server_location_leaves_what_the_user_moved() {
        let f = setup();
        let (inbox, account) = (f.inbox, f.account);
        let m2 = f.message(inbox, 2);
        f.write(|db| db.ops().move_messages(&[m2], None, f.archive, 1));
        let delete = |uid| f.write(|db| db.ops().delete_on_server(account, inbox, 7, uid, 2));

        assert!(delete(2).is_empty(), "moved by the user meanwhile");
        // A row of that generation goes with it; a UID never synced is queued all the same.
        assert_eq!(delete(3).without_ops(), Touched::from([(account, inbox)]));
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

    #[test]
    fn flag_changes_are_queued_for_what_changed() {
        let f = setup();
        let (m1, m2) = (f.message(f.inbox, 1), f.message(f.inbox, 2));
        let touched = f.write(|db| {
            db.ops()
                .set_flags(&[m1, m2], Flags::SEEN | Flags::FLAGGED, Flags::empty(), 0)
        });
        assert_eq!(touched.without_ops(), Touched::from([(f.account, f.inbox)]));
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
            []
        );
        assert_eq!(f.row(m1).unwrap().2, Flags::FLAGGED);
        assert_eq!(f.row(m2).unwrap().2, Flags::SEEN | Flags::FLAGGED);
    }

    #[test]
    fn moved_messages_show_in_the_target_until_the_server_moves_them() {
        let f = setup();
        let m2 = f.message(f.inbox, 2);
        let touched = f.write(|db| db.ops().move_messages(&[m2], None, f.archive, 0));
        assert_eq!(
            touched.without_ops(),
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
            []
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
    fn deleting_moves_to_trash_and_deletes_from_it() {
        let f = setup();
        let (m1, m2, m3) = (
            f.message(f.inbox, 1),
            f.message(f.inbox, 2),
            f.message(f.inbox, 3),
        );
        f.write(|db| db.ops().delete_messages(&[m2], DeleteMode::ToTrash, 0));
        assert_eq!(f.row(m2), Some((f.trash, None, Flags::empty())));
        // In Trash (even before it got there on the server): for good.
        f.write(|db| db.ops().delete_messages(&[m2], DeleteMode::ToTrash, 0));
        assert_eq!(f.row(m2), None);
        f.write(|db| db.ops().delete_messages(&[m3], DeleteMode::Permanent, 0));
        assert_eq!(f.row(m3), None);
        let kinds: Vec<(u32, OpKind)> = f
            .pending(f.inbox)
            .iter()
            .map(|op| (op.uid, op.kind.op_kind()))
            .collect();
        assert_eq!(kinds, [(2, OpKind::Delete), (3, OpKind::Delete)]);
        assert_eq!(f.counts(f.trash), (0, 0));

        // Without a Trash folder, only when that was asked.
        f.write(|db| {
            db.conn.execute("UPDATE folders SET role = NULL", [])?;
            Ok(())
        });
        assert!(matches!(
            f.store
                .write(|db| db.ops().delete_messages(&[m1], DeleteMode::ToTrash, 0)),
            Err(Error::Invalid(_))
        ));
        assert!(f.row(m1).is_some());
    }

    #[test]
    fn a_change_tells_its_ops_even_once_their_row_is_gone() {
        let f = setup();
        let (m1, m2) = (f.message(f.inbox, 1), f.message(f.inbox, 2));
        let queued = |f: &Fixture| f.ops().iter().map(|op| op.id).collect::<BTreeSet<_>>();

        let flagged = f
            .write(|db| db.ops().set_flags(&[m1], Flags::FLAGGED, Flags::empty(), 0))
            .ops;
        let moved = f
            .write(|db| db.ops().move_messages(&[m2], None, f.archive, 0))
            .ops;
        assert_eq!(&flagged | &moved, queued(&f));
        // Still on its way: sent elsewhere (back, then to Trash) by the same op.
        let redirected = f.write(|db| db.ops().move_messages(&[m2], None, f.inbox, 0));
        assert_eq!(redirected.ops, moved);
        let trashed = f.write(|db| db.ops().delete_messages(&[m2], DeleteMode::OnlyToTrash, 0));
        assert_eq!(trashed.ops, moved);

        // INBOX rebuilt on the server: the move's row goes, its op stays, as a conflict.
        f.write(|db| db.reset_uidvalidity(f.inbox, 2));
        assert_eq!(f.row(m2), None);
        let conflict = f
            .ops()
            .into_iter()
            .find(|op| moved.contains(&op.id))
            .unwrap();
        assert_eq!(conflict.message_id, None);
        assert_eq!(conflict.status, OpStatus::Conflict);
    }

    #[test]
    fn only_to_trash_never_deletes_for_good() {
        let f = setup();
        let (m1, m2) = (f.message(f.inbox, 1), f.message(f.inbox, 2));
        let m1_before = f.row(m1);
        f.write(|db| db.ops().delete_messages(&[m2], DeleteMode::OnlyToTrash, 0));
        assert_eq!(f.row(m2), Some((f.trash, None, Flags::empty())));
        // One of them in Trash already: none of them goes.
        assert!(matches!(
            f.store.write(|db| db
                .ops()
                .delete_messages(&[m1, m2], DeleteMode::OnlyToTrash, 0)),
            Err(Error::Invalid(_))
        ));
        assert_eq!(f.row(m1), m1_before);
        assert_eq!(f.row(m2), Some((f.trash, None, Flags::empty())));

        f.write(|db| {
            db.conn.execute("UPDATE folders SET role = NULL", [])?;
            Ok(())
        });
        assert!(matches!(
            f.store
                .write(|db| db.ops().delete_messages(&[m1], DeleteMode::OnlyToTrash, 0)),
            Err(Error::Invalid(_))
        ));
        assert!(f.row(m1).is_some());
    }

    #[test]
    fn deleting_decides_for_each_message() {
        let f = setup();
        let m2 = f.message(f.inbox, 2);
        // Another account, with no Trash folder.
        let other = test_util::account(&f.store, "b@example.com");
        let other_inbox = f.write(|db| {
            db.reconcile_folders(other, &[folder("INBOX", Some(FolderRole::Inbox))])?;
            let inbox = db.folders().list(other)?[0].id;
            db.reset_uidvalidity(inbox, 7)?;
            let message = NewMessage {
                state: FlagState {
                    uid: 1,
                    ..Default::default()
                },
                ..Default::default()
            };
            db.messages().upsert(other, inbox, &[message])?;
            Ok(inbox)
        });
        let lone = f.message(other_inbox, 1);
        // One change: to Trash, and for good where there is none. Refused as a whole
        // unless for good is asked.
        assert!(matches!(
            f.store.write(|db| db
                .ops()
                .delete_messages(&[m2, lone], DeleteMode::ToTrash, 0)),
            Err(Error::Invalid(_))
        ));
        assert_eq!(f.row(m2), Some((f.inbox, Some(2), Flags::empty())));
        f.write(|db| {
            db.ops()
                .delete_messages(&[m2, lone], DeleteMode::ByOutcome, 0)
        });
        assert_eq!(f.row(m2), Some((f.trash, None, Flags::empty())));
        assert_eq!(f.row(lone), None);
        let kinds: Vec<(u32, OpKind)> = f
            .pending(other_inbox)
            .iter()
            .map(|op| (op.uid, op.kind.op_kind()))
            .collect();
        assert_eq!(kinds, [(1, OpKind::Delete)]);
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
    fn moves_stay_within_the_account() {
        let f = setup();
        let other = test_util::account(&f.store, "b@example.com");
        let theirs = f.write(|db| {
            db.reconcile_folders(other, &[folder("INBOX", Some(FolderRole::Inbox))])?;
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

    #[test]
    fn moves_between_gmail_labels_change_labels() {
        let g = gmail();
        let (m1, f) = (g.message(1), &g.f);
        let touched = g.write(|db| {
            db.ops()
                .move_messages(&[m1], Some(f["INBOX"]), f["Work"], 0)
        });
        assert_eq!(touched.folders, g.set(&["INBOX", "Work", "All Mail"]));
        // Important lists it too, with its labels.
        let all = g.set(&["INBOX", "Work", "Important", "All Mail"]);
        assert_eq!(touched.lists().collect::<BTreeSet<_>>(), all);
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
            db.ops().delete_messages(&[m1], DeleteMode::ToTrash, 0)
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
        // Only the label added is changed: the others list the message.
        assert_eq!(touched.folders, g.set(&["Work", "All Mail"]));
        let all = g.set(&["INBOX", "Work", "Important", "All Mail"]);
        assert_eq!(touched.lists().collect::<BTreeSet<_>>(), all);
        // So does undoing it.
        let ops = g.replay_ops("All Mail");
        let touched = g.write(|db| db.ops().failed(f["All Mail"], &ops, "NO"));
        let lists: BTreeSet<_> = touched.lists().collect();
        assert!(g.set(&["INBOX", "Work", "Important"]).is_subset(&lists));
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
    fn label_changes_waiting_for_a_move_go_when_it_turns_into_a_delete() {
        let g = gmail();
        let (m2, f) = (g.message(2), &g.f);
        g.write(|db| {
            db.ops()
                .move_messages(&[m2], Some(f["Trash"]), f["INBOX"], 0)?;
            db.ops().delete_messages(&[m2], DeleteMode::Permanent, 0)
        });
        // Not `+\Inbox` in Trash first: Gmail would take the message out of Trash.
        assert_eq!(g.pending("Trash"), [(2, OpKind::Delete)]);
        assert_eq!(g.store.read(|db| db.ops().list(None)).unwrap().len(), 1);
    }

    #[test]
    fn inbox_added_in_trash_moves_the_message_out() {
        let g = gmail();
        let (m2, f) = (g.message(2), &g.f);
        g.write(|db| {
            db.ops()
                .label_messages(&[m2], &[f["Work"], f["INBOX"]], &[], 0)
        });
        assert_eq!(g.state(m2), ("All Mail", None, vec!["INBOX", "Work"]));
        // Not `+\Inbox` in Trash, which Gmail would take as a move of its own: both
        // labels wait for the move.
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
        assert_eq!(
            g.pending("All Mail"),
            [(50, g.label("INBOX", true)), (50, g.label("Work", true))]
        );
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
}
