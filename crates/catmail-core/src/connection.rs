//! Logged-in IMAP (and SMTP) connections for an account.

use std::sync::Arc;
use std::time::Duration;

use catmail_proto::Credentials;
use catmail_proto::imap::Session;
use catmail_types::{Account, AccountId, ServerConfig};

use crate::engine::{Engine, blocking};
use crate::error::ImapError;
use crate::{Error, Result, auth};

/// Per network read or write, not per command.
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(60);

/// Opens a new connection to the account's IMAP server and logs in.
pub(crate) async fn connect(engine: &Arc<Engine>, account_id: AccountId) -> Result<Session> {
    let (account, credentials) = account_credentials(engine, account_id).await?;
    login(engine, &account.imap, &credentials).await
}

/// Opens a new connection to the account's SMTP server and logs in.
pub(crate) async fn connect_smtp(
    engine: &Arc<Engine>,
    account_id: AccountId,
) -> Result<catmail_proto::smtp::Session> {
    let (account, credentials) = account_credentials(engine, account_id).await?;
    let mut session =
        catmail_proto::smtp::Session::connect(&account.smtp, &engine.tls()?, IO_TIMEOUT).await?;
    session.login(&credentials).await?;
    Ok(session)
}

async fn account_credentials(
    engine: &Arc<Engine>,
    account_id: AccountId,
) -> Result<(Account, Credentials)> {
    let (account, secret_key) = engine
        .read(move |db| {
            let accounts = db.accounts();
            Ok(accounts
                .get(account_id)?
                .zip(accounts.secret_key(account_id)?))
        })
        .await?
        .ok_or_else(|| Error::AccountNotFound(account_id.to_string()))?;
    let credentials = {
        let (engine, account) = (engine.clone(), account.clone());
        blocking(move || auth::credentials(&account, &secret_key, engine.secrets.as_ref())).await?
    };
    Ok((account, credentials))
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
