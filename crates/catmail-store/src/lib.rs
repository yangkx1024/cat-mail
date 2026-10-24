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
mod identities;
mod messages;
mod ops;
mod outbox;
mod private;

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use catmail_types::BlobHash;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use rusqlite_migration::{M, Migrations};

pub use accounts::Accounts;
pub use attachments::{AttachmentSource, Attachments, NewAttachment};
pub use blob::BlobStore;
pub use drafts::{DraftReply, DraftSnapshot, DraftUpload, Drafts, NewDraftAttachment};
pub use error::{Error, Result};
pub use folders::{FolderSyncState, Folders, Reconciled, RemoteFolder};
pub use identities::{DeletedIdentity, Identities, Sending};
pub use messages::{MessageLocation, Messages, NewBody, NewMessage};
pub use ops::{
    CopyCheckpoint, LabelCopies, Moved, PendingOps, ReplayFolder, ReplayKind, ReplayOp, Touched,
};
pub use outbox::{NewOutboxEntry, Outbox, OutboxJob};
pub use private::create_private_dir;

/// Every `*_blob_hash` column (a test checks none is missing).
const BLOB_REFERENCES: &str = "
    SELECT raw_blob_hash AS hash FROM message_bodies WHERE raw_blob_hash IS NOT NULL
    UNION ALL SELECT blob_hash FROM attachments WHERE blob_hash IS NOT NULL
    UNION ALL SELECT blob_hash FROM draft_attachments
    UNION ALL SELECT raw_blob_hash FROM outbox";

const MIGRATIONS: &[M<'static>] = &[M::up(include_str!("../migrations/0001_init.sql"))];

/// Read connections, opened with the store; further concurrent reads wait for one to
/// come back.
const READERS: usize = 4;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Handle to the SQLite database. Cheap to clone.
///
/// All access goes through [`Store::read`] or [`Store::write`], which hand the closure a
/// [`Db`] whose repositories share one transaction. Multi-table changes (a local edit
/// plus its `pending_ops` row, synced messages plus the folder cursor) are therefore
/// committed atomically or not at all.
///
/// One write connection behind a mutex plus a small pool of read-only connections, so
/// reads (the message list) don't queue behind a sync batch's write transaction: WAL
/// gives each read the last committed snapshot meanwhile. The closure must still not
/// call back into the same `Store`: a write inside a write deadlocks, a read inside a
/// write doesn't see the uncommitted change, and nested reads can exhaust the pool.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    // Declared first so readers close first: the last connection to close checkpoints
    // the WAL, which a read-only one can't.
    readers: Readers,
    writer: Mutex<Connection>,
}

struct Readers {
    idle: Mutex<Vec<Connection>>,
    returned: Condvar,
}

impl Readers {
    fn open(path: &Path) -> Result<Self> {
        let open = || -> Result<Connection> {
            // Read-only, so a write slipped into a `read` closure fails instead of
            // taking the write lock behind the writer's back.
            let conn = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.busy_timeout(BUSY_TIMEOUT)?;
            Ok(conn)
        };
        Ok(Self {
            idle: Mutex::new((0..READERS).map(|_| open()).collect::<Result<_>>()?),
            returned: Condvar::new(),
        })
    }

    /// Lends an idle connection, waiting for one to be returned if all are lent out.
    fn get(&self) -> Reader<'_> {
        let mut idle = self
            .returned
            .wait_while(lock(&self.idle), |idle| idle.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
        Reader {
            readers: self,
            conn: idle.pop(),
        }
    }
}

/// A pooled read connection, returned to the pool when dropped.
struct Reader<'r> {
    readers: &'r Readers,
    /// Only `None` once dropped.
    conn: Option<Connection>,
}

impl Reader<'_> {
    fn conn(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("lent out while idle")
    }
}

