use std::future::Future;

use tokio::runtime::{Handle, Runtime as TokioRuntime};
use tokio::task::JoinError;

use crate::{Error, Result};

/// The tokio runtime engine work runs on: either owned by the client (GUI, where the UI
/// executor is not tokio) or borrowed from the caller (CLI, already inside tokio).
pub(crate) struct Runtime {
    owned: Option<TokioRuntime>,
    handle: Handle,
}

impl Runtime {
    pub(crate) fn owned() -> Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .thread_name("catmail-engine")
            .enable_all()
            .build()?;
        Ok(Self {
            handle: rt.handle().clone(),
            owned: Some(rt),
        })
    }

    pub(crate) fn shared(handle: Handle) -> Self {
        Self {
            owned: None,
            handle,
        }
    }

    /// Runs `fut` on the engine runtime. The returned future can be polled by any executor.
    #[expect(dead_code, reason = "used by the IMAP sync engine from M1 on")]
    pub(crate) async fn run<F, T>(&self, fut: F) -> Result<T>
    where
        F: Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        self.handle.spawn(fut).await.map_err(join_error)?
    }

    /// Runs blocking work (SQLite, keyring) on the runtime's blocking pool.
    pub(crate) async fn run_blocking<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.handle.spawn_blocking(f).await.map_err(join_error)?
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // A plain drop would block until every task finishes and panics inside async
        // contexts; the engine's state is persisted in SQLite, so don't wait.
        if let Some(rt) = self.owned.take() {
            rt.shutdown_background();
        }
    }
}

fn join_error(e: JoinError) -> Error {
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
