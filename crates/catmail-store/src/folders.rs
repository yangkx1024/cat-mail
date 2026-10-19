use std::collections::HashSet;

use catmail_types::{AccountId, Folder, FolderId, FolderRole};
use rusqlite::{Connection, OptionalExtension, Row, named_params, params};

use crate::Result;
use crate::columns::parse_opt_text;
use crate::ops::{PendingOps, Touched};

const COLUMNS: &str = "id, account_id, name, delimiter, role, selectable,
    unread_count, total_count, last_synced_at";

/// A mailbox as listed by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFolder {
    pub remote_name: String,
    pub name: String,
    pub delimiter: Option<char>,
    pub role: Option<FolderRole>,
    pub selectable: bool,
}

/// What [`Folders::reconcile`] changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// Folders were added, removed or changed.
    pub changed: bool,
    /// Other folders whose messages changed with them, and the accounts whose queued
    /// changes did: messages moved locally into a deleted folder go back.
    pub touched: Touched,
}

/// What the sync engine needs to resume a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSyncState {
    pub id: FolderId,
    pub remote_name: String,
    pub name: String,
    pub selectable: bool,
    pub uidvalidity: Option<u32>,
    /// Both cursors describe the last *completed* sync; see the `folders` schema.
    pub uidnext: Option<u32>,
    pub highest_modseq: Option<u64>,
    /// Messages stored now, and the highest stored UID (0 if none); enough to decide
    /// whether anything changed without loading every UID.
    pub message_count: u32,
    pub max_uid: u32,
}

pub struct Folders<'c> {
    conn: &'c Connection,
}

