//! Local persistence for cat-mail: the SQLite database and the on-disk blob store.
//!
//! Everything here is synchronous. `catmail-core` calls into it from blocking tasks.

mod accounts;
mod attachments;
mod blob;
mod columns;
mod drafts;
mod error;
mod folders;
mod fts;
mod messages;
mod ops;
mod outbox;
mod private;

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use catmail_types::BlobHash;
use rusqlite::{Connection, TransactionBehavior};
use rusqlite_migration::{M, Migrations};

pub use accounts::Accounts;
pub use attachments::{AttachmentSource, Attachments, NewAttachment};
pub use blob::BlobStore;
pub use drafts::{DraftReply, DraftSnapshot, DraftUpload, Drafts, NewDraftAttachment};
pub use error::{Error, Result};
pub use folders::{FolderSyncState, Folders, Reconciled, RemoteFolder};
pub use messages::{MessageLocation, Messages, NewBody, NewMessage};
pub use ops::{CopyCheckpoint, Moved, PendingOps, ReplayFolder, ReplayKind, ReplayOp, Touched};
pub use outbox::{NewOutboxEntry, Outbox, OutboxJob};
pub use private::create_private_dir;

/// Every `*_blob_hash` column (a test checks none is missing).
const BLOB_REFERENCES: &str = "
    SELECT raw_blob_hash AS hash FROM message_bodies WHERE raw_blob_hash IS NOT NULL
    UNION ALL SELECT blob_hash FROM attachments WHERE blob_hash IS NOT NULL
    UNION ALL SELECT blob_hash FROM draft_attachments
    UNION ALL SELECT raw_blob_hash FROM outbox";

const MIGRATIONS: &[M<'static>] = &[M::up(include_str!("../migrations/0001_init.sql"))];

/// Handle to the SQLite database. Cheap to clone.
///
/// All access goes through [`Store::read`] or [`Store::write`], which hand the closure a
/// [`Db`] whose repositories share one transaction. Multi-table changes (a local edit
/// plus its `pending_ops` row, synced messages plus the folder cursor) are therefore
/// committed atomically or not at all.
///
/// A single connection behind a mutex for now; split into one writer plus a reader pool
/// once sync and UI reads start contending. The closure must not call back into the
/// same `Store`, or it deadlocks.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

/// Repositories bound to one connection and transaction.
pub struct Db<'c> {
    conn: &'c Connection,
}

