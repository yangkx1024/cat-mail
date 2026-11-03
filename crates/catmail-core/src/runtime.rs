use std::future::Future;
use std::sync::Mutex;

use tokio::runtime::{Handle, Runtime as TokioRuntime};
use tokio::task::JoinError;

use crate::engine::lock;
use crate::{Error, Result};

/// The tokio runtime engine work runs on: either owned by the client (GUI, where the UI
/// executor is not tokio) or borrowed from the caller (CLI, already inside tokio).
pub(crate) struct Runtime {
    /// Taken when shut down.
    owned: Mutex<Option<TokioRuntime>>,
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
            owned: Mutex::new(Some(rt)),
        })
    }

    pub(crate) fn shared(handle: Handle) -> Self {
        Self {
            owned: Mutex::new(None),
            handle,
        }
    }

    /// Runs `fut` on the engine runtime. The returned future can be polled by any executor.
    pub(crate) async fn run<F, T>(&self, fut: F) -> Result<T>
    where
        F: Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        self.handle.spawn(fut).await.map_err(run_error)?
    }

    /// Starts `fut` on the engine runtime and leaves it running.
    pub(crate) fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.handle.spawn(fut);
    }

    /// Runs blocking work (SQLite, keyring) on the runtime's blocking pool.
    pub(crate) async fn run_blocking<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.handle.spawn_blocking(f).await.map_err(run_error)?
    }

    /// Shuts the runtime down if it is the client's own: the tasks on it are dropped at
    /// their next await, and the work they hold with them. Blocking work that started
    /// runs to its end. A borrowed runtime is left alone.
    pub(crate) fn shut_down(&self) {
        // A plain drop would block until every task finishes and panics inside async
        // contexts; the engine's state is persisted in SQLite, so don't wait.
        if let Some(rt) = lock(&self.owned).take() {
            rt.shutdown_background();
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// Why a call's task on the runtime ended without its result. Nothing aborts those: one
/// cancelled was dropped as the runtime shut down.
fn run_error(e: JoinError) -> Error {
    if e.is_cancelled() {
        Error::Closed
    } else {
        join_error(e)
    }
}

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