impl<'c> Folders<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// INBOX first, then by name.
    pub fn list(&self, account: AccountId) -> Result<Vec<Folder>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM folders WHERE account_id = ?1
             ORDER BY remote_name <> 'INBOX', name COLLATE NOCASE, id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([account.0], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get(&self, id: FolderId) -> Result<Option<Folder>> {
        let sql = format!("SELECT {COLUMNS} FROM folders WHERE id = ?1");
        Ok(self.conn.query_row(&sql, [id.0], from_row).optional()?)
    }

    /// Looks a folder up by its display name, its remote name, or (case-insensitively,
    /// as IMAP does) `INBOX`.
    pub fn find_by_name(&self, account: AccountId, name: &str) -> Result<Option<Folder>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM folders
             WHERE account_id = :account
               AND (name = :name OR remote_name = :name
                    OR (remote_name = 'INBOX' AND upper(:name) = 'INBOX'))
             ORDER BY name <> :name
             LIMIT 1"
        );
        Ok(self
            .conn
            .query_row(
                &sql,
                named_params! { ":account": account.0, ":name": name },
                from_row,
            )
            .optional()?)
    }

    /// The account's selectable folder with `role` (id and server name), if it has one.
    pub fn by_role(
        &self,
        account: AccountId,
        role: FolderRole,
    ) -> Result<Option<(FolderId, String)>> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT id, remote_name FROM folders
                 WHERE account_id = ?1 AND role = ?2 AND selectable ORDER BY id LIMIT 1",
            )?
            .query_row(params![account.0, role.as_str()], |r| {
                Ok((FolderId(r.get(0)?), r.get(1)?))
            })
            .optional()?)
    }

    /// INBOX first, then by id.
    pub fn sync_states(&self, account: AccountId) -> Result<Vec<FolderSyncState>> {
        // Messages moved here locally and not yet on the server have no UID; sync leaves
        // them out of its counts.
        let mut stmt = self.conn.prepare(
            "SELECT id, remote_name, name, selectable, uidvalidity, uidnext, highest_modseq,
                    total_count - (SELECT count(*) FROM messages
                                   WHERE folder_id = folders.id AND uid IS NULL),
                    (SELECT ifnull(max(uid), 0) FROM messages WHERE folder_id = folders.id)
             FROM folders WHERE account_id = ?1
             ORDER BY remote_name <> 'INBOX', id",
        )?;
        let rows = stmt.query_map([account.0], |r| {
            Ok(FolderSyncState {
                id: FolderId(r.get(0)?),
                remote_name: r.get(1)?,
                name: r.get(2)?,
                selectable: r.get(3)?,
                uidvalidity: r.get(4)?,
                uidnext: r.get(5)?,
                highest_modseq: r.get::<_, Option<i64>>(6)?.map(|m| m as u64),
                message_count: r.get(7)?,
                max_uid: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Makes the account's folders match `remote`: inserts new ones, updates changed
    /// attributes, and deletes folders the server no longer lists (with their messages).
    pub fn reconcile(&self, account: AccountId, remote: &[RemoteFolder]) -> Result<Reconciled> {
        let mut changed = false;
        let mut touched = Touched::new();
        let mut upsert = self.conn.prepare(
            "INSERT INTO folders (account_id, remote_name, name, delimiter, role, selectable)
             VALUES (:account, :remote_name, :name, :delimiter, :role, :selectable)
             ON CONFLICT (account_id, remote_name) DO UPDATE SET
                 name = excluded.name, delimiter = excluded.delimiter,
                 role = excluded.role, selectable = excluded.selectable
             WHERE (name, delimiter, role, selectable)
                   IS NOT (excluded.name, excluded.delimiter, excluded.role, excluded.selectable)",
        )?;
        for folder in remote {
            changed |= upsert.execute(named_params! {
                ":account": account.0,
                ":remote_name": folder.remote_name,
                ":name": folder.name,
                ":delimiter": folder.delimiter.map(String::from),
                ":role": folder.role.map(FolderRole::as_str),
                ":selectable": folder.selectable,
            })? > 0;
        }

        let listed: HashSet<&str> = remote.iter().map(|f| f.remote_name.as_str()).collect();
        let gone: Vec<i64> = self
            .sync_states(account)?
            .into_iter()
            .filter(|f| !listed.contains(f.remote_name.as_str()))
            .map(|f| f.id.0)
            .collect();
        for id in gone {
            touched.extend(PendingOps::new(self.conn).forget_folder(FolderId(id))?);
            self.forget_remote_draft_copies(FolderId(id), None)?;
            self.conn
                .execute("DELETE FROM folders WHERE id = ?1", [id])?;
            changed = true;
        }

        // A mailbox deleted while it had children stays listed as a \Noselect node
        // (RFC 3501 DELETE). Sync never opens such a folder, so its old messages would
        // linger; the row stays for the hierarchy.
        let emptied: Vec<i64> = self
            .conn
            .prepare(
                "SELECT id FROM folders
                 WHERE account_id = ?1 AND selectable = 0 AND uidvalidity IS NOT NULL",
            )?
            .query_map([account.0], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in emptied {
            touched.extend(self.end_generation(FolderId(id), None)?);
            changed = true;
        }
        Ok(Reconciled { changed, touched })
    }

    /// Starts a new UIDVALIDITY generation (or the first one): every message row and
    /// cursor of the old generation is dropped, pending operations against old UIDs
    /// become conflicts, and draft copies addressed by old UIDs are forgotten.
    /// Returns the folders whose messages or queued changes this changed, besides
    /// `folder`'s own messages: local moves out of it are undone.
    pub fn reset_uidvalidity(&self, folder: FolderId, uidvalidity: u32) -> Result<Touched> {
        self.end_generation(folder, Some(uidvalidity))
    }

    /// Drops everything tied to the folder's UIDs except what belongs to `next`, the
    /// generation that replaces it (`None`: the folder no longer holds messages at all).
    fn end_generation(&self, folder: FolderId, next: Option<u32>) -> Result<Touched> {
        self.conn
            .execute("DELETE FROM messages WHERE folder_id = ?1", [folder.0])?;
        let touched = PendingOps::new(self.conn).end_generation(folder, next)?;
        self.forget_remote_draft_copies(folder, next)?;
        self.conn.execute(
            "UPDATE folders SET uidvalidity = ?2, uidnext = NULL, highest_modseq = NULL
             WHERE id = ?1",
            params![folder.0, next],
        )?;
        Ok(touched)
    }

    /// Records a completed sync.
    pub fn set_synced(
        &self,
        folder: FolderId,
        uidnext: Option<u32>,
        highest_modseq: Option<u64>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET uidnext = ?2, highest_modseq = ?3, last_synced_at = unixepoch()
             WHERE id = ?1",
            params![folder.0, uidnext, highest_modseq.map(|m| m as i64)],
        )?;
        Ok(())
    }

    /// A draft's remote UID is meaningless once its folder is gone or has a new
    /// UIDVALIDITY; folder, UIDVALIDITY and UID are cleared together.
    fn forget_remote_draft_copies(&self, folder: FolderId, keep: Option<u32>) -> Result<()> {
        self.conn.execute(
            "UPDATE drafts
             SET remote_folder_id = NULL, remote_uidvalidity = NULL, remote_uid = NULL
             WHERE remote_folder_id = ?1 AND remote_uidvalidity IS NOT ?2",
            params![folder.0, keep],
        )?;
        Ok(())
    }
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Folder> {
    let delimiter: Option<String> = row.get("delimiter")?;
    Ok(Folder {
        id: FolderId(row.get("id")?),
        account_id: AccountId(row.get("account_id")?),
        name: row.get("name")?,
        delimiter: delimiter.and_then(|d| d.chars().next()),
        role: parse_opt_text(row, "role")?,
        selectable: row.get("selectable")?,
        unread_count: row.get("unread_count")?,
        total_count: row.get("total_count")?,
        last_synced_at: row.get("last_synced_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::NewMessage;
    use crate::{Store, test_util};

    fn remote(remote_name: &str, name: &str, role: Option<FolderRole>) -> RemoteFolder {
        RemoteFolder {
            remote_name: remote_name.into(),
            name: name.into(),
            delimiter: Some('/'),
            role,
            selectable: true,
        }
    }

    fn message(uid: u32) -> NewMessage {
        NewMessage {
            state: catmail_types::FlagState {
                uid,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn reconcile_inserts_updates_and_deletes() {
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        let listed = vec![
            remote("Sent", "Sent", None),
            remote("INBOX", "INBOX", Some(FolderRole::Inbox)),
            remote("&XfJT0ZAB-", "已发送", Some(FolderRole::Sent)),
        ];
        assert!(
            store
                .write(|db| db.folders().reconcile(account, &listed))
                .unwrap()
                .changed
        );
        assert!(
            !store
                .write(|db| db.folders().reconcile(account, &listed))
                .unwrap()
                .changed
        );

        let folders = store.read(|db| db.folders().list(account)).unwrap();
        let names: Vec<_> = folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["INBOX", "Sent", "已发送"]);
        assert_eq!(folders[0].delimiter, Some('/'));
        assert_eq!(folders[2].role, Some(FolderRole::Sent));
        let by_role = |role| {
            store
                .read(|db| db.folders().by_role(account, role))
                .unwrap()
                .map(|(_, remote_name)| remote_name)
        };
        assert_eq!(by_role(FolderRole::Sent).as_deref(), Some("&XfJT0ZAB-"));
        assert_eq!(by_role(FolderRole::Trash), None);

        // "Sent" gains a role, the UTF-7 folder disappears together with its messages.
        let sent_utf7 = folders[2].id;
        store
            .write(|db| db.messages().upsert(account, sent_utf7, &[message(1)]))
            .unwrap();
        let listed = vec![
            remote("INBOX", "INBOX", Some(FolderRole::Inbox)),
            remote("Sent", "Sent", Some(FolderRole::Sent)),
        ];
        assert!(
            store
                .write(|db| db.folders().reconcile(account, &listed))
                .unwrap()
                .changed
        );
        let folders = store.read(|db| db.folders().list(account)).unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[1].role, Some(FolderRole::Sent));
        assert_eq!(store.read(|db| db.messages().count(sent_utf7)).unwrap(), 0);
    }

    #[test]
    fn becoming_a_hierarchy_node_ends_the_generation() {
        // RFC 3501 DELETE of a mailbox with children empties it and leaves its name as
        // \Noselect, which sync never opens again.
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        store
            .write(|db| {
                db.folders()
                    .reconcile(account, &[remote("Parent", "Parent", None)])
            })
            .unwrap();
        let folder = store.read(|db| db.folders().list(account)).unwrap()[0].id;
        store
            .write(|db| {
                db.folders().reset_uidvalidity(folder, 7)?;
                db.messages()
                    .upsert(account, folder, &[message(1), message(2)])?;
                db.folders().set_synced(folder, Some(3), Some(10))?;
                db.conn.execute(
                    "INSERT INTO pending_ops (account_id, folder_id, uidvalidity, uid, kind)
                     VALUES (?1, ?2, 7, 1, 'flags')",
                    rusqlite::params![account.0, folder.0],
                )?;
                Ok(())
            })
            .unwrap();

        let mut node = remote("Parent", "Parent", None);
        node.selectable = false;
        assert!(
            store
                .write(|db| db.folders().reconcile(account, &[node]))
                .unwrap()
                .changed
        );
        store
            .read(|db| {
                let f = db.folders().get(folder)?.expect("the hierarchy row stays");
                assert!(!f.selectable);
                assert_eq!((f.total_count, f.unread_count), (0, 0));
                assert_eq!(db.messages().count(folder)?, 0);
                let state = &db.folders().sync_states(account)?[0];
                assert_eq!(
                    (state.uidvalidity, state.uidnext, state.highest_modseq),
                    (None, None, None)
                );
                let status: String =
                    db.conn
                        .query_row("SELECT status FROM pending_ops", [], |r| r.get(0))?;
                assert_eq!(status, "conflict");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn find_by_name() {
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        let other = test_util::account(&store, "b@example.com");
        let listed = vec![
            remote("INBOX", "INBOX", Some(FolderRole::Inbox)),
            remote("&XfJT0ZAB-", "已发送", None),
        ];
        store
            .write(|db| db.folders().reconcile(account, &listed))
            .unwrap();
        store
            .write(|db| db.folders().reconcile(other, &listed))
            .unwrap();

        let find = |name: &str| {
            store
                .read(|db| db.folders().find_by_name(account, name))
                .unwrap()
                .map(|f| (f.account_id, f.name))
        };
        assert_eq!(find("已发送"), Some((account, "已发送".into())));
        assert_eq!(find("&XfJT0ZAB-"), Some((account, "已发送".into())));
        assert_eq!(find("inbox"), Some((account, "INBOX".into())));
        assert_eq!(find("Missing"), None);
    }

    #[test]
    fn uidvalidity_reset_drops_the_old_generation() {
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        store
            .write(|db| {
                db.folders()
                    .reconcile(account, &[remote("INBOX", "INBOX", None)])
            })
            .unwrap();
        let folder = store.read(|db| db.folders().list(account)).unwrap()[0].id;
        store
            .write(|db| {
                db.folders().reset_uidvalidity(folder, 7)?;
                db.messages()
                    .upsert(account, folder, &[message(1), message(2)])?;
                db.folders().set_synced(folder, Some(3), Some(10))
            })
            .unwrap();
        store
            .write(|db| {
                db.conn.execute_batch(&format!(
                    "INSERT INTO pending_ops (account_id, folder_id, uidvalidity, uid, kind)
                     VALUES ({a}, {f}, 7, 1, 'flags'), ({a}, {f}, 9, 1, 'flags');
                     INSERT INTO drafts (account_id, remote_folder_id, remote_uidvalidity, remote_uid)
                     VALUES ({a}, {f}, 7, 1);",
                    a = account.0,
                    f = folder.0
                ))?;
                Ok(())
            })
            .unwrap();

        store
            .write(|db| db.folders().reset_uidvalidity(folder, 9))
            .unwrap();
        store
            .read(|db| {
                assert_eq!(db.messages().count(folder)?, 0);
                let state = &db.folders().sync_states(account)?[0];
                assert_eq!(
                    (state.uidvalidity, state.uidnext, state.highest_modseq),
                    (Some(9), None, None)
                );
                assert_eq!(db.folders().get(folder)?.unwrap().total_count, 0);
                let ops: Vec<(i64, String)> = db
                    .conn
                    .prepare("SELECT uidvalidity, status FROM pending_ops ORDER BY id")?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                assert_eq!(ops, [(7, "conflict".into()), (9, "pending".into())]);
                let draft: (Option<i64>, Option<i64>, Option<i64>) = db.conn.query_row(
                    "SELECT remote_folder_id, remote_uidvalidity, remote_uid FROM drafts",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                assert_eq!(draft, (None, None, None));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn deleting_a_folder_forgets_remote_draft_copies() {
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        store
            .write(|db| {
                db.folders()
                    .reconcile(account, &[remote("Drafts", "Drafts", None)])
            })
            .unwrap();
        let folder = store.read(|db| db.folders().list(account)).unwrap()[0].id;
        let insert_draft = format!(
            "INSERT INTO drafts (account_id, remote_folder_id, remote_uidvalidity, remote_uid)
             VALUES ({}, {}, 7, 1)",
            account.0, folder.0
        );
        store
            .write(|db| Ok(db.conn.execute(&insert_draft, [])?))
            .unwrap();

        // A bare delete would leave the UID behind; the CHECK refuses that.
        let bare = store.write(|db| Ok(db.conn.execute("DELETE FROM folders", [])?));
        assert!(bare.is_err());

        store
            .write(|db| db.folders().reconcile(account, &[]))
            .unwrap();
        let remaining: i64 = store
            .read(|db| {
                Ok(db.conn.query_row(
                    "SELECT count(*) FROM drafts WHERE remote_uid IS NULL AND remote_uidvalidity IS NULL",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(remaining, 1, "the draft survives, without its remote copy");
    }
}