impl<'c> Db<'c> {
    pub fn accounts(&self) -> Accounts<'c> {
        Accounts::new(self.conn)
    }

    pub fn folders(&self) -> Folders<'c> {
        Folders::new(self.conn)
    }

    pub fn messages(&self) -> Messages<'c> {
        Messages::new(self.conn)
    }

    pub fn attachments(&self) -> Attachments<'c> {
        Attachments::new(self.conn)
    }

    pub fn drafts(&self) -> Drafts<'c> {
        Drafts::new(self.conn)
    }

    pub fn outbox(&self) -> Outbox<'c> {
        Outbox::new(self.conn)
    }

    pub fn ops(&self) -> PendingOps<'c> {
        PendingOps::new(self.conn)
    }

    /// Every blob some row refers to, for [`BlobStore::collect_garbage`]. A new
    /// `*_blob_hash` column must be added here, or GC deletes what it points to.
    pub fn blob_references(&self) -> Result<HashSet<BlobHash>> {
        let mut stmt = self.conn.prepare(BLOB_REFERENCES)?;
        let rows = stmt.query_map([], |r| columns::parse_text(r, "hash"))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

impl Store {
    /// Opens (creating if needed) the database at `path` and migrates it to the latest
    /// schema. The database and its WAL files are made readable by the user alone.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        private::new_file(path)?;
        // SQLite gives the WAL files it creates the database's mode; ones left from an
        // earlier session keep theirs.
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_owned();
            file.push(suffix);
            private::make_private(Path::new(&file))?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // Commits survive the app crashing, not a power cut; see `write_durable`.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Zeroes deleted rows and freed pages, so a removed account's or message's text
        // doesn't linger in the file. Costs writing the freed pages (through the WAL),
        // also those FTS merges free.
        conn.pragma_update(None, "secure_delete", true)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Migrations::from_slice(MIGRATIONS).to_latest(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs `f` against a consistent snapshot. Nothing is written.
    pub fn read<T>(&self, f: impl FnOnce(&Db<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        // Dropped without commit: a read-only transaction just ends.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        f(&Db { conn: &tx })
    }

    /// Runs `f` in one write transaction, committed only if `f` returns `Ok`.
    ///
    /// Notify observers (events) after this returns, never from inside `f`: the change
    /// isn't visible until the commit.
    pub fn write<T>(&self, f: impl FnOnce(&Db<'_>) -> Result<T>) -> Result<T> {
        Self::transact(&mut self.conn(), f)
    }

    /// Like [`write`](Self::write), but the commit is on disk when this returns, even if
    /// the power fails or the OS crashes next; a plain write may be lost to those (not
    /// to the app crashing). Earlier commits are synced along with it. For commits that
    /// something irreversible follows, such as the end of a message being sent.
    pub fn write_durable<T>(&self, f: impl FnOnce(&Db<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        conn.pragma_update(None, "synchronous", "FULL")?;
        let out = Self::transact(&mut conn, f);
        // Failing to switch back only costs speed: later commits get synced too.
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        out
    }

    fn transact<T>(conn: &mut Connection, f: impl FnOnce(&Db<'_>) -> Result<T>) -> Result<T> {
        // IMMEDIATE takes the write lock up front instead of failing on upgrade later.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&Db { conn: &tx })?;
        tx.commit()?;
        Ok(out)
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock can't leave the connection itself inconsistent:
        // any open transaction is rolled back when its guard drops.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use catmail_types::{AccountId, AuthKind, NewAccount, Security, ServerConfig};

    use crate::Store;

    pub(crate) fn account(store: &Store, email: &str) -> AccountId {
        let server = ServerConfig {
            host: "localhost".into(),
            port: 993,
            security: Security::Tls,
        };
        let new = NewAccount {
            email: email.into(),
            display_name: None,
            username: email.into(),
            imap: server.clone(),
            smtp: server,
            auth: AuthKind::Password,
        };
        store
            .write(|db| db.accounts().insert(&new, &format!("key:{email}")))
            .unwrap()
            .id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO accounts (id, email, username, imap_host, imap_port, imap_security,
                                   smtp_host, smtp_port, smtp_security, auth_kind, secret_key)
             VALUES (1, 'a@example.com', 'a', 'imap', 993, 'tls', 'smtp', 465, 'tls', 'password', 'ka'),
                    (2, 'b@example.com', 'b', 'imap', 993, 'tls', 'smtp', 465, 'tls', 'password', 'kb');
             INSERT INTO folders (id, account_id, remote_name, name, uidvalidity)
             VALUES (20, 1, 'INBOX', 'INBOX', 7),
                    (21, 2, 'INBOX', 'INBOX', 7);",
        )
        .unwrap();
    }

    fn is_constraint_violation(err: &rusqlite::Error) -> bool {
        err.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation)
    }

    #[cfg(unix)]
    #[test]
    fn database_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catmail.db");
        let wal = dir.path().join("catmail.db-wal");
        // As left by a version that didn't restrict them.
        for file in [&path, &wal] {
            std::fs::write(file, b"").unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let store = Store::open(&path).unwrap();
        store
            .write(|db| {
                seed(db.conn);
                Ok(())
            })
            .unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let file = dir.path().join(format!("catmail.db{suffix}"));
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{suffix}: {mode:o}");
        }
    }

    #[test]
    fn deleted_content_is_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("catmail.db")).unwrap();
        let secure_delete: i64 = store
            .conn()
            .pragma_query_value(None, "secure_delete", |r| r.get(0))
            .unwrap();
        assert_eq!(secure_delete, 1);
    }

    #[test]
    fn durable_writes_are_synced() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("catmail.db")).unwrap();
        let synchronous = |conn: &Connection| -> i64 {
            conn.pragma_query_value(None, "synchronous", |r| r.get(0))
                .unwrap()
        };
        let journal: String = store
            .conn()
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        // NORMAL is 1, FULL 2.
        assert_eq!(store.write(|db| Ok(synchronous(db.conn))).unwrap(), 1);
        assert_eq!(
            store.write_durable(|db| Ok(synchronous(db.conn))).unwrap(),
            2
        );
        assert_eq!(synchronous(&store.conn()), 1, "only for that commit");
    }

    #[test]
    fn blob_references_cover_every_blob_column() {
        // GC deletes any blob whose column is missing from the query.
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn();
        let columns: Vec<(String, String)> = conn
            .prepare(
                "SELECT m.name, p.name FROM sqlite_master m, pragma_table_info(m.name) p
                 WHERE m.type = 'table' AND p.name LIKE '%blob_hash'",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(columns.len(), 4, "{columns:?}");
        let sql = BLOB_REFERENCES
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for (table, column) in columns {
            assert!(
                sql.contains(&format!("SELECT {column} FROM {table}"))
                    || sql.contains(&format!("SELECT {column} AS hash FROM {table}")),
                "{table}.{column} is not in BLOB_REFERENCES"
            );
        }
    }

    #[test]
    fn migrations_are_valid() {
        Migrations::from_slice(MIGRATIONS).validate().unwrap();
    }

    #[test]
    fn deleting_account_cascades_to_messages_and_fts() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn();
        seed(&conn);
        conn.execute_batch(
            "INSERT INTO messages (id, account_id, folder_id, uid, subject)
             VALUES (10, 1, 20, 1, 'hello world');
             INSERT INTO messages_fts (rowid, subject) VALUES (10, 'hello world');",
        )
        .unwrap();
        let hits = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'hello'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(hits(&conn), 1);

        conn.execute("DELETE FROM accounts WHERE id = 1", [])
            .unwrap();
        assert_eq!(hits(&conn), 0);
        let messages: i64 = conn
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(messages, 0);
    }

    #[test]
    fn rows_cannot_reference_another_accounts_folder() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn();
        seed(&conn);
        // Folder 21 belongs to account 2.
        for sql in [
            "INSERT INTO messages (account_id, folder_id, uid) VALUES (1, 21, 1)",
            "INSERT INTO pending_ops (account_id, folder_id, uidvalidity, uid, kind)
             VALUES (1, 21, 7, 1, 'delete')",
            "INSERT INTO pending_ops (account_id, folder_id, uidvalidity, uid, kind,
                                      target_folder_id)
             VALUES (1, 20, 7, 1, 'move', 21)",
        ] {
            let err = conn.execute(sql, []).unwrap_err();
            assert!(is_constraint_violation(&err), "{sql}: {err}");
        }
        conn.execute(
            "INSERT INTO messages (account_id, folder_id, uid) VALUES (1, 20, 1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn same_uid_in_two_folders_has_independent_flags() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn();
        seed(&conn);
        conn.execute_batch(
            "INSERT INTO folders (id, account_id, remote_name, name) VALUES (22, 1, 'Archive', 'Archive');
             INSERT INTO messages (account_id, folder_id, uid, flags) VALUES (1, 20, 5, 1);
             INSERT INTO messages (account_id, folder_id, uid, flags) VALUES (1, 22, 5, 0);",
        )
        .unwrap();
        let err = conn
            .execute(
                "INSERT INTO messages (account_id, folder_id, uid) VALUES (1, 20, 5)",
                [],
            )
            .unwrap_err();
        assert!(is_constraint_violation(&err), "{err}");
    }

    #[test]
    fn write_rolls_back_on_error() {
        let store = Store::open_in_memory().unwrap();
        let result: Result<()> = store.write(|db| {
            db.conn.execute(
                "INSERT INTO accounts (email, username, imap_host, imap_port, imap_security,
                                       smtp_host, smtp_port, smtp_security, auth_kind, secret_key)
                 VALUES ('a@example.com', 'a', 'i', 993, 'tls', 's', 465, 'tls', 'password', 'k')",
                [],
            )?;
            Err(Error::AlreadyExists(
                "simulated failure after the first write".into(),
            ))
        });
        assert!(result.is_err());
        assert!(store.read(|db| db.accounts().list()).unwrap().is_empty());
    }
}
