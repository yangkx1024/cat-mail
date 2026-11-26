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
//!
//! The changes and the ops they queue are in `local`, replay's side of the queue in
//! `replay`; what both use, and listing and dismissing ops, here.

mod local;
mod replay;

use catmail_types::{
    AccountId, Flags, FolderId, MessageId, OpKind, OpStatus, PendingOp, PendingOpId,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::columns::{conversion_error, parse_text};
use crate::messages::shown;
use crate::touched::Touched;
use crate::{Error, Result};

pub use replay::{CopyCheckpoint, Moved, ReplayFolder, ReplayKind, ReplayOp};

/// An op `o` of account `?1` (any account if NULL) that failed or conflicted.
const SETTLED: &str = "o.status <> 'pending' AND (?1 IS NULL OR o.account_id = ?1)";

const COLUMNS: &str = "o.id, o.account_id, o.folder_id, o.message_id, o.kind, o.flags_add,
    o.flags_remove, o.target_folder_id, o.label_add, o.status, o.attempts, o.last_error,
    o.created_at, m.subject";

pub struct PendingOps<'c> {
    conn: &'c Connection,
}

impl<'c> PendingOps<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

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

    /// Records in `touched` the folders that show the message besides its location
    /// ([`shown`]), before a change to it.
    fn touch_shown(&self, id: MessageId, flagged: bool, touched: &mut Touched) -> Result<()> {
        touched.shown.extend(shown(self.conn, id.0, flagged)?);
        Ok(())
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
mod fixtures {
    use std::collections::{BTreeSet, HashMap};

    use catmail_types::{FlagState, Folder, FolderRole, GmailLabel};

    use super::*;
    use crate::Db;
    use crate::folders::RemoteFolder;
    use crate::messages::NewMessage;
    use crate::test_util::{self, TestStore};

    pub(super) struct Fixture {
        pub(super) store: TestStore,
        pub(super) account: AccountId,
        pub(super) inbox: FolderId,
        pub(super) archive: FolderId,
        pub(super) trash: FolderId,
    }

    pub(super) fn folder(name: &str, role: Option<FolderRole>) -> RemoteFolder {
        RemoteFolder {
            remote_name: name.into(),
            name: name.into(),
            delimiter: Some('/'),
            role,
            kind: catmail_types::FolderKind::Location,
            selectable: true,
            inferiors: true,
        }
    }

    /// INBOX with UIDs 1..=3 (1 is read), an empty Archive and Trash, all synced.
    pub(super) fn setup() -> Fixture {
        let store = test_util::store();
        let account = test_util::account(&store, "a@example.com");
        let folders: Vec<Folder> = store
            .write(|db| {
                db.reconcile_folders(
                    account,
                    &[
                        folder("INBOX", Some(FolderRole::Inbox)),
                        folder("Archive", Some(FolderRole::Archive)),
                        folder("Trash", Some(FolderRole::Trash)),
                    ],
                )?;
                let folders = db.folders().list(account)?;
                for f in &folders {
                    db.reset_uidvalidity(f.id, 7)?;
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
        pub(super) fn write<T>(&self, f: impl FnOnce(&Db<'_>) -> Result<T>) -> T {
            self.store.write(f).unwrap()
        }

        pub(super) fn message(&self, folder: FolderId, uid: u32) -> MessageId {
            test_util::message_id(&self.store, folder, uid)
        }

        /// (folder, uid, flags) of the row, if it exists.
        pub(super) fn row(&self, id: MessageId) -> Option<(FolderId, Option<u32>, Flags)> {
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

        pub(super) fn counts(&self, folder: FolderId) -> (u32, u32) {
            let f = self
                .store
                .read(|db| db.folders().get(folder))
                .unwrap()
                .unwrap();
            (f.total_count, f.unread_count)
        }

        pub(super) fn pending(&self, folder: FolderId) -> Vec<ReplayOp> {
            self.store.read(|db| db.ops().pending(folder)).unwrap()
        }

        pub(super) fn ops(&self) -> Vec<PendingOp> {
            self.store.read(|db| db.ops().list(None)).unwrap()
        }
    }

    pub(super) fn moved(uidvalidity: u32, uids: &[(u32, u32)]) -> Moved {
        Moved {
            uidvalidity,
            uids: uids.iter().copied().collect(),
        }
    }

    impl ReplayKind {
        pub(super) fn op_kind(&self) -> OpKind {
            match *self {
                Self::Flags { add, remove } => OpKind::Flags { add, remove },
                Self::Move { to, .. } => OpKind::Move { to },
                Self::Delete => OpKind::Delete,
                Self::Label { label, add, .. } => OpKind::Label { label, add },
            }
        }
    }

    /// The Gmail account of [`test_util::gmail`], its folders named without `[Gmail]/`.
    /// Message 1 is in All Mail, in INBOX and Important, message 2 in Trash.
    pub(super) struct Gmail {
        pub(super) store: TestStore,
        pub(super) account: AccountId,
        pub(super) f: HashMap<&'static str, FolderId>,
    }

    pub(super) fn gmail() -> Gmail {
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
        /// The folders named, as `Touched` holds them.
        pub(super) fn set(&self, names: &[&str]) -> BTreeSet<(AccountId, FolderId)> {
            names
                .iter()
                .map(|name| (self.account, self.f[*name]))
                .collect()
        }

        pub(super) fn write<T>(&self, f: impl FnOnce(&Db<'_>) -> Result<T>) -> T {
            self.store.write(f).unwrap()
        }

        pub(super) fn message(&self, n: u32) -> MessageId {
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
        pub(super) fn state(
            &self,
            id: MessageId,
        ) -> (&'static str, Option<u32>, Vec<&'static str>) {
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

        pub(super) fn replay_ops(&self, folder: &str) -> Vec<ReplayOp> {
            self.store
                .read(|db| db.ops().pending(self.f[folder]))
                .unwrap()
        }

        /// The folder's ops to replay: (UID, kind).
        pub(super) fn pending(&self, folder: &str) -> Vec<(u32, OpKind)> {
            self.replay_ops(folder)
                .into_iter()
                .map(|op| (op.uid, op.kind.op_kind()))
                .collect()
        }

        pub(super) fn label(&self, name: &str, add: bool) -> OpKind {
            OpKind::Label {
                label: self.f[name],
                add,
            }
        }

        /// What `Folders` counts for the label: (total, unread).
        pub(super) fn count(&self, name: &str) -> (u32, u32) {
            let f = self
                .store
                .read(|db| db.folders().get(self.f[name]))
                .unwrap()
                .unwrap();
            (f.total_count, f.unread_count)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::fixtures::*;

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
