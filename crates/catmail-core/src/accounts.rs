//! Accounts and the identities they send as: adding them (logging in to their servers
//! first, if asked), changing and removing them, with their passwords in the keyring.
//! Adding writes the keyring entry before the row naming it ([`insert_with_secret`]);
//! removing deletes the row (`RETURNING secret_key`) before the entry.

use std::sync::Arc;

use catmail_proto::Credentials;
use catmail_proto::imap::Session;
use catmail_store::{Identities, SmtpAnnounced};
use catmail_types::{
    Account, AccountId, Event, Identity, IdentityId, NewAccount, NewIdentity, ServerConfig,
};
use secrecy::{ExposeSecret, SecretString};
use uuid::Uuid;

use crate::connection::Announced;
use crate::engine::Engine;
use crate::task::{blocking, now};
use crate::{Error, Result, addresses, connection, drafts, prefetch, watch};

/// Whether [`MailClient::add_account`](crate::MailClient::add_account) and
/// [`MailClient::add_identity`](crate::MailClient::add_identity) log in to the servers
/// before storing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verify {
    /// Log in first; store nothing if that fails.
    Login,
    /// Store without contacting the servers (offline, or a server not up yet).
    Skip,
}

/// Starts the background work the engine does per account (sync, prefetch of bodies), if
/// it does any: for an account added, or kept after a removal failed. Call from inside the
/// engine runtime.
fn spawn_background(engine: &Arc<Engine>, account: AccountId) {
    watch::spawn(engine, account);
    prefetch::spawn(engine, account);
}

/// Cleans up and checks a new account before anything is stored or contacted.
pub(crate) fn check_new_account(new: &mut NewAccount, password: &SecretString) -> Result<()> {
    new.display_name = drafts::clean_name(new.display_name.as_deref());
    validate_new_account(new)?;
    check_password(password)
}

/// Cleans up and checks a new identity before anything is stored or contacted.
pub(crate) fn check_new_identity(new: &mut NewIdentity, password: &SecretString) -> Result<()> {
    new.display_name = drafts::clean_name(new.display_name.as_deref());
    validate_new_identity(new)?;
    check_password(password)
}

/// Stores the account, checked with [`check_new_account`], and its password: see
/// [`MailClient::add_account`](crate::MailClient::add_account).
pub(crate) async fn add_account(
    engine: Arc<Engine>,
    new: NewAccount,
    password: SecretString,
    verify: Verify,
) -> Result<Account> {
    let (smtp, session) = match verify {
        Verify::Login => {
            let (smtp, session) =
                check_account(&engine, &new, &password)
                    .await
                    .inspect_err(|e| {
                        tracing::warn!(
                            imap = %server(&new.imap),
                            smtp = %server(&new.smtp),
                            error = %e,
                            "couldn't sign in to a new account"
                        );
                    })?;
            (Some(smtp), Some(session))
        }
        Verify::Skip => (None, None),
    };
    let announced = session.as_ref().map(Announced::of).unwrap_or_default();
    let inserted = blocking({
        let engine = engine.clone();
        move || {
            insert_with_secret(&engine, "account", &new.email, &password, |key| {
                Ok(engine.store.write(|db| {
                    let account = db.accounts().insert(&new, key)?;
                    announced.record(db, account.id)?;
                    if let Some(own) = db.identities().default_of(account.id)?
                        && let Some(smtp) = smtp
                    {
                        db.identities().set_smtp_announced(own, smtp)?;
                    }
                    Ok(Account {
                        gmail_server: announced.gmail,
                        ..account
                    })
                })?)
            })
        }
    })
    .await;
    let account = match inserted {
        Ok(account) => account,
        Err(e) => {
            if let Some(session) = session {
                session.logout().await;
            }
            return Err(e);
        }
    };
    engine.emit(Event::AccountAdded {
        account: account.id,
    });
    // For background sync to start on, or the next request.
    if let Some(session) = session {
        engine.pool(account.id).put(session);
    }
    spawn_background(&engine, account.id);
    Ok(account)
}

