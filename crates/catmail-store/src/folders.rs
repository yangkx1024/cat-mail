use std::collections::{HashMap, HashSet};

use catmail_types::{
    AccountId, Folder, FolderId, FolderKind, FolderRole, GmailLabel, special_folder,
};
use rusqlite::{Connection, OptionalExtension, Row, named_params, params};

use crate::Result;
use crate::columns::{parse_opt_text, parse_text};
use crate::drafts::Drafts;
use crate::ops::{PendingOps, Touched};

/// Accounts with an `\All` mailbox (Gmail), whose labels were once synced as folders,
/// a copy of a message in each, told apart only by Message-ID. Replay still looks for
/// such copies; to be removed with it (see `docs/architecture/labels.md`).
pub(crate) const LABEL_ACCOUNTS: &str = "SELECT account_id FROM folders WHERE role = 'all'";

const COLUMNS: &str = "id, account_id, name, delimiter, role, kind, listed, selectable,
    unread_count, total_count, last_synced_at";

/// A mailbox as listed by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFolder {
    pub remote_name: String,
    pub name: String,
    pub delimiter: Option<char>,
    pub role: Option<FolderRole>,
    pub kind: FolderKind,
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

/// Messages moved here locally and not yet on the server have no UID; sync leaves them
/// out of its counts.
const SYNC_STATE: &str = "
    SELECT id, remote_name, name, selectable, uidvalidity, uidnext, highest_modseq,
           total_count - (SELECT count(*) FROM messages
                          WHERE folder_id = folders.id AND uid IS NULL),
           (SELECT ifnull(max(uid), 0) FROM messages WHERE folder_id = folders.id)
    FROM folders";

/// The order sync goes through an account's locations: INBOX first, where new mail is,
/// then All Mail, then by id.
const LOCATION_ORDER: &str = "ORDER BY remote_name <> 'INBOX', role IS NOT 'all', id";

