//! What engine work is made of, below the engine: the signals that stop it, running
//! blocking work, and time. Nothing here knows the engine, so everything can use it.

use std::future::Future;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use catmail_types::AccountId;
use tokio::sync::watch;
use tokio::task::JoinError;

use crate::{Error, Result};

/// Tells a task holding the engine weakly that the engine is gone.
#[derive(Clone)]
pub(crate) struct Closing(watch::Receiver<()>);

impl Closing {
    /// From the engine's sender, which is never sent to: it is dropped with the engine.
    pub fn new(receiver: watch::Receiver<()>) -> Self {
        Self(receiver)
    }

    pub async fn closed(&mut self) {
        // Nothing is ever sent: this ends when the sender, owned by the engine, is gone.
        let _ = self.0.changed().await;
    }
}

/// Tells work on an account that the account is being removed.
pub(crate) struct Cancellation {
    removed: watch::Receiver<bool>,
    account: AccountId,
}

impl Cancellation {
    /// From the account's sender, set when its removal starts and dropped with the
    /// engine.
    pub fn new(removed: watch::Receiver<bool>, account: AccountId) -> Self {
        Self { removed, account }
    }

    pub fn is_cancelled(&self) -> bool {
        *self.removed.borrow()
    }

    /// Runs `work` on the account. Fails with [`Error::AccountNotFound`] if the account
    /// was removed before, and stops it with [`Error::Cancelled`] if it is removed
    /// meanwhile.
    pub async fn run<T>(mut self, work: impl Future<Output = Result<T>>) -> Result<T> {
        if self.is_cancelled() {
            return Err(Error::AccountNotFound(self.account.to_string()));
        }
        tokio::select! {
            result = work => result,
            () = self.cancelled() => Err(Error::Cancelled),
        }
    }

    /// Resolves once the account is being removed (at once if it already is), or the
    /// engine is gone: work holding the engine only weakly stops then too.
    pub async fn cancelled(&mut self) {
        // Err: the sender, owned by the engine, is gone.
        let _ = self.removed.wait_for(|c| *c).await;
    }
}

/// A panic while holding one of these locks can't leave the data inconsistent.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs blocking work (SQLite, keyring) on the current runtime's blocking pool.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(join_error)?
}

/// A task that ended without its result: it panicked, or was aborted.
pub(crate) fn join_error(e: JoinError) -> Error {
    if e.is_panic() {
        let payload = e.into_panic();
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        Error::Task(format!("panicked: {msg}"))
    } else {
        Error::Task(e.to_string())
    }
}

/// `first`, doubled for each failure in a row after the first, up to `max`.
pub(crate) fn backoff(first: Duration, failures: u32, max: Duration) -> Duration {
    first
        .saturating_mul(1 << failures.saturating_sub(1).min(16))
        .min(max)
}

/// Now, in unix seconds.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
