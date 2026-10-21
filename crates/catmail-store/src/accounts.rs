use catmail_types::{Account, AccountId, NewAccount, ServerConfig};
use rusqlite::{Connection, OptionalExtension, Row, named_params};

use crate::columns::parse_text;
use crate::error::is_unique_violation;
use crate::{Error, Result};

const COLUMNS: &str = "id, email, display_name, username,
    imap_host, imap_port, imap_security,
    smtp_host, smtp_port, smtp_security,
    auth_kind, created_at";

pub struct Accounts<'c> {
    conn: &'c Connection,
}

impl<'c> Accounts<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// `secret_key` names the keyring entry that already holds the account's password.
    pub fn insert(&self, new: &NewAccount, secret_key: &str) -> Result<Account> {
        let conn = self.conn;
        let sql = format!(
            "INSERT INTO accounts (email, display_name, username,
                                   imap_host, imap_port, imap_security,
                                   smtp_host, smtp_port, smtp_security, auth_kind, secret_key)
             VALUES (:email, :display_name, :username,
                     :imap_host, :imap_port, :imap_security,
                     :smtp_host, :smtp_port, :smtp_security, :auth_kind, :secret_key)
             RETURNING {COLUMNS}"
        );
        conn.query_row(
            &sql,
            named_params! {
                ":email": new.email,
                ":display_name": new.display_name,
                ":username": new.username,
                ":imap_host": new.imap.host,
                ":imap_port": new.imap.port,
                ":imap_security": new.imap.security.as_str(),
                ":smtp_host": new.smtp.host,
                ":smtp_port": new.smtp.port,
                ":smtp_security": new.smtp.security.as_str(),
                ":auth_kind": new.auth.as_str(),
                ":secret_key": secret_key,
            },
            from_row,
        )
        .map_err(|e| {
            if is_unique_violation(&e) {
                Error::AlreadyExists(format!("account {}", new.email))
            } else {
                e.into()
            }
        })
    }

    pub fn get(&self, id: AccountId) -> Result<Option<Account>> {
        let conn = self.conn;
        let sql = format!("SELECT {COLUMNS} FROM accounts WHERE id = ?1");
        Ok(conn.query_row(&sql, [id.0], from_row).optional()?)
    }

    /// Case-insensitive lookup by email address.
    pub fn find_by_email(&self, email: &str) -> Result<Option<Account>> {
        let conn = self.conn;
        let sql = format!("SELECT {COLUMNS} FROM accounts WHERE email = ?1");
        Ok(conn.query_row(&sql, [email], from_row).optional()?)
    }

    pub fn list(&self) -> Result<Vec<Account>> {
        let conn = self.conn;
        let sql = format!("SELECT {COLUMNS} FROM accounts ORDER BY id");
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Keyring entry name for the account's password.
    pub fn secret_key(&self, id: AccountId) -> Result<Option<String>> {
        let conn = self.conn;
        Ok(conn
            .query_row(
                "SELECT secret_key FROM accounts WHERE id = ?1",
                [id.0],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Deletes the account and, via cascades, everything stored for it.
    /// Returns the deleted account's secret key, or `None` if there was no such account.
    pub fn delete(&self, id: AccountId) -> Result<Option<String>> {
        let conn = self.conn;
        Ok(conn
            .query_row(
                "DELETE FROM accounts WHERE id = ?1 RETURNING secret_key",
                [id.0],
                |r| r.get(0),
            )
            .optional()?)
    }
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: AccountId(row.get("id")?),
        email: row.get("email")?,
        display_name: row.get("display_name")?,
        username: row.get("username")?,
        imap: ServerConfig {
            host: row.get("imap_host")?,
            port: row.get("imap_port")?,
            security: parse_text(row, "imap_security")?,
        },
        smtp: ServerConfig {
            host: row.get("smtp_host")?,
            port: row.get("smtp_port")?,
            security: parse_text(row, "smtp_security")?,
        },
        auth: parse_text(row, "auth_kind")?,
        created_at: row.get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use catmail_types::{AuthKind, Security};
    use rusqlite::types::Type;

    use super::*;
    use crate::test_util;

    fn new_account(email: &str) -> NewAccount {
        NewAccount {
            email: email.into(),
            display_name: Some("Test".into()),
            username: email.into(),
            imap: ServerConfig {
                host: "imap.example.com".into(),
                port: 993,
                security: Security::Tls,
            },
            smtp: ServerConfig {
                host: "smtp.example.com".into(),
                port: 587,
                security: Security::StartTls,
            },
            auth: AuthKind::Password,
        }
    }

    fn key(email: &str) -> String {
        format!("key:{email}")
    }

    #[test]
    fn insert_get_list_delete() {
        let store = test_util::store();
        let conn = store.conn();
        let accounts = Accounts::new(&conn);

        let a = accounts
            .insert(&new_account("a@example.com"), &key("a@example.com"))
            .unwrap();
        let b = accounts
            .insert(&new_account("b@example.com"), &key("b@example.com"))
            .unwrap();
        assert_eq!(a.smtp.security, Security::StartTls);
        assert!(a.created_at > 0);

        assert_eq!(accounts.get(a.id).unwrap().as_ref(), Some(&a));
        assert_eq!(accounts.list().unwrap(), vec![a.clone(), b.clone()]);
        assert_eq!(
            accounts
                .find_by_email("A@Example.com")
                .unwrap()
                .map(|x| x.id),
            Some(a.id)
        );

        assert_eq!(
            accounts.secret_key(a.id).unwrap().as_deref(),
            Some("key:a@example.com")
        );
        assert_eq!(
            accounts.delete(a.id).unwrap().as_deref(),
            Some("key:a@example.com")
        );
        assert_eq!(accounts.delete(a.id).unwrap(), None);
        assert_eq!(accounts.list().unwrap(), vec![b]);
    }

    #[test]
    fn ids_are_not_reused() {
        let store = test_util::store();
        let a = Accounts::new(&store.conn())
            .insert(&new_account("a@example.com"), &key("a@example.com"))
            .unwrap();
        Accounts::new(&store.conn()).delete(a.id).unwrap();
        let b = Accounts::new(&store.conn())
            .insert(&new_account("a@example.com"), &key("a@example.com"))
            .unwrap();
        assert!(b.id > a.id);
    }

    #[test]
    fn duplicate_email_is_rejected() {
        let store = test_util::store();
        Accounts::new(&store.conn())
            .insert(&new_account("a@example.com"), &key("a@example.com"))
            .unwrap();
        let err = Accounts::new(&store.conn())
            .insert(&new_account("A@example.com"), &key("A@example.com"))
            .unwrap_err();
        assert!(matches!(err, Error::AlreadyExists(_)), "{err:?}");
    }

    #[test]
    fn corrupt_enum_is_reported() {
        let store = test_util::store();
        let a = Accounts::new(&store.conn())
            .insert(&new_account("a@example.com"), &key("a@example.com"))
            .unwrap();
        store
            .conn()
            .execute("UPDATE accounts SET imap_security = 'ssl3'", [])
            .unwrap();
        let err = Accounts::new(&store.conn()).get(a.id).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Sqlite(rusqlite::Error::FromSqlConversionFailure(_, Type::Text, _))
            ),
            "{err:?}"
        );
    }
}