impl Drop for Reader<'_> {
    fn drop(&mut self) {
        // Any transaction on it ended with its guard, also on a panic.
        if let Some(conn) = self.conn.take() {
            lock(&self.readers.idle).push(conn);
            self.readers.returned.notify_one();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding the lock can't leave a connection inconsistent: any open
    // transaction is rolled back when its guard drops.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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

    pub fn identities(&self) -> Identities<'c> {
        Identities::new(self.conn)
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
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // Commits survive the app crashing, not a power cut; see `write_durable`.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Zeroes deleted rows and freed pages, so a removed account's or message's text
        // doesn't linger in the file. Costs writing the freed pages (through the WAL),
        // also those FTS merges free.
        conn.pragma_update(None, "secure_delete", true)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        Migrations::from_slice(MIGRATIONS).to_latest(&mut conn)?;
        Ok(Self {
            inner: Arc::new(Inner {
                // After the migration, so no read sees an older schema.
                readers: Readers::open(path)?,
                writer: Mutex::new(conn),
            }),
        })
    }

    /// Runs `f` against a consistent snapshot: every query in it sees the database as
    /// of its first query, so at least everything committed before this call. Nothing
    /// is written. Doesn't wait for a write in progress.
    pub fn read<T>(&self, f: impl FnOnce(&Db<'_>) -> Result<T>) -> Result<T> {
        let mut reader = self.inner.readers.get();
        // Dropped without commit: a read-only transaction just ends.
        let tx = reader
            .conn()
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
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

    /// The write connection.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        lock(&self.inner.writer)
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use catmail_types::{AccountId, NewAccount, Security, ServerConfig};
    use tempfile::TempDir;

    use crate::Store;

    /// A store in a temporary directory, deleted along with it. On a file, not in
    /// memory: other connections to an in-memory database would open another, empty
    /// one, so reads couldn't use the pool.
    pub(crate) struct TestStore {
        store: Store,
        _dir: TempDir,
    }

    impl std::ops::Deref for TestStore {
        type Target = Store;

        fn deref(&self) -> &Store {
            &self.store
        }
    }

    pub(crate) fn store() -> TestStore {
        let dir = tempfile::tempdir().unwrap();
        TestStore {
            store: Store::open(&dir.path().join("catmail.db")).unwrap(),
            _dir: dir,
        }
    }

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
                                   secret_key)
             VALUES (1, 'a@example.com', 'a', 'imap', 993, 'tls', 'ka'),
                    (2, 'b@example.com', 'b', 'imap', 993, 'tls', 'kb');
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
        let store = test_util::store();
        let secure_delete: i64 = store
            .conn()
            .pragma_query_value(None, "secure_delete", |r| r.get(0))
            .unwrap();
        assert_eq!(secure_delete, 1);
    }

    #[test]
    fn durable_writes_are_synced() {
        let store = test_util::store();
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
        let store = test_util::store();
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
        let store = test_util::store();
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
        let store = test_util::store();
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
        let store = test_util::store();
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
    fn reads_do_not_wait_for_a_write() {
        use std::sync::mpsc;
        let store = test_util::store();
        test_util::account(&store, "a@example.com");
        // The read runs inside the write only to hold the write open meanwhile; on
        // another thread, so a read stuck behind it fails the test instead of hanging.
        let (done, read) = mpsc::channel();
        let writer = std::thread::spawn({
            let store = store.clone();
            move || {
                store.write(|db| {
                    db.conn.execute("DELETE FROM accounts", [])?;
                    let seen = store.read(|db| db.accounts().list())?.len();
                    done.send(seen).unwrap();
                    Ok(())
                })
            }
        });
        let seen = read
            .recv_timeout(Duration::from_secs(5))
            .expect("the read waited for the write");
        assert_eq!(seen, 1, "the last committed snapshot");
        writer.join().unwrap().unwrap();
        assert!(store.read(|db| db.accounts().list()).unwrap().is_empty());
    }

    #[test]
    fn reads_cannot_write() {
        let store = test_util::store();
        let result = store.read(|db| {
            db.conn
                .execute("DELETE FROM accounts", [])
                .map_err(Error::from)
        });
        let Err(Error::Sqlite(err)) = result else {
            panic!("{result:?}");
        };
        assert_eq!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ReadOnly),
            "{err}"
        );
    }

    #[test]
    fn more_concurrent_reads_than_connections() {
        let store = test_util::store();
        test_util::account(&store, "a@example.com");
        let threads = 3 * READERS;
        let start = Arc::new(std::sync::Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|i| {
                let store = store.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    for j in 0..20 {
                        // Reads during commits mustn't fail with SQLITE_BUSY either.
                        if i == 0 {
                            test_util::account(&store, &format!("{j}@example.com"));
                        }
                        assert!(!store.read(|db| db.accounts().list()).unwrap().is_empty());
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(
            lock(&store.inner.readers.idle).len(),
            READERS,
            "all returned"
        );
    }

    #[test]
    fn write_rolls_back_on_error() {
        let store = test_util::store();
        let result: Result<()> = store.write(|db| {
            db.conn.execute(
                "INSERT INTO accounts (email, username, imap_host, imap_port, imap_security,
                                       smtp_host, smtp_port, smtp_security, secret_key)
                 VALUES ('a@example.com', 'a', 'i', 993, 'tls', 's', 465, 'tls', 'k')",
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
