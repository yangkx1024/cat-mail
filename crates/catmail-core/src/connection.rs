//! Logged-in IMAP (and SMTP) connections for an account.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use catmail_proto::imap::Session;
use catmail_proto::{Credentials, smtp};
use catmail_store::{Db, DraftSnapshot};
use catmail_types::{Account, AccountId, Address, DraftId, IdentityId, ServerConfig};
use secrecy::{ExposeSecret, SecretString};

use crate::engine::{Engine, blocking};
use crate::error::ImapError;
use crate::{Error, Result};

/// Per network read or write, not per command.
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(60);

/// Opens a new connection to the account's IMAP server and logs in.
pub(crate) async fn connect(engine: &Arc<Engine>, account_id: AccountId) -> Result<Session> {
    let (account, credentials, append_limit) = account_credentials(engine, account_id).await?;
    let session = login(engine, &account.imap, &credentials).await?;
    let announced = announced_append_limit(session.capabilities());
    remember_limit(engine, append_limit, announced, move |db, limit| {
        db.accounts().set_append_limit(account_id, limit)
    })
    .await;
    Ok(session)
}

/// The largest message the IMAP server takes in APPEND, as recorded. `APPENDLIMIT=0` is no
/// APPEND at all (RFC 7889): no size, nor anything for attachments to fit, since no copy
/// of any size is kept.
pub(crate) fn announced_append_limit(
    capabilities: &catmail_proto::imap::Capabilities,
) -> Option<u64> {
    capabilities.append_limit().filter(|&limit| limit > 0)
}

