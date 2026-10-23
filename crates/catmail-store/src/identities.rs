use catmail_types::{AccountId, Identity, IdentityId, NewIdentity};
use rusqlite::{Connection, OptionalExtension, Row, named_params};

use crate::columns::{server, size_limit, size_limit_column};
use crate::drafts::Drafts;
use crate::error::is_unique_violation;
use crate::{Error, Result};

/// An identity with what sending as it takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sending {
    pub identity: Identity,
    /// The SIZE its SMTP server announced when last connected to; `None` if not yet, or
    /// none.
    pub smtp_max_size: Option<u64>,
    /// Keyring entry name of the password its SMTP server takes: its own, or its
    /// account's.
    pub login_key: String,
}

/// What [`Identities::delete`] removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedIdentity {
    pub account: AccountId,
    /// Its own keyring entry, if it had one.
    pub secret_key: Option<String>,
    /// Drafts written as it, now written as the account's default.
    pub drafts: usize,
}

const COLUMNS: &str = "id, account_id, email, display_name, username,
    smtp_host, smtp_port, smtp_security, is_default, created_at";

pub struct Identities<'c> {
    conn: &'c Connection,
}

impl<'c> Identities<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// `secret_key` names the keyring entry that already holds the identity's password;
    /// `smtp_max_size` is the SIZE its server announced, if known. `None` if there is no
    /// such account.
    pub fn insert(
        &self,
        account: AccountId,
        new: &NewIdentity,
        secret_key: &str,
        smtp_max_size: Option<u64>,
    ) -> Result<Option<Identity>> {
        self.insert_row(account, new, Some(secret_key), false, smtp_max_size)
    }

    /// The account's first identity, its default: it logs in with the account's password.
    pub(crate) fn insert_default(&self, account: AccountId, new: &NewIdentity) -> Result<()> {
        self.insert_row(account, new, None, true, None)?;
        Ok(())
    }

    fn insert_row(
        &self,
        account: AccountId,
        new: &NewIdentity,
        secret_key: Option<&str>,
        is_default: bool,
        smtp_max_size: Option<u64>,
    ) -> Result<Option<Identity>> {
        let sql = format!(
            "INSERT INTO identities (account_id, email, display_name, username,
                                     smtp_host, smtp_port, smtp_security, secret_key,
                                     is_default, smtp_max_size)
             SELECT id, :email, :display_name, :username,
                    :smtp_host, :smtp_port, :smtp_security, :secret_key, :is_default,
                    :smtp_max_size
             FROM accounts WHERE id = :account
             RETURNING {COLUMNS}"
        );
        self.conn
            .query_row(
                &sql,
                named_params! {
                    ":account": account.0,
                    ":email": new.email,
                    ":display_name": new.display_name,
                    ":username": new.username,
                    ":smtp_host": new.smtp.host,
                    ":smtp_port": new.smtp.port,
                    ":smtp_security": new.smtp.security.as_str(),
                    ":secret_key": secret_key,
                    ":is_default": is_default,
                    ":smtp_max_size": size_limit_column(smtp_max_size),
                },
                from_row,
            )
            .optional()
            .map_err(|e| {
                if is_unique_violation(&e) {
                    Error::AlreadyExists(format!(
                        "identity {} through {}",
                        new.email, new.smtp.host
                    ))
                } else {
                    e.into()
                }
            })
    }

    pub fn get(&self, id: IdentityId) -> Result<Option<Identity>> {
        let sql = format!("SELECT {COLUMNS} FROM identities WHERE id = ?1");
        Ok(self
            .conn
            .prepare_cached(&sql)?
            .query_row([id.0], from_row)
            .optional()?)
    }

    /// The default first, then the others oldest first.
    pub fn list(&self, account: AccountId) -> Result<Vec<Identity>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM identities WHERE account_id = ?1
             ORDER BY is_default DESC, id"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([account.0], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The account's identity that sends as `email`, in any case: the default if it is
    /// one of those (the same address through two servers), else the oldest.
    pub fn by_email(&self, account: AccountId, email: &str) -> Result<Option<IdentityId>> {
        // `email` compares without case (`COLLATE NOCASE`).
        Ok(self
            .conn
            .prepare_cached(
                "SELECT id FROM identities WHERE account_id = ?1 AND email = ?2
                 ORDER BY is_default DESC, id LIMIT 1",
            )?
            .query_row((account.0, email), |r| Ok(IdentityId(r.get(0)?)))
            .optional()?)
    }

    /// The account's default identity; `None` if there is no such account.
    pub fn default_of(&self, account: AccountId) -> Result<Option<IdentityId>> {
        Ok(self
            .conn
            .prepare_cached("SELECT id FROM identities WHERE account_id = ?1 AND is_default")?
            .query_row([account.0], |r| Ok(IdentityId(r.get(0)?)))
            .optional()?)
    }

    /// Makes it its account's default, in place of the one that was. Returns the
    /// account, or `None` if there is no such identity.
    pub fn set_default(&self, id: IdentityId) -> Result<Option<AccountId>> {
        let Some(account) = self.account_of(id)? else {
            return Ok(None);
        };
        // In two steps: the unique index would see two defaults halfway through one.
        self.conn.execute(
            "UPDATE identities SET is_default = 0 WHERE account_id = ?1 AND is_default",
            [account.0],
        )?;
        self.conn
            .execute("UPDATE identities SET is_default = 1 WHERE id = ?1", [id.0])?;
        Ok(Some(account))
    }

    pub(crate) fn account_of(&self, id: IdentityId) -> Result<Option<AccountId>> {
        Ok(self
            .conn
            .prepare_cached("SELECT account_id FROM identities WHERE id = ?1")?
            .query_row([id.0], |r| Ok(AccountId(r.get(0)?)))
            .optional()?)
    }

    /// The account's identity `id`; `None` if there is no such identity of the account.
    pub fn sending(&self, account: AccountId, id: IdentityId) -> Result<Option<Sending>> {
        let sql = format!(
            "SELECT {COLUMNS}, smtp_max_size,
                    coalesce(secret_key, (SELECT a.secret_key FROM accounts a
                                          WHERE a.id = identities.account_id)) AS login_key
             FROM identities
             WHERE account_id = :account AND id = :id"
        );
        Ok(self
            .conn
            .prepare_cached(&sql)?
            .query_row(named_params! { ":account": account.0, ":id": id.0 }, |r| {
                Ok(Sending {
                    identity: from_row(r)?,
                    smtp_max_size: size_limit(r, "smtp_max_size")?,
                    login_key: r.get("login_key")?,
                })
            })
            .optional()?)
    }

    /// Records the SIZE its SMTP server announced; `None` for none.
    pub fn set_smtp_max_size(&self, id: IdentityId, limit: Option<u64>) -> Result<()> {
        self.conn.execute(
            "UPDATE identities SET smtp_max_size = ?2 WHERE id = ?1",
            (id.0, size_limit_column(limit)),
        )?;
        Ok(())
    }

    /// Keyring entries of the account's identities that have their own, which go with
    /// it.
    pub fn secret_keys(&self, account: AccountId) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT secret_key FROM identities
             WHERE account_id = ?1 AND secret_key IS NOT NULL",
        )?;
        let rows = stmt.query_map([account.0], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Deletes the identity; drafts written as it go to the account's default. Refused
    /// for the default, and while the outbox has messages to send through it: they would
    /// have no server. `None` if there was no such identity.
    pub fn delete(&self, id: IdentityId, now: i64) -> Result<Option<DeletedIdentity>> {
        let Some(identity) = self.get(id)? else {
            return Ok(None);
        };
        if identity.is_default {
            return Err(Error::Conflict(format!(
                "identity {id} is the account's default; make another one the default first"
            )));
        }
        let queued: i64 = self.conn.query_row(
            "SELECT count(*) FROM outbox WHERE identity_id = ?1",
            [id.0],
            |r| r.get(0),
        )?;
        if queued > 0 {
            return Err(Error::Conflict(format!(
                "identity {id} has messages in the outbox; wait for them to be sent, or \
                 cancel them first"
            )));
        }
        let account = identity.account_id;
        let default = self
            .default_of(account)?
            .ok_or_else(|| Error::Conflict(format!("account {account} has no default identity")))?;
        let drafts = Drafts::new(self.conn).reassign_identity(id, default, now)?;
        let secret_key = self.conn.query_row(
            "DELETE FROM identities WHERE id = ?1 RETURNING secret_key",
            [id.0],
            |r| r.get(0),
        )?;
        Ok(Some(DeletedIdentity {
            account,
            secret_key,
            drafts,
        }))
    }

    /// The account's identity `id` or, with `None`, its default. Fails unless the
    /// identity is one of the account's.
    pub(crate) fn resolve(&self, account: AccountId, id: Option<IdentityId>) -> Result<IdentityId> {
        let Some(id) = id else {
            return self
                .default_of(account)?
                .ok_or_else(|| Error::Invalid(format!("account {account} not found")));
        };
        match self.account_of(id)? {
            Some(owner) if owner == account => Ok(id),
            Some(_) => Err(Error::Invalid(format!(
                "identity {id} belongs to another account"
            ))),
            None => Err(Error::Invalid(format!("identity {id} not found"))),
        }
    }
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Identity> {
    Ok(Identity {
        id: IdentityId(row.get("id")?),
        account_id: AccountId(row.get("account_id")?),
        email: row.get("email")?,
        display_name: row.get("display_name")?,
        username: row.get("username")?,
        smtp: server(row, "smtp")?,
        is_default: row.get("is_default")?,
        created_at: row.get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use catmail_types::{BlobHash, DraftContent, DraftId, Security, ServerConfig};

    use super::*;
    use crate::outbox::NewOutboxEntry;
    use crate::test_util::{self, TestStore};

    fn new_identity(email: &str) -> NewIdentity {
        NewIdentity {
            email: email.into(),
            display_name: Some("Newsletter".into()),
            username: "resend".into(),
            smtp: ServerConfig {
                host: "smtp.resend.com".into(),
                port: 465,
                security: Security::Tls,
            },
        }
    }

    fn add(store: &TestStore, account: AccountId, email: &str) -> Identity {
        store
            .write(|db| {
                db.identities().insert(
                    account,
                    &new_identity(email),
                    &format!("key:{account}:{email}"),
                    None,
                )
            })
            .unwrap()
            .unwrap()
    }

    fn default_of(store: &TestStore, account: AccountId) -> IdentityId {
        store
            .read(|db| db.identities().default_of(account))
            .unwrap()
            .unwrap()
    }

    fn draft(store: &TestStore, account: AccountId, identity: Option<IdentityId>) -> DraftId {
        store
            .write(|db| {
                db.drafts()
                    .insert(account, identity, &DraftContent::default(), None, 1)
            })
            .unwrap()
    }

    fn identity_of(store: &TestStore, draft: DraftId) -> IdentityId {
        store
            .read(|db| db.drafts().get(draft))
            .unwrap()
            .unwrap()
            .identity
    }

    #[test]
    fn insert_list_and_refuse_duplicates() {
        let store = test_util::store();
        let a = test_util::account(&store, "a@example.com");
        let own = default_of(&store, a);
        let first = add(&store, a, "news@example.com");
        assert_eq!(first.account_id, a);
        assert!(!first.is_default);
        // The account's own address, through another server, is an identity like any.
        let again = add(&store, a, "a@example.com");
        let list: Vec<_> = store
            .read(|db| db.identities().list(a))
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(list, [own, first.id, again.id], "the default first");
        let sending = |id| {
            store
                .read(|db| db.identities().sending(a, id))
                .unwrap()
                .unwrap()
        };
        assert_eq!(sending(own).identity.id, own);
        assert_eq!(
            sending(first.id).login_key,
            format!("key:{a}:news@example.com"),
            "its own password"
        );
        assert_eq!(sending(first.id).smtp_max_size, None);
        store
            .write(|db| db.identities().set_smtp_max_size(first.id, Some(1000)))
            .unwrap();
        assert_eq!(sending(first.id).smtp_max_size, Some(1000));

        // The same address, server and login.
        let twice = store.write(|db| {
            db.identities()
                .insert(a, &new_identity("NEWS@example.com"), "key:other", None)
        });
        assert!(matches!(twice, Err(Error::AlreadyExists(_))), "{twice:?}");
        // Another account may have it.
        let b = test_util::account(&store, "b@example.com");
        add(&store, b, "news@example.com");
        let missing = store.write(|db| {
            db.identities()
                .insert(AccountId(99), &new_identity("x@example.com"), "key:x", None)
        });
        assert!(matches!(missing, Ok(None)));
    }

    #[test]
    fn drafts_are_written_as_the_default_unless_told_and_only_as_their_accounts() {
        let store = test_util::store();
        let a = test_util::account(&store, "a@example.com");
        let b = test_util::account(&store, "b@example.com");
        let theirs = add(&store, b, "news@example.com");
        let id = draft(&store, a, None);
        assert_eq!(identity_of(&store, id), default_of(&store, a));

        let content = DraftContent::default();
        let insert = store.write(|db| db.drafts().insert(a, Some(theirs.id), &content, None, 1));
        assert!(matches!(insert, Err(Error::Invalid(_))), "{insert:?}");
        let update = store.write(|db| db.drafts().update(id, theirs.id, &content, 2));
        assert!(matches!(update, Err(Error::Invalid(_))), "{update:?}");

        let mine = add(&store, a, "news@example.com");
        store
            .write(|db| db.drafts().update(id, mine.id, &content, 3))
            .unwrap();
        assert_eq!(identity_of(&store, id), mine.id);
    }

    #[test]
    fn the_default_can_be_changed_but_not_deleted() {
        let store = test_util::store();
        let a = test_util::account(&store, "a@example.com");
        let own = default_of(&store, a);
        let news = add(&store, a, "news@example.com");
        let refused = store.write(|db| db.identities().delete(own, 5));
        assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");

        assert_eq!(
            store
                .write(|db| db.identities().set_default(news.id))
                .unwrap(),
            Some(a)
        );
        assert_eq!(default_of(&store, a), news.id);
        let list = store.read(|db| db.identities().list(a)).unwrap();
        assert_eq!((list[0].id, list[0].is_default), (news.id, true));
        assert_eq!((list[1].id, list[1].is_default), (own, false));
        // New drafts are from it, and the first one can go now.
        let id = draft(&store, a, None);
        assert_eq!(identity_of(&store, id), news.id);
        let deleted = store.write(|db| db.identities().delete(own, 5)).unwrap();
        assert_eq!(
            deleted.map(|d| d.secret_key),
            Some(None),
            "it had the account's"
        );
        assert_eq!(
            store
                .write(|db| db.identities().set_default(IdentityId(99)))
                .unwrap(),
            None
        );
    }

    #[test]
    fn by_email_prefers_the_default_then_the_oldest() {
        let store = test_util::store();
        let a = test_util::account(&store, "a@example.com");
        let b = test_util::account(&store, "b@example.com");
        let own = default_of(&store, a);
        let by_email = |account, email: &str| {
            store
                .read(|db| db.identities().by_email(account, email))
                .unwrap()
        };
        let first = add(&store, a, "news@example.com");
        let second = store
            .write(|db| {
                let other_login = NewIdentity {
                    username: "other".into(),
                    ..new_identity("news@example.com")
                };
                db.identities().insert(a, &other_login, "key:other", None)
            })
            .unwrap()
            .unwrap();
        assert_eq!(by_email(a, "NEWS@example.com"), Some(first.id));
        store
            .write(|db| db.identities().set_default(second.id))
            .unwrap();
        assert_eq!(by_email(a, "news@example.com"), Some(second.id));
        // The account's own address through another server: the first identity is older.
        add(&store, a, "a@example.com");
        assert_eq!(by_email(a, "a@example.com"), Some(own));
        assert_eq!(by_email(a, "b@example.com"), None, "another account's");
        assert_eq!(by_email(b, "news@example.com"), None);
    }

    #[test]
    fn deleting_one_hands_its_drafts_to_the_default_but_not_its_outbox() {
        let store = test_util::store();
        let a = test_util::account(&store, "a@example.com");
        let identity = add(&store, a, "news@example.com");
        let written = draft(&store, a, Some(identity.id));
        let sending = draft(&store, a, Some(identity.id));
        let revision = |id| {
            store
                .read(|db| Ok(db.drafts().snapshot(id)?.unwrap().revision))
                .unwrap()
        };
        let before = revision(written);
        let outbox = store
            .write(|db| {
                db.outbox().queue(
                    &NewOutboxEntry {
                        account: a,
                        draft: sending,
                        revision: revision(sending),
                        raw: BlobHash::parse(&"a".repeat(64)).unwrap(),
                        message_id: "m@example.com".into(),
                        subject: "s".into(),
                        envelope_from: "news@example.com".into(),
                        recipients: vec!["b@example.com".into()],
                    },
                    1,
                )
            })
            .unwrap();
        let entry = store.read(|db| db.outbox().get(outbox)).unwrap().unwrap();
        assert_eq!(entry.identity, identity.id, "the draft's");

        let refused = store.write(|db| db.identities().delete(identity.id, 5));
        assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
        store.write(|db| db.outbox().cancel(outbox)).unwrap();
        let deleted = store
            .write(|db| db.identities().delete(identity.id, 5))
            .unwrap();
        assert_eq!(
            deleted,
            Some(DeletedIdentity {
                account: a,
                secret_key: Some(format!("key:{a}:news@example.com")),
                // The one cancelled is a draft again.
                drafts: 2,
            })
        );
        let written = store.read(|db| db.drafts().get(written)).unwrap().unwrap();
        assert_eq!(written.identity, default_of(&store, a));
        assert_eq!(revision(written.id), before + 1, "uploaded again");
        assert_eq!(written.updated_at, 5);
        // Saved by someone who still had the one removed: it stays from the default.
        let saved = DraftContent {
            subject: "later".into(),
            ..written.content.clone()
        };
        store
            .write(|db| db.drafts().update(written.id, identity.id, &saved, 6))
            .unwrap();
        let again = store
            .read(|db| db.drafts().get(written.id))
            .unwrap()
            .unwrap();
        assert_eq!(again.content, saved);
        assert_eq!(again.identity, default_of(&store, a));
        assert_eq!(
            store
                .write(|db| db.identities().delete(identity.id, 5))
                .unwrap(),
            None
        );
    }

    #[test]
    fn they_go_with_their_account() {
        let store = test_util::store();
        let a = test_util::account(&store, "a@example.com");
        let identity = add(&store, a, "news@example.com");
        draft(&store, a, Some(identity.id));
        assert_eq!(
            store.read(|db| db.identities().secret_keys(a)).unwrap(),
            [format!("key:{a}:news@example.com")]
        );
        store.write(|db| db.accounts().delete(a)).unwrap();
        assert_eq!(
            store.read(|db| db.identities().get(identity.id)).unwrap(),
            None
        );
        assert!(store.read(|db| db.identities().list(a)).unwrap().is_empty());
    }
}
