//! Schema migrations, counted in `PRAGMA user_version`: the number of migrations the
//! database has had, as `rusqlite_migration` counted them, so the two stay interchangeable.
//! Once released, a migration is never renumbered, reordered or removed.

use rusqlite::{Connection, OptionalExtension};

use crate::{Error, Result, Store};

/// Brings the database to the last of `migrations`: runs the ones it hasn't had, in order,
/// in one transaction with the new `user_version`, so a failure leaves it as it was. Refuses
/// a database that has had more migrations than there are (a newer catmail's): this one
/// doesn't know its schema.
///
/// `PRAGMA foreign_keys` is a no-op inside a transaction: a migration that rebuilds a table
/// can't turn foreign keys off, and what it may break is caught by the check before commit.
pub(crate) fn to_latest(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    let known = migrations.len();
    Store::transact(conn, |db| {
        let conn = db.conn;
        // A negative version is out of range for u32: an error, as it should be.
        let found: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let found = found as usize;
        if found > known {
            return Err(Error::Migration(format!(
                "the database is from a newer catmail (schema {found}, this one knows {known})"
            )));
        }
        if found == known {
            return Ok(());
        }
        for (i, sql) in migrations.iter().enumerate().skip(found) {
            conn.execute_batch(sql)
                .map_err(|e| Error::Migration(format!("migration {}: {e}", i + 1)))?;
        }
        if let Some(table) = conn
            .query_row("PRAGMA foreign_key_check", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
        {
            return Err(Error::Migration(format!(
                "migrating to {known} left a foreign key in {table} pointing at no row"
            )));
        }
        conn.pragma_update(None, "user_version", known as u32)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        conn
    }

    fn version(conn: &Connection) -> u32 {
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    fn tables(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn a_new_database_gets_every_migration() {
        let mut conn = conn();
        to_latest(&mut conn, &["CREATE TABLE a (x)", "CREATE TABLE b (y)"]).unwrap();
        assert_eq!(version(&conn), 2);
        assert_eq!(tables(&conn), ["a", "b"]);
    }

    #[test]
    fn a_migrated_database_is_left_alone() {
        let mut conn = conn();
        // Run again, CREATE TABLE would fail: the second call must not run it.
        to_latest(&mut conn, &["CREATE TABLE a (x)"]).unwrap();
        to_latest(&mut conn, &["CREATE TABLE a (x)"]).unwrap();
        assert_eq!(version(&conn), 1);
    }

    #[test]
    fn only_the_migrations_a_database_has_not_had_run() {
        // As `rusqlite_migration` left a database after its first migration.
        let mut conn = conn();
        conn.execute_batch("CREATE TABLE a (x); PRAGMA user_version = 1;")
            .unwrap();
        to_latest(&mut conn, &["CREATE TABLE a (x)", "CREATE TABLE b (y)"]).unwrap();
        assert_eq!(version(&conn), 2);
        assert_eq!(tables(&conn), ["a", "b"]);
    }

    #[test]
    fn a_newer_database_is_refused_and_left_as_it_is() {
        let mut conn = conn();
        conn.execute_batch("CREATE TABLE a (x); PRAGMA user_version = 3;")
            .unwrap();
        let err = to_latest(&mut conn, &["CREATE TABLE a (x)"]).unwrap_err();
        assert!(err.to_string().contains("newer catmail"), "{err}");
        assert_eq!(version(&conn), 3);
    }

    #[test]
    fn a_failed_migration_leaves_the_database_as_it_was() {
        let mut conn = conn();
        to_latest(&mut conn, &["CREATE TABLE a (x)"]).unwrap();
        let err = to_latest(
            &mut conn,
            &[
                "CREATE TABLE a (x)",
                "CREATE TABLE b (y)",
                "CREATE TABLE c (",
            ],
        )
        .unwrap_err();
        assert!(err.to_string().contains("migration 3"), "{err}");
        assert_eq!(version(&conn), 1);
        assert_eq!(tables(&conn), ["a"]);
    }

    #[test]
    fn a_migration_that_breaks_a_foreign_key_is_rolled_back() {
        // Foreign keys off, as around a table rebuild: nothing stops the orphan but the check.
        // (The bundled SQLite turns them on by default.)
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        let err = to_latest(
            &mut conn,
            &["CREATE TABLE p (id INTEGER PRIMARY KEY);
               CREATE TABLE c (p INTEGER REFERENCES p (id));
               INSERT INTO c VALUES (7);"],
        )
        .unwrap_err();
        assert!(err.to_string().contains("in c"), "{err}");
        assert_eq!(version(&conn), 0);
        assert!(tables(&conn).is_empty());
    }

    #[test]
    fn a_negative_version_is_refused() {
        let mut conn = conn();
        conn.pragma_update(None, "user_version", -1).unwrap();
        assert!(to_latest(&mut conn, &["CREATE TABLE a (x)"]).is_err());
    }
}