/// Records the size limit a server just announced (IMAP APPENDLIMIT, SMTP SIZE), if it
/// isn't the one `recorded`: servers change it. Failing to only leaves the old one, which
/// sending checks anyway.
async fn remember_limit(
    engine: &Arc<Engine>,
    recorded: Option<u64>,
    announced: Option<u64>,
    record: impl FnOnce(&Db<'_>, Option<u64>) -> catmail_store::Result<()> + Send + 'static,
) {
    if recorded != announced {
        record_limit(engine, move |db| record(db, announced)).await;
    }
}

/// Writes a size limit with `record`, which only logs failing to: see [`remember_limit`].
async fn record_limit(
    engine: &Arc<Engine>,
    record: impl FnOnce(&Db<'_>) -> catmail_store::Result<()> + Send + 'static,
) {
    if let Err(e) = engine.write(record).await {
        tracing::warn!(error = %e, "couldn't record the server's size limit");
    }
}

/// The draft as it is now, and who it is from; `None` if it is gone.
pub(crate) fn draft_sender(
    db: &Db<'_>,
    draft: DraftId,
) -> catmail_store::Result<Option<(DraftSnapshot, Address)>> {
    let Some(snapshot) = db.drafts().snapshot(draft)? else {
        return Ok(None);
    };
    let d = &snapshot.draft;
    // The composite key keeps the identity the draft's account's.
    let identity = db.identities().get(d.identity)?;
    Ok(identity.map(|identity| (snapshot, identity.address())))
}

/// Identities of an account that send over the same SMTP login: the same server,
/// username and password, as aliases set up at one provider are.
pub(crate) struct SmtpLogin {
    /// With the SIZE recorded for each.
    identities: Vec<(SendAs, Option<u64>)>,
    server: ServerConfig,
    credentials: Credentials,
}

impl SmtpLogin {
    pub fn identities(&self) -> impl Iterator<Item = &SendAs> {
        self.identities.iter().map(|(identity, _)| identity)
    }

    /// Opens a new connection and logs in. The SIZE the server announces is recorded for
    /// every identity sending over it, as [`remember_limit`] does.
    pub async fn connect(&mut self, engine: &Arc<Engine>) -> Result<smtp::Session> {
        let session = smtp_login(engine, &self.server, &self.credentials).await?;
        let announced = session.max_size();
        let changed: Vec<_> = self
            .identities
            .iter_mut()
            .filter(|(_, recorded)| *recorded != announced)
            .map(|(identity, recorded)| {
                *recorded = announced;
                identity.id
            })
            .collect();
        if !changed.is_empty() {
            record_limit(engine, move |db| {
                changed
                    .into_iter()
                    .try_for_each(|identity| db.identities().set_smtp_max_size(identity, announced))
            })
            .await;
        }
        Ok(session)
    }
}

/// An identity of an account, with the address it sends as.
#[derive(Clone)]
pub(crate) struct SendAs {
    pub id: IdentityId,
    pub email: String,
}

/// The logins that send as the account's `identities`, each identity in exactly one, in
/// the order of the first identity of each. One that is gone, or whose password can't be
/// read, comes back with why.
///
/// Identities with the same server and username but different passwords stay apart: one
/// of the passwords is out of date, and logging in with it mustn't hold back the other.
pub(crate) async fn smtp_logins(
    engine: &Arc<Engine>,
    account: AccountId,
    identities: Vec<IdentityId>,
) -> Result<Vec<std::result::Result<SmtpLogin, (SendAs, Error)>>> {
    let found = engine
        .read(move |db| {
            identities
                .into_iter()
                .map(|identity| Ok((identity, db.identities().sending(account, identity)?)))
                .collect::<catmail_store::Result<Vec<_>>>()
        })
        .await?;
    let mut logins: Vec<std::result::Result<SmtpLogin, _>> = Vec::new();
    // Each keyring entry read once: the account's serves every identity without its own.
    let mut passwords: HashMap<String, SecretString> = HashMap::new();
    for (identity, sending) in found {
        // One gone has no address, nor needs one: nothing is recorded about sending as it.
        let email = sending.as_ref().map(|s| s.identity.email.clone());
        let send_as = SendAs {
            id: identity,
            email: email.unwrap_or_default(),
        };
        let Some(sending) = sending else {
            logins.push(Err((send_as, Error::IdentityNotFound(identity))));
            continue;
        };
        let key = sending.login_key;
        let password = match passwords.get(&key) {
            Some(password) => password.clone(),
            None => match password(engine, key.clone(), &sending.identity.email).await {
                Ok(password) => passwords.entry(key).or_insert(password).clone(),
                Err(e) => {
                    logins.push(Err((send_as, e)));
                    continue;
                }
            },
        };
        let (server, username) = (sending.identity.smtp, sending.identity.username);
        let shared = logins.iter_mut().flatten().find(|login| {
            login.server == server
                && login.credentials.username == username
                && login.credentials.password.expose_secret() == password.expose_secret()
        });
        match shared {
            Some(login) => login.identities.push((send_as, sending.smtp_max_size)),
            None => logins.push(Ok(SmtpLogin {
                identities: vec![(send_as, sending.smtp_max_size)],
                server,
                credentials: Credentials { username, password },
            })),
        }
    }
    Ok(logins)
}

/// Opens a new connection to the SMTP server `server` and logs in.
pub(crate) async fn smtp_login(
    engine: &Engine,
    server: &ServerConfig,
    credentials: &Credentials,
) -> Result<smtp::Session> {
    let mut session = smtp::Session::connect(server, &engine.tls()?, IO_TIMEOUT).await?;
    session.login(credentials).await?;
    Ok(session)
}

/// Also the APPENDLIMIT the server announced when last connected to.
async fn account_credentials(
    engine: &Arc<Engine>,
    account_id: AccountId,
) -> Result<(Account, Credentials, Option<u64>)> {
    let ((account, secret_key), append_limit) = engine
        .read(move |db| {
            let accounts = db.accounts();
            let found = accounts
                .get(account_id)?
                .zip(accounts.secret_key(account_id)?);
            let append_limit = accounts.append_limit(account_id)?;
            Ok(found.map(|found| (found, append_limit)))
        })
        .await?
        .ok_or_else(|| Error::AccountNotFound(account_id.to_string()))?;
    let password = password(engine, secret_key, &account.email).await?;
    let credentials = Credentials {
        username: account.username.clone(),
        password,
    };
    Ok((account, credentials, append_limit))
}

/// The password in the keyring entry `secret_key`, for the account or identity with
/// that address.
async fn password(engine: &Arc<Engine>, secret_key: String, email: &str) -> Result<SecretString> {
    let engine = engine.clone();
    blocking(move || engine.secrets.get(&secret_key))
        .await?
        .ok_or_else(|| {
            Error::Secrets(format!(
                "no stored password for {email}; remove it and add it again"
            ))
        })
}

/// Opens a new connection to `server` and logs in.
pub(crate) async fn login(
    engine: &Engine,
    server: &ServerConfig,
    credentials: &Credentials,
) -> Result<Session> {
    let mut session = Session::connect(server, &engine.tls()?, IO_TIMEOUT).await?;
    session.login(credentials).await?;
    Ok(session)
}

/// Runs `op` on one of the account's idle connections, or a new one, and keeps the
/// connection for next time unless it failed; see [`on_session`].
///
/// Pass an `async move` closure that owns what it captures (a clone of the engine, not a
/// reference to it): with a borrowed capture held across an `.await`, the closure's
/// future can't be proven `Send` (a compiler limitation with higher-ranked lifetimes),
/// so neither can the caller's.
pub(crate) async fn with_session<T>(
    engine: &Arc<Engine>,
    account: AccountId,
    op: impl AsyncFnMut(&mut Session) -> Result<T>,
) -> Result<T> {
    let mut slot = engine.take_session(account);
    let result = on_session(engine, account, &mut slot, op).await;
    // Anything but a fatal error leaves the connection in a known state.
    if let Some(session) = slot
        && !matches!(&result, Err(Error::Imap(ImapError(e))) if e.is_fatal())
    {
        engine.return_session(account, session);
    }
    result
}

/// Runs `op` on the connection in `slot`, or on a new one put there. If the one there
/// turns out to be dead (disconnected, not merely unusable after a refusal), `op` runs
/// once more on a fresh one.
pub(crate) async fn on_session<T>(
    engine: &Arc<Engine>,
    account: AccountId,
    slot: &mut Option<Session>,
    mut op: impl AsyncFnMut(&mut Session) -> Result<T>,
) -> Result<T> {
    if let Some(session) = slot {
        match op(session).await {
            // A fresh connection, not another idle one: those are likely dead too.
            Err(Error::Imap(ImapError(e))) if e.is_disconnected() => {
                tracing::debug!(%account, error = %e, "reused connection was dead; reconnecting");
            }
            result => return result,
        }
    }
    *slot = None;
    let session = slot.insert(connect(engine, account).await?);
    op(session).await
}
