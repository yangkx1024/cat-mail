//! Deleting stored bodies and attachments nothing refers to any more: on request
//! ([`MailClient::collect_garbage`](crate::MailClient::collect_garbage)), and now and then
//! in the background for long-running frontends, which keep the data directory locked
//! so that nothing else can do it.

use std::sync::Weak;
use std::time::Duration;

use crate::engine::Engine;
use crate::task::{Closing, blocking};

/// What long-running frontends pass to
/// [`collect_garbage_every`](crate::MailClientBuilder::collect_garbage_every).
pub(crate) const DEFAULT_GC_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// The first background collection waits at most this long after the engine opens:
/// not at once, where it would compete with the first sync, but before a frontend
/// opened only briefly is closed again.
const FIRST_DELAY: Duration = Duration::from_secs(5 * 60);

/// Collects every `every` for as long as the engine exists, holding its storage only
/// while collecting.
pub(crate) async fn run(engine: Weak<Engine>, every: Duration, mut closing: Closing) {
    let mut wait = every.min(FIRST_DELAY);
    loop {
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = closing.closed() => return,
        }
        let Some(storage) = engine.upgrade().map(|e| e.storage()) else {
            return;
        };
        match blocking(move || storage.collect_garbage()).await {
            Ok(report) if report.removed > 0 => {
                tracing::info!(
                    removed = report.removed,
                    bytes = report.bytes,
                    "removed unreferenced blobs"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "collecting unreferenced blobs failed"),
        }
        wait = every;
    }
}
