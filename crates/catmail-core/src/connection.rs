//! Logged-in IMAP (and SMTP) connections for an account.

use std::sync::Arc;
use std::time::Duration;

use catmail_proto::Credentials;
use catmail_proto::imap::Session;
use catmail_store::{Db, DraftSnapshot};
use catmail_types::{Account, AccountId, Address, DraftId, IdentityId, ServerConfig};
use secrecy::SecretString;

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
    if recorded == announced {
        return;
    }
    if let Err(e) = engine.write(move |db| record(db, announced)).await {
        tracing::warn!(error = %e, "couldn't record the server's size limit");
    }
}

/// Who a message is sent as, and the SMTP server that sends it.
pub(crate) struct Sender {
    pub from: Address,
    /// The SIZE the server announced when last connected to.
    pub smtp_max_size: Option<u64>,
    smtp: ServerConfig,
    username: String,
    login_key: String,
}

/// The account's identity `identity`, its default with `None`. `None` if the account or
/// the identity is gone.
pub(crate) fn sender(
    db: &Db<'_>,
    account: AccountId,
    identity: Option<IdentityId>,
) -> catmail_store::Result<Option<Sender>> {
    Ok(db
        .identities()
        .sending(account, identity)?
        .map(|sending| Sender {
            from: sending.identity.address(),
            smtp_max_size: sending.smtp_max_size,
            smtp: sending.identity.smtp,
            username: sending.identity.username,
            login_key: sending.login_key,
        }))
}

/// The draft as it is now, and who it is from; `None` if it is gone.
pub(crate) fn draft_sender(
    db: &Db<'_>,
    draft: DraftId,
) -> catmail_store::Result<Option<(DraftSnapshot, Sender)>> {
    let Some(snapshot) = db.drafts().snapshot(draft)? else {
        return Ok(None);
    };
    let d = &snapshot.draft;
    let sender = sender(db, d.account_id, d.content.identity)?;
    Ok(sender.map(|sender| (snapshot, sender)))
}

/// Opens a new connection to the SMTP server that sends as `identity`, and logs in.
pub(crate) async fn connect_smtp(
    engine: &Arc<Engine>,
    account: AccountId,
    identity: IdentityId,
) -> Result<catmail_proto::smtp::Session> {
    let sender = engine
        .read(move |db| sender(db, account, Some(identity)))
        .await?
        .ok_or(Error::IdentityNotFound(identity))?;
    let password = password(engine, sender.login_key, &sender.from.email).await?;
    let credentials = Credentials {
        username: sender.username,
        password,
    };
    let session = smtp_login(engine, &sender.smtp, &credentials).await?;
    remember_limit(
        engine,
        sender.smtp_max_size,
        session.max_size(),
        move |db, limit| db.identities().set_smtp_max_size(identity, limit),
    )
    .await;
    Ok(session)
}

/// Opens a new connection to the SMTP server `server` and logs in.
pub(crate) async fn smtp_login(
    engine: &Engine,
    server: &ServerConfig,
    credentials: &Credentials,
) -> Result<catmail_proto::smtp::Session> {
    let mut session =
        catmail_proto::smtp::Session::connect(server, &engine.tls()?, IO_TIMEOUT).await?;
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
