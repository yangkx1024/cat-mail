//! Logged-in IMAP connections for an account.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use catmail_proto::imap::Session;
use catmail_types::AccountId;

use crate::engine::{Engine, blocking};
use crate::{Error, Result, auth};

/// Per network read or write, not per command.
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) type SessionFuture<'s, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 's>>;

/// Opens a new connection to the account's IMAP server and logs in.
pub(crate) async fn connect(engine: &Arc<Engine>, account_id: AccountId) -> Result<Session> {
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
    let mut session = Session::connect(&account.imap, &engine.tls()?, IO_TIMEOUT).await?;
    session.login(&credentials).await?;
    Ok(session)
}

/// Runs `op` on one of the account's idle connections, or a new one, and keeps the
/// connection for next time unless it failed. If a reused connection turns out to be
/// dead, `op` runs once more on a fresh one.
pub(crate) async fn with_session<T>(
    engine: &Arc<Engine>,
    account: AccountId,
    mut op: impl for<'s> FnMut(&'s mut Session) -> SessionFuture<'s, T>,
) -> Result<T> {
    let mut retried = false;
    loop {
        // After a dead idle connection, the others in the pool are suspect too.
        let pooled = if retried {
            None
        } else {
            engine.take_session(account)
        };
        let (mut session, reused) = match pooled {
            Some(session) => (session, true),
            None => (connect(engine, account).await?, false),
        };
        match op(&mut session).await {
            Err(Error::Imap(e)) if e.is_fatal() => {
                if reused && !retried {
                    tracing::debug!(%account, error = %e, "idle connection was dead; reconnecting");
                    retried = true;
                    continue;
                }
                return Err(Error::Imap(e));
            }
            // Anything else leaves the connection in a known state.
            result => {
                engine.return_session(account, session);
                return result;
            }
        }
    }
}