/// Stores another address the account sends as, checked with [`check_new_identity`]:
/// see [`MailClient::add_identity`](crate::MailClient::add_identity).
pub(crate) async fn add_identity(
    engine: Arc<Engine>,
    account: AccountId,
    new: NewIdentity,
    password: SecretString,
    verify: Verify,
) -> Result<Identity> {
    let smtp = match verify {
        Verify::Login => check_identity(&engine, &new, &password)
            .await
            .inspect_err(|e| {
                tracing::warn!(
                    %account,
                    smtp = %server(&new.smtp),
                    error = %e,
                    "couldn't sign in for a new sending address"
                );
            })
            .map(Some)?,
        Verify::Skip => None,
    };
    let identity = blocking({
        let engine = engine.clone();
        move || {
            insert_with_secret(&engine, "identity", &new.email, &password, |key| {
                engine
                    .store
                    .write(|db| db.identities().insert(account, &new, key, smtp))?
                    .ok_or_else(|| Error::AccountNotFound(account.to_string()))
            })
        }
    })
    .await?;
    engine.emit(Event::AccountChanged { account });
    Ok(identity)
}

/// Makes a change to an identity that returns its account and how many drafts it
/// changed, `None` if there is no such identity. Blocking.
pub(crate) fn update_identity(
    engine: &Engine,
    id: IdentityId,
    change: impl FnOnce(&Identities<'_>) -> catmail_store::Result<Option<(AccountId, usize)>>,
) -> Result<()> {
    let (account, drafts) = engine
        .store
        .write(|db| change(&db.identities()))?
        .ok_or(Error::IdentityNotFound(id))?;
    engine.emit(Event::AccountChanged { account });
    if drafts > 0 {
        drafts::changed(engine, account);
    }
    Ok(())
}

/// See [`MailClient::remove_identity`](crate::MailClient::remove_identity). Blocking.
pub(crate) fn remove_identity(engine: &Engine, id: IdentityId) -> Result<()> {
    let deleted = engine
        .store
        .write(|db| db.identities().delete(id, now()))?
        .ok_or(Error::IdentityNotFound(id))?;
    if let Some(key) = &deleted.secret_key {
        forget_secret(engine, key);
    }
    let account = deleted.account;
    engine.forget_identity(account, id);
    engine.emit(Event::AccountChanged { account });
    if deleted.drafts > 0 {
        drafts::changed(engine, account);
    }
    Ok(())
}

/// See [`MailClient::remove_account`](crate::MailClient::remove_account).
pub(crate) async fn remove_account(engine: Arc<Engine>, id: AccountId) -> Result<()> {
    let _stopped = engine.start_removal(id).await;
    let result = blocking({
        let engine = engine.clone();
        move || {
            let deleted = engine.store.write(|db| {
                // Its identities go with it.
                let identities = db.identities().secret_keys(id)?;
                Ok(db.accounts().delete(id)?.map(|key| (key, identities)))
            })?;
            let Some((secret_key, identities)) = deleted else {
                return Err(Error::AccountNotFound(id.to_string()));
            };
            for key in std::iter::once(secret_key).chain(identities) {
                forget_secret(&engine, &key);
            }
            engine.emit(Event::AccountRemoved { account: id });
            Ok(())
        }
    })
    .await;
    if let Err(e) = &result {
        // Nothing was removed.
        engine.cancel_removal(id);
        // Its background sync stopped for the removal; the account stays.
        if !matches!(e, Error::AccountNotFound(_)) {
            spawn_background(&engine, id);
        }
    }
    result
}

fn check_password(password: &SecretString) -> Result<()> {
    if password.expose_secret().is_empty() {
        return Err(Error::InvalidInput("password must not be empty".into()));
    }
    Ok(())
}

/// Logs in to the account's IMAP and SMTP servers, both at once, and out of the SMTP
/// server again; returns what the SMTP server announced, and the IMAP connection.
async fn check_account(
    engine: &Engine,
    new: &NewAccount,
    password: &SecretString,
) -> Result<(SmtpAnnounced, Session)> {
    let credentials = Credentials {
        username: new.username.clone(),
        password: password.clone(),
    };
    // IMAP's failure is the one to report if both fail, and once it has failed SMTP's
    // answer isn't waited for.
    let mut imap = std::pin::pin!(connection::login(engine, &new.imap, &credentials));
    let mut smtp = std::pin::pin!(check_smtp(engine, &new.smtp, &credentials));
    let (session, smtp) = tokio::select! {
        session = &mut imap => (session?, smtp.await),
        smtp = &mut smtp => (imap.await?, smtp),
    };
    match smtp {
        Ok(smtp) => Ok((smtp, session)),
        Err(e) => {
            session.logout().await;
            Err(e)
        }
    }
}

async fn check_identity(
    engine: &Engine,
    new: &NewIdentity,
    password: &SecretString,
) -> Result<SmtpAnnounced> {
    let credentials = Credentials {
        username: new.username.clone(),
        password: password.clone(),
    };
    check_smtp(engine, &new.smtp, &credentials).await
}

/// A server as the log shows it: where, and how it is secured. The address and username
/// stay out of the log.
fn server(server: &ServerConfig) -> String {
    format!("{}:{} {}", server.host, server.port, server.security)
}

/// Logs in to the SMTP server `server` and out again; returns what it announced. Its
/// failure as [`Error::OutgoingServer`].
async fn check_smtp(
    engine: &Engine,
    server: &ServerConfig,
    credentials: &Credentials,
) -> Result<SmtpAnnounced> {
    let session = connection::smtp_login(engine, server, credentials)
        .await
        .map_err(|e| Error::OutgoingServer(Box::new(e)))?;
    let announced = connection::announced(&session);
    session.quit().await;
    Ok(announced)
}

/// Stores `password` in the keyring under a new key for a `kind` (`account`, `identity`)
/// with that address, then runs `insert`, which stores the row naming the key; deletes
/// the key again if that fails. Secret first, row second: whoever can see the row can
/// also see its secret, so a concurrent removal always finds and deletes it. The random
/// key keeps data directories apart.
fn insert_with_secret<T>(
    engine: &Engine,
    kind: &str,
    email: &str,
    password: &SecretString,
    insert: impl FnOnce(&str) -> Result<T>,
) -> Result<T> {
    let key = format!("{kind}:{email}:{}", Uuid::new_v4().simple());
    engine.secrets.set(&key, password)?;
    let inserted = insert(&key);
    if inserted.is_err()
        && let Err(e) = engine.secrets.delete(&key)
    {
        tracing::warn!(error = %e, "failed to delete password after {kind} insert failed");
    }
    inserted
}

/// Deletes the keyring entry of a row already deleted: left behind, it would only be
/// clutter.
fn forget_secret(engine: &Engine, key: &str) {
    if let Err(e) = engine.secrets.delete(key) {
        tracing::warn!(error = %e, "failed to delete stored password");
    }
}

fn validate_new_identity(new: &NewIdentity) -> Result<()> {
    // It goes into MAIL FROM and the From header.
    addresses::check_address(&new.email)?;
    if new.username.is_empty() {
        return Err(Error::InvalidInput("username must not be empty".into()));
    }
    validate_server("SMTP", &new.smtp)
}

fn validate_new_account(new: &NewAccount) -> Result<()> {
    let invalid = |msg: &str| Err(Error::InvalidInput(msg.into()));
    // Not checked as an address to send from: an account whose address can't be sent
    // from yet (non-ASCII) still reads its mail, and is refused when queueing.
    let Some((local, domain)) = new.email.split_once('@') else {
        return invalid("email must contain `@`");
    };
    if local.is_empty() || domain.is_empty() {
        return invalid("email must have a local part and a domain");
    }
    if new.username.is_empty() {
        return invalid("username must not be empty");
    }
    validate_server("IMAP", &new.imap)?;
    validate_server("SMTP", &new.smtp)
}

fn validate_server(proto: &str, server: &ServerConfig) -> Result<()> {
    if server.host.is_empty() {
        return Err(Error::InvalidInput(format!(
            "{proto} host must not be empty"
        )));
    }
    if server.port == 0 {
        return Err(Error::InvalidInput(format!("{proto} port must not be 0")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::runtime::Handle;

    use super::*;
    use crate::client::fixtures::{GatedSecrets, new_account, open_client, secret_key, stored};
    use crate::{Config, MailClient, MemorySecrets, SecretStore};

    struct FailingSecrets;

    impl SecretStore for FailingSecrets {
        fn set(&self, _: &str, _: &SecretString) -> Result<()> {
            Err(Error::Secrets("locked".into()))
        }
        fn get(&self, _: &str) -> Result<Option<SecretString>> {
            Ok(None)
        }
        fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn account_lifecycle_on_shared_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let client = MailClient::builder(Config::with_data_dir(dir.path()))
            .runtime(Handle::current())
            .secrets(secrets.clone())
            .open()
            .unwrap();
        let mut events = client.subscribe();

        let account = client
            .add_account(new_account("a@example.com"), "hunter2".into(), Verify::Skip)
            .await
            .unwrap();
        assert_eq!(
            events.recv().await.unwrap(),
            Event::AccountAdded {
                account: account.id
            }
        );
        let key = secret_key(&client, account.id);
        assert_eq!(stored(&secrets, &key).as_deref(), Some("hunter2"));
        assert_eq!(client.list_accounts().await.unwrap(), vec![account.clone()]);

        client.remove_account(account.id).await.unwrap();
        assert_eq!(
            events.recv().await.unwrap(),
            Event::AccountRemoved {
                account: account.id
            }
        );
        assert!(secrets.get(&key).unwrap().is_none());
        assert!(matches!(
            client.remove_account(account.id).await,
            Err(Error::AccountNotFound(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn identity_passwords_are_kept_apart_and_removed_with_them() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let client = open_client(dir.path(), secrets.clone());
        let account = client
            .add_account(new_account("a@example.com"), "imap-pw".into(), Verify::Skip)
            .await
            .unwrap();
        let new = NewIdentity {
            email: "news@example.com".into(),
            display_name: None,
            username: "resend".into(),
            smtp: new_account("a@example.com").smtp,
        };
        let identity = client
            .add_identity(account.id, new.clone(), "api-key".into(), Verify::Skip)
            .await
            .unwrap();
        let key = |id| {
            client
                .engine()
                .unwrap()
                .store
                .read(|db| db.identities().sending(account.id, id))
                .unwrap()
                .unwrap()
                .login_key
        };
        assert_eq!(
            stored(&secrets, &key(identity.id)).as_deref(),
            Some("api-key")
        );
        assert_eq!(secrets.len(), 2);

        // A duplicate leaves no password behind, nor does a missing account.
        let again = client
            .add_identity(account.id, new.clone(), "other".into(), Verify::Skip)
            .await;
        assert!(matches!(again, Err(Error::AlreadyExists(_))), "{again:?}");
        let missing = client
            .add_identity(AccountId(99), new.clone(), "other".into(), Verify::Skip)
            .await;
        assert!(
            matches!(missing, Err(Error::AccountNotFound(_))),
            "{missing:?}"
        );
        assert_eq!(secrets.len(), 2);

        client.remove_identity(identity.id).await.unwrap();
        assert_eq!(secrets.len(), 1);
        assert!(matches!(
            client.remove_identity(identity.id).await,
            Err(Error::IdentityNotFound(_))
        ));

        client
            .add_identity(account.id, new, "api-key".into(), Verify::Skip)
            .await
            .unwrap();
        assert_eq!(secrets.len(), 2);
        client.remove_account(account.id).await.unwrap();
        assert_eq!(secrets.len(), 0, "the identity's went with the account");
    }

    #[test]
    fn secret_store_failure_rolls_back_account() {
        let dir = tempfile::tempdir().unwrap();
        let client = MailClient::builder(Config::with_data_dir(dir.path()))
            .secrets(Arc::new(FailingSecrets))
            .open()
            .unwrap();

        let result = pollster::block_on(client.add_account(
            new_account("a@example.com"),
            "pw".into(),
            Verify::Skip,
        ));
        assert!(matches!(result, Err(Error::Secrets(_))));
        assert!(
            pollster::block_on(client.list_accounts())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let client = MailClient::builder(Config::with_data_dir(dir.path()))
            .secrets(Arc::new(MemorySecrets::default()))
            .open()
            .unwrap();

        let mut bad = new_account("no-at-sign");
        let r = pollster::block_on(client.add_account(bad.clone(), "pw".into(), Verify::Skip));
        assert!(matches!(r, Err(Error::InvalidInput(_))));

        bad.email = "a@example.com".into();
        bad.imap.port = 0;
        let r = pollster::block_on(client.add_account(bad, "pw".into(), Verify::Skip));
        assert!(matches!(r, Err(Error::InvalidInput(_))));

        let r = pollster::block_on(client.add_account(
            new_account("a@example.com"),
            "".into(),
            Verify::Skip,
        ));
        assert!(matches!(r, Err(Error::InvalidInput(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn data_dirs_do_not_share_credentials() {
        let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let secrets = Arc::new(MemorySecrets::default());
        let a = open_client(dir_a.path(), secrets.clone());
        let b = open_client(dir_b.path(), secrets.clone());

        let acc_a = a
            .add_account(new_account("a@example.com"), "pw-a".into(), Verify::Skip)
            .await
            .unwrap();
        let acc_b = b
            .add_account(new_account("b@example.com"), "pw-b".into(), Verify::Skip)
            .await
            .unwrap();
        assert_eq!(acc_a.id, acc_b.id, "both databases start at the same id");

        b.remove_account(acc_b.id).await.unwrap();
        assert_eq!(
            stored(&secrets, &secret_key(&a, acc_a.id)).as_deref(),
            Some("pw-a")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remove_during_add_leaves_no_orphaned_secret() {
        let dir = tempfile::tempdir().unwrap();
        let (secrets, release) = GatedSecrets::new();
        let client = open_client(dir.path(), secrets.clone());

        let add = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .add_account(new_account("a@example.com"), "pw".into(), Verify::Skip)
                    .await
            }
        });
        // The password write is in flight; the row must not be visible yet.
        secrets.entered.notified().await;
        let id = AccountId(1);
        assert!(matches!(
            client.remove_account(id).await,
            Err(Error::AccountNotFound(_))
        ));
        release.send(()).unwrap();
        assert_eq!(add.await.unwrap().unwrap().id, id);
        assert!(
            !client.engine().unwrap().cancellation(id).is_cancelled(),
            "the failed removal must not leave the new account marked removed"
        );

        client.remove_account(id).await.unwrap();
        assert_eq!(secrets.inner.len(), 0, "no orphaned credential");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_insert_deletes_the_new_secret() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let client = open_client(dir.path(), secrets.clone());

        client
            .add_account(new_account("a@example.com"), "pw".into(), Verify::Skip)
            .await
            .unwrap();
        let dup = client
            .add_account(new_account("A@example.com"), "pw".into(), Verify::Skip)
            .await;
        assert!(matches!(dup, Err(Error::AlreadyExists(_))), "{dup:?}");
        assert_eq!(secrets.len(), 1);
    }
}