fn sync_state(r: &rusqlite::Row<'_>) -> rusqlite::Result<FolderSyncState> {
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
        let mut stmt = self.conn.prepare_cached(&sql)?;
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

    /// The account's folder for `role` (id and server name), if it has one: the one
    /// [`special_folder`] picks, as frontends do from the folder list.
    pub fn by_role(
        &self,
        account: AccountId,
        role: FolderRole,
    ) -> Result<Option<(FolderId, String)>> {
        let Some(id) = special_folder(&self.list(account)?, role).map(|f| f.id) else {
            return Ok(None);
        };
        Ok(self
            .conn
            .prepare_cached("SELECT remote_name FROM folders WHERE id = ?1")?
            .query_row([id.0], |r| r.get(0))
            .optional()?
            .map(|remote_name| (id, remote_name)))
    }

    /// The account's locations, which sync goes through (labels have no sync state), in
    /// [`LOCATION_ORDER`].
    pub fn sync_states(&self, account: AccountId) -> Result<Vec<FolderSyncState>> {
        let mut stmt = self.conn.prepare(&format!(
            "{SYNC_STATE} WHERE account_id = ?1 AND kind = 'location' {LOCATION_ORDER}"
        ))?;
        let rows = stmt.query_map([account.0], sync_state)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The account's locations that can hold messages, in [`LOCATION_ORDER`]: what a sync
    /// of every folder goes through, one by one.
    pub fn location_ids(&self, account: AccountId) -> Result<Vec<FolderId>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT id FROM folders
             WHERE account_id = ?1 AND kind = 'location' AND selectable {LOCATION_ORDER}"
        ))?;
        let rows = stmt.query_map([account.0], |r| Ok(FolderId(r.get(0)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn sync_state(&self, id: FolderId) -> Result<Option<FolderSyncState>> {
        Ok(self
            .conn
            .prepare_cached(&format!("{SYNC_STATE} WHERE id = ?1"))?
            .query_row([id.0], sync_state)
            .optional()?)
    }

    /// Makes the account's folders match `remote`: inserts new ones, updates changed
    /// attributes, and deletes folders the server no longer lists (with their messages).
    /// A label that messages still carry stays, unlisted: Gmail leaves the labels hidden
    /// in its settings out of LIST, not out of X-GM-LABELS.
    pub fn reconcile(&self, account: AccountId, remote: &[RemoteFolder]) -> Result<Reconciled> {
        let mut changed = false;
        let mut touched = Touched::new();
        // (id, kind, role, whether messages carry it as a label), by remote name.
        let stored: HashMap<String, (i64, FolderKind, Option<FolderRole>, bool)> = self
            .conn
            .prepare(
                "SELECT remote_name, id, kind, role,
                        kind = 'label'
                        AND EXISTS (SELECT 1 FROM message_labels WHERE folder_id = folders.id)
                 FROM folders WHERE account_id = ?1",
            )?
            .query_map([account.0], |r| {
                Ok((
                    r.get(0)?,
                    (
                        r.get(1)?,
                        parse_text(r, "kind")?,
                        parse_opt_text(r, "role")?,
                        r.get(4)?,
                    ),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        // A folder that is new, or whose kind or role changed.
        let changed_kind = |f: &RemoteFolder| {
            stored
                .get(&f.remote_name)
                .map(|&(_, kind, role, _)| (kind, role))
                != Some((f.kind, f.role))
        };
        // What a folder counts follows from its kind and role, which the count triggers
        // don't watch: a change of either (the account's mode changed, say), or a new
        // Starred, which shows messages already stored, is counted anew.
        let recount = remote.iter().any(|f| {
            changed_kind(f)
                && (stored.contains_key(&f.remote_name)
                    || (f.kind == FolderKind::Label && f.role == Some(FolderRole::Flagged)))
        });
        // X-GM-LABELS names a system label by its role, which only a listed mailbox has:
        // the labels of one hidden until now were dropped, and its showing again changes
        // no message's MODSEQ. Every location's flags and labels are fetched anew.
        let relabel = remote.iter().any(|f| {
            use FolderRole::{Drafts, Important, Inbox, Sent};
            f.kind == FolderKind::Label
                && matches!(f.role, Some(Inbox | Sent | Drafts | Important))
                && changed_kind(f)
        });
        let mut upsert = self.conn.prepare(
            "INSERT INTO folders (account_id, remote_name, name, delimiter, role, kind,
                                  selectable)
             VALUES (:account, :remote_name, :name, :delimiter, :role, :kind, :selectable)
             ON CONFLICT (account_id, remote_name) DO UPDATE SET
                 name = excluded.name, delimiter = excluded.delimiter, role = excluded.role,
                 kind = excluded.kind, listed = 1, selectable = excluded.selectable
             WHERE (name, delimiter, role, kind, listed, selectable)
                   IS NOT (excluded.name, excluded.delimiter, excluded.role, excluded.kind, 1,
                           excluded.selectable)",
        )?;
        for folder in remote {
            changed |= upsert.execute(named_params! {
                ":account": account.0,
                ":remote_name": folder.remote_name,
                ":name": folder.name,
                ":delimiter": folder.delimiter.map(String::from),
                ":role": folder.role.map(FolderRole::as_str),
                ":kind": folder.kind.as_str(),
                ":selectable": folder.selectable,
            })? > 0;
        }

        let listed: HashSet<&str> = remote.iter().map(|f| f.remote_name.as_str()).collect();
        for (remote_name, (id, _, _, carried)) in stored {
            if listed.contains(remote_name.as_str()) {
                continue;
            }
            if carried {
                changed |= self.conn.execute(
                    "UPDATE folders SET listed = 0 WHERE id = ?1 AND listed",
                    [id],
                )? > 0;
                continue;
            }
            touched.merge(PendingOps::new(self.conn).forget_folder(FolderId(id))?);
            Drafts::new(self.conn).end_generation(FolderId(id), None)?;
            self.conn
                .execute("DELETE FROM folders WHERE id = ?1", [id])?;
            changed = true;
        }

        // A mailbox deleted while it had children stays listed as a \Noselect node
        // (RFC 3501 DELETE). Sync never opens such a folder, so its old messages would
        // linger; the row stays for the hierarchy. A location that became a label holds
        // no messages of its own either.
        let emptied: Vec<i64> = self
            .conn
            .prepare(
                "SELECT id FROM folders
                 WHERE account_id = ?1 AND (selectable = 0 OR kind = 'label')
                   AND uidvalidity IS NOT NULL",
            )?
            .query_map([account.0], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in emptied {
            touched.merge(self.end_generation(FolderId(id), None)?);
            changed = true;
        }
        if relabel {
            self.conn.execute(
                "UPDATE folders SET highest_modseq = NULL
                 WHERE account_id = ?1 AND kind = 'location'",
                [account.0],
            )?;
        }
        if recount {
            // A label that became a location is no message's label.
            self.conn.execute(
                "DELETE FROM message_labels WHERE folder_id IN (
                     SELECT id FROM folders WHERE account_id = ?1 AND kind = 'location')",
                [account.0],
            )?;
            self.recount(account)?;
        }
        Ok(Reconciled { changed, touched })
    }

    /// Counts the account's folders anew, as the count triggers would have: needed when
    /// what a folder counts changes (see [`reconcile`](Self::reconcile)).
    fn recount(&self, account: AccountId) -> Result<()> {
        for (column, filter) in [("total_count", "1"), ("unread_count", "m.flags & 1 = 0")] {
            self.conn.execute(
                &format!(
                    "UPDATE folders SET {column} = CASE
                         WHEN kind = 'location' THEN
                             (SELECT count(*) FROM messages m
                              WHERE m.folder_id = folders.id AND {filter})
                         WHEN id IN (SELECT id FROM starred) THEN
                             (SELECT count(*) FROM messages m
                              WHERE m.account_id = folders.account_id AND m.flags & 2 <> 0
                                AND m.folder_id IN all_mail AND {filter})
                         ELSE
                             (SELECT count(*) FROM message_labels l
                              JOIN messages m ON m.id = l.message_id
                              WHERE l.folder_id = folders.id AND m.folder_id IN all_mail
                                AND {filter})
                     END
                     WHERE account_id = ?1"
                ),
                [account.0],
            )?;
        }
        Ok(())
    }

    /// The label folder for a label X-GM-LABELS names. A user label the server doesn't
    /// list (hidden in Gmail's settings) gets an unlisted folder; a system label without
    /// a folder has none, nor does Starred: it is `\Flagged`, not a label row.
    pub fn label(&self, account: AccountId, label: &GmailLabel) -> Result<Option<FolderId>> {
        let mut by_role = self.conn.prepare_cached(
            "SELECT id FROM folders WHERE account_id = ?1 AND kind = 'label' AND role = ?2
             ORDER BY id LIMIT 1",
        )?;
        let mut by_name = self.conn.prepare_cached(
            "SELECT id, kind = 'label' FROM folders WHERE account_id = ?1 AND remote_name = ?2",
        )?;
        let mut unlisted = self.conn.prepare_cached(
            "INSERT INTO folders (account_id, remote_name, name, delimiter, kind, listed)
             VALUES (?1, ?2, ?3,
                     (SELECT delimiter FROM folders
                      WHERE account_id = ?1 AND delimiter IS NOT NULL LIMIT 1),
                     'label', 0)
             RETURNING id",
        )?;
        let id = match label {
            GmailLabel::System(FolderRole::Flagged) => None,
            GmailLabel::System(role) => by_role
                .query_row(params![account.0, role.as_str()], |r| r.get(0))
                .optional()?,
            GmailLabel::User { remote_name, name } => {
                let found: Option<(i64, bool)> = by_name
                    .query_row(params![account.0, remote_name], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .optional()?;
                match found {
                    Some((id, is_label)) => is_label.then_some(id),
                    None => Some(
                        unlisted.query_row(params![account.0, remote_name, name], |r| r.get(0))?,
                    ),
                }
            }
        };
        Ok(id.map(FolderId))
    }

    /// Ends the UIDVALIDITY generation of every location of the account, as a new
    /// UIDVALIDITY would: its mode changed, so what a message row is changed with it, and
    /// sync fetches them anew. Returns what [`reset_uidvalidity`](Self::reset_uidvalidity)
    /// does.
    pub fn end_generations(&self, account: AccountId) -> Result<Touched> {
        let mut touched = Touched::new();
        for folder in self.sync_states(account)? {
            if folder.uidvalidity.is_some() {
                touched.merge(self.end_generation(folder.id, None)?);
            }
        }
        Ok(touched)
    }

    /// Starts a new UIDVALIDITY generation (or the first one): every message row with a
    /// UID and the cursor of the old generation are dropped, pending operations against old UIDs
    /// become conflicts, and draft copies addressed by old UIDs are forgotten.
    /// Returns the folders whose messages or queued changes this changed, besides
    /// `folder`'s own messages: local moves out of it are undone.
    pub fn reset_uidvalidity(&self, folder: FolderId, uidvalidity: u32) -> Result<Touched> {
        self.end_generation(folder, Some(uidvalidity))
    }

    /// Drops everything tied to the folder's UIDs except what belongs to `next`, the
    /// generation that replaces it (`None`: the folder no longer holds messages at all).
    /// Messages moved here locally stay: they have no UID here yet, and their moves name
    /// where the server has them now.
    fn end_generation(&self, folder: FolderId, next: Option<u32>) -> Result<Touched> {
        self.conn.execute(
            "DELETE FROM messages WHERE folder_id = ?1 AND uid IS NOT NULL",
            [folder.0],
        )?;
        let touched = PendingOps::new(self.conn).end_generation(folder, next)?;
        Drafts::new(self.conn).end_generation(folder, next)?;
        self.conn.execute(
            "UPDATE folders SET uidvalidity = ?2, uidnext = NULL, highest_modseq = NULL
             WHERE id = ?1",
            params![folder.0, next],
        )?;
        Ok(touched)
    }

    /// Records the cursor of a sync that stored every message. It counts as a completed
    /// sync (`last_synced_at`) only with `flags_refreshed`: a sync that left the flags of
    /// stored messages alone may be behind on them.
    pub fn set_synced(
        &self,
        folder: FolderId,
        uidnext: Option<u32>,
        highest_modseq: Option<u64>,
        flags_refreshed: bool,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE folders SET uidnext = ?2, highest_modseq = ?3,
                last_synced_at = CASE WHEN ?4 THEN unixepoch() ELSE last_synced_at END
             WHERE id = ?1",
            params![
                folder.0,
                uidnext,
                highest_modseq.map(|m| m as i64),
                flags_refreshed
            ],
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
        kind: parse_text(row, "kind")?,
        listed: row.get("listed")?,
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
    use crate::test_util;

    fn remote(remote_name: &str, name: &str, role: Option<FolderRole>) -> RemoteFolder {
        RemoteFolder {
            remote_name: remote_name.into(),
            name: name.into(),
            delimiter: Some('/'),
            role,
            kind: FolderKind::Location,
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
        let store = test_util::store();
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
        let store = test_util::store();
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
                db.folders().set_synced(folder, Some(3), Some(10), true)?;
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
        let store = test_util::store();
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
    fn a_sync_that_left_the_flags_alone_is_not_completed() {
        let store = test_util::store();
        let account = test_util::account(&store, "a@example.com");
        store
            .write(|db| {
                db.folders()
                    .reconcile(account, &[remote("INBOX", "INBOX", None)])
            })
            .unwrap();
        let folder = store.read(|db| db.folders().list(account)).unwrap()[0].id;
        let state = || store.read(|db| db.folders().get(folder)).unwrap().unwrap();

        store
            .write(|db| db.folders().set_synced(folder, Some(3), None, false))
            .unwrap();
        assert_eq!(state().last_synced_at, None);
        assert_eq!(
            store
                .read(|db| db.folders().sync_state(folder))
                .unwrap()
                .unwrap()
                .uidnext,
            Some(3)
        );
        store
            .write(|db| db.folders().set_synced(folder, Some(4), None, true))
            .unwrap();
        let synced = state().last_synced_at;
        assert!(synced.is_some());
        store
            .write(|db| db.folders().set_synced(folder, Some(5), None, false))
            .unwrap();
        assert_eq!(state().last_synced_at, synced);
    }

    #[test]
    fn uidvalidity_reset_drops_the_old_generation() {
        let store = test_util::store();
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
                db.folders().set_synced(folder, Some(3), Some(10), true)
            })
            .unwrap();
        store
            .write(|db| {
                db.conn.execute_batch(&format!(
                    "INSERT INTO pending_ops (account_id, folder_id, uidvalidity, uid, kind)
                     VALUES ({a}, {f}, 7, 1, 'flags'), ({a}, {f}, 9, 1, 'flags');
                     INSERT INTO drafts (account_id, identity_id, remote_folder_id,
                                         remote_uidvalidity, remote_uid, remote_message_id,
                                         remote_revision)
                     SELECT {a}, id, {f}, 7, 1, 'd@x', 0 FROM identities WHERE account_id = {a};",
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
                let draft: (Option<i64>, Option<i64>, Option<i64>, Option<i64>) =
                    db.conn.query_row(
                        "SELECT remote_folder_id, remote_uidvalidity, remote_uid, remote_revision
                         FROM drafts",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )?;
                assert_eq!(draft, (None, None, None, None));
                let found_again: (i64, String, i64) = db.conn.query_row(
                    "SELECT folder_id, message_id_hdr, revision FROM draft_uploads",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                assert_eq!(found_again, (folder.0, "d@x".into(), 0), "looked for again");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn deleting_a_folder_forgets_remote_draft_copies() {
        let store = test_util::store();
        let account = test_util::account(&store, "a@example.com");
        store
            .write(|db| {
                db.folders()
                    .reconcile(account, &[remote("Drafts", "Drafts", None)])
            })
            .unwrap();
        let folder = store.read(|db| db.folders().list(account)).unwrap()[0].id;
        let insert_draft = format!(
            "INSERT INTO drafts (account_id, identity_id, remote_folder_id, remote_uidvalidity,
                                 remote_uid, remote_message_id, remote_revision)
             SELECT {a}, id, {f}, 7, 1, 'd@x', 0 FROM identities WHERE account_id = {a}",
            a = account.0,
            f = folder.0
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

    #[test]
    fn labels_stay_while_carried_and_kinds_keep_their_messages_apart() {
        let store = test_util::store();
        let account = test_util::account(&store, "a@gmail.com");
        let label = |name: &str| RemoteFolder {
            kind: FolderKind::Label,
            ..remote(name, name, None)
        };
        let all_mail = remote("[Gmail]/All Mail", "All Mail", Some(FolderRole::All));
        let listed = [all_mail.clone(), label("Work"), label("Old")];
        store
            .write(|db| db.folders().reconcile(account, &listed))
            .unwrap();
        let id = |name: &str| {
            store
                .read(|db| db.folders().find_by_name(account, name))
                .unwrap()
                .map(|f| (f.id, f.kind, f.listed))
        };
        let (all, ..) = id("All Mail").unwrap();
        let work = GmailLabel::user("Work", "Work");
        let mut labelled = message(1);
        labelled.state.labels = Some(vec![work.clone(), work]);
        store
            .write(|db| {
                db.folders().reset_uidvalidity(all, 7)?;
                db.messages().upsert(account, all, &[labelled])
            })
            .unwrap();

        // Hidden in Gmail's settings: Work still labels a message, Old nothing.
        assert!(
            store
                .write(|db| db
                    .folders()
                    .reconcile(account, std::slice::from_ref(&all_mail)))
                .unwrap()
                .changed
        );
        let (work_id, _, work_listed) = id("Work").unwrap();
        assert!(!work_listed);
        assert_eq!(id("Old"), None);
        assert_eq!(
            store
                .read(|db| db.folders().get(work_id))
                .unwrap()
                .unwrap()
                .total_count,
            1
        );
        assert!(
            !store
                .write(|db| db
                    .folders()
                    .reconcile(account, std::slice::from_ref(&all_mail)))
                .unwrap()
                .changed
        );
        store
            .write(|db| {
                db.folders()
                    .reconcile(account, &[all_mail.clone(), label("Work")])
            })
            .unwrap();
        assert_eq!(id("Work"), Some((work_id, FolderKind::Label, true)));

        // A label that became a location labels nothing; a location that became a label
        // holds no messages, nor a generation.
        let work_location = remote("Work", "Work", None);
        let all_label = RemoteFolder {
            kind: FolderKind::Label,
            ..all_mail
        };
        store
            .write(|db| db.folders().reconcile(account, &[all_label, work_location]))
            .unwrap();
        store
            .read(|db| {
                assert_eq!(db.folders().get(work_id)?.unwrap().total_count, 0);
                assert_eq!(db.messages().count(all)?, 0);
                let state = db.folders().sync_state(all)?.unwrap();
                assert_eq!(state.uidvalidity, None);
                let labels: i64 =
                    db.conn
                        .query_row("SELECT count(*) FROM message_labels", [], |r| r.get(0))?;
                assert_eq!(labels, 0);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn labels_are_found_by_role_or_name() {
        let store = test_util::store();
        let account = test_util::account(&store, "a@gmail.com");
        let label = |name: &str, role| RemoteFolder {
            kind: FolderKind::Label,
            ..remote(name, name, role)
        };
        store
            .write(|db| {
                db.folders().reconcile(
                    account,
                    &[
                        label("INBOX", Some(FolderRole::Inbox)),
                        label("[Gmail]/Wichtig", Some(FolderRole::Important)),
                        label("[Gmail]/Starred", Some(FolderRole::Flagged)),
                        remote("[Gmail]/Trash", "Trash", Some(FolderRole::Trash)),
                        label("&ZeVnLIqe-", None),
                    ],
                )
            })
            .unwrap();
        let found: Vec<FolderId> = store
            .write(|db| {
                let labels = [
                    GmailLabel::System(FolderRole::Important),
                    GmailLabel::System(FolderRole::Inbox),
                    GmailLabel::System(FolderRole::Flagged),
                    GmailLabel::System(FolderRole::Sent),
                    GmailLabel::user("&ZeVnLIqe-", "日本語"),
                    GmailLabel::user("[Gmail]/Trash", "Trash"),
                    GmailLabel::user("Hidden/Deep", "Hidden/Deep"),
                ];
                let mut found = Vec::new();
                for label in &labels {
                    found.extend(db.folders().label(account, label)?);
                }
                Ok(found)
            })
            .unwrap();
        let names: Vec<_> = store
            .read(|db| {
                found
                    .iter()
                    .map(|&id| Ok(db.folders().get(id)?.unwrap()))
                    .collect::<Result<Vec<_>>>()
            })
            .unwrap()
            .into_iter()
            .map(|f| (f.name, f.listed))
            .collect();
        assert_eq!(
            names,
            [
                ("[Gmail]/Wichtig".into(), true),
                ("INBOX".into(), true),
                ("&ZeVnLIqe-".into(), true),
                ("Hidden/Deep".into(), false),
            ]
        );
    }
}
