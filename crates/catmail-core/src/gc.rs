//! Deleting stored bodies and attachments nothing refers to any more: on request
//! ([`MailClient::collect_garbage`](crate::MailClient::collect_garbage)), and now and then
//! in the background for long-running frontends, which keep the data directory locked
//! so that nothing else can do it.

use std::sync::Weak;
use std::time::Duration;

use catmail_types::GcReport;

use crate::Result;
use crate::engine::{Closing, Engine, blocking};

/// How long an unreferenced blob survives. Far longer than the gap between storing a
/// blob and committing the row that refers to it, which is what GC must not cut into.
const BLOB_GRACE: Duration = Duration::from_secs(60 * 60);

/// What long-running frontends pass to
/// [`collect_garbage_every`](crate::MailClientBuilder::collect_garbage_every).
pub const DEFAULT_GC_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// The first background collection waits at most this long after the engine opens:
/// not at once, where it would compete with the first sync, but before a frontend
/// opened only briefly is closed again.
const FIRST_DELAY: Duration = Duration::from_secs(5 * 60);

/// Blocking.
pub(crate) fn collect(engine: &Engine) -> Result<GcReport> {
    // References first: see BlobStore::collect_garbage.
    let referenced = engine.store.read(|db| db.blob_references())?;
    Ok(engine.blobs.collect_garbage(&referenced, BLOB_GRACE)?)
}

/// Collects every `every` for as long as the engine exists, holding it only while
/// collecting.
pub(crate) async fn run(engine: Weak<Engine>, every: Duration, mut closing: Closing) {
    let mut wait = every.min(FIRST_DELAY);
    loop {
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = closing.closed() => return,
        }
        let Some(engine) = engine.upgrade() else {
            return;
        };
        match blocking(move || collect(&engine)).await {
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
