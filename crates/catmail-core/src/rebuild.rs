//! Making stored bodies again once what parsing and sanitizing make of a message has
//! changed ([`mime::CONTENT_VERSION`]), so that, for one, stricter sanitizing reaches
//! bodies stored long before they are opened.
//!
//! A body kept with its raw message is parsed again from it: text, HTML, attachments,
//! snippet and index entry. One fetched in parts has no raw message; its HTML is
//! sanitized again (sanitizing is idempotent, so stricter rules apply), and its text
//! stays. What a looser rule would now keep is lost for it: fetching the body again
//! would cost the download budget and need the server, for the largest messages.
//!
//! Opening a message rebuilds its body first ([`on_open`]); the engine also rebuilds
//! every stale body in the background ([`run`]), for search.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use catmail_proto::mime::{self, CONTENT_VERSION};
use catmail_store::{Messages, NewBody, StaleBody};
use catmail_types::{AccountId, Event, FolderId, MessageId};

use crate::Result;
use crate::body::{new_body, stored};
use crate::engine::{Closing, Engine, blocking};

/// Bodies rebuilt in one transaction.
const BATCH: usize = 20;

/// A body made again, to be stored in place of the stale one.
struct Rebuilt {
    stale: StaleBody,
    made: Made,
}

enum Made {
    /// From the raw message.
    Whole(NewBody),
    /// The stored HTML sanitized again, and the Content-IDs its images refer to.
    Html(Option<String>, HashSet<String>),
}

impl Rebuilt {
    /// Returns false if the message is gone or was rebuilt meanwhile.
    fn store(&self, messages: &Messages<'_>) -> catmail_store::Result<bool> {
        let (id, from) = (self.stale.id, self.stale.version);
        match &self.made {
            Made::Whole(body) => messages.rebuild_body(id, from, body),
            Made::Html(html, images) => {
                messages.refresh_html(id, from, CONTENT_VERSION, html.as_deref(), |a| {
                    mime::is_inline(a.content_id.as_deref(), &a.mime_type, images)
                })
            }
        }
    }
}

/// Blocking. `None` if the message's body is gone or current.
fn make(engine: &Engine, id: MessageId) -> Result<Option<Rebuilt>> {
    let Some(stale) = engine
        .store
        .read(|db| db.messages().stale_body(id, CONTENT_VERSION))?
    else {
        return Ok(None);
    };
    let raw = match &stale.raw {
        Some(hash) => {
            let raw = stored(engine, hash)?;
            if raw.is_none() {
                tracing::warn!(%hash, %id, "raw message is missing; sanitizing the HTML again");
            }
            raw
        }
        None => None,
    };
    let made = match raw {
        // Not put in the blob store again: the row keeps referring to it.
        Some(raw) => Made::Whole(new_body(engine, mime::parse_message(&raw), None)?),
        None => match stale.html.as_deref().map(mime::sanitized_html) {
            Some((html, images)) => Made::Html(Some(html), images),
            None => Made::Html(None, HashSet::new()),
        },
    };
    Ok(Some(Rebuilt { stale, made }))
}

/// Rebuilds the body of a message being opened, which an older content version made.
/// A failure leaves the stored body, sanitized as it was before.
pub(crate) async fn on_open(engine: &Arc<Engine>, id: MessageId) {
    rebuild(engine, vec![id]).await;
}

/// Rebuilds every stale body, `delay` after the engine opened, batch by batch, holding
/// the engine only during a batch. Once is enough: whatever is stored meanwhile is made
/// by the current version.
pub(crate) async fn run(engine: Weak<Engine>, delay: Duration, mut closing: Closing) {
    tokio::select! {
        () = tokio::time::sleep(delay) => {}
        () = closing.closed() => return,
    }
    let Some(strong) = engine.upgrade() else {
        return;
    };
    let stale = match strong
        .read(|db| db.messages().stale_bodies(CONTENT_VERSION))
        .await
    {
        Ok(stale) => stale,
        Err(e) => {
            tracing::warn!(error = %e, "listing bodies to rebuild failed");
            return;
        }
    };
    drop(strong);
    if stale.is_empty() {
        return;
    }
    tracing::info!(
        bodies = stale.len(),
        version = CONTENT_VERSION,
        "rebuilding bodies made by an older content version"
    );
    let (mut rebuilt, mut failed) = (0, 0);
    for batch in stale.chunks(BATCH) {
        let Some(engine) = engine.upgrade() else {
            return;
        };
        let (stored, failures) = rebuild(&engine, batch.to_vec()).await;
        rebuilt += stored;
        failed += failures;
    }
    tracing::info!(rebuilt, failed, "rebuilt bodies");
}

/// Makes the bodies of `ids` again, skipping (and logging) those that fail, stores them
/// in one transaction, then announces the folders whose lists show them changed
/// (snippets and attachment markers). Returns how many were stored and how many failed.
async fn rebuild(engine: &Arc<Engine>, ids: Vec<MessageId>) -> (usize, usize) {
    let count = ids.len();
    let done = blocking({
        let engine = engine.clone();
        move || {
            let mut failed = 0;
            let made: Vec<Rebuilt> = ids
                .iter()
                .filter_map(|&id| match make(&engine, id) {
                    Ok(made) => made,
                    Err(e) => {
                        tracing::warn!(%id, error = %e, "rebuilding the body failed");
                        failed += 1;
                        None
                    }
                })
                .collect();
            let changed = engine.store.write(|db| {
                let messages = db.messages();
                let mut changed = BTreeSet::<(AccountId, FolderId)>::new();
                let mut stored = 0;
                for rebuilt in &made {
                    if rebuilt.store(&messages)? {
                        // Its location, and on Gmail the labels (and Starred) showing it.
                        let account = rebuilt.stale.account;
                        changed.insert((account, rebuilt.stale.folder));
                        let labels = messages.touched_labels();
                        changed.extend(labels.into_iter().map(|folder| (account, folder)));
                        stored += 1;
                    }
                }
                Ok((changed, stored))
            })?;
            Ok((changed, failed))
        }
    })
    .await;
    match done {
        Ok(((changed, stored), failed)) => {
            for (account, folder) in changed {
                engine.emit(Event::MessagesChanged { account, folder });
            }
            (stored, failed)
        }
        Err(e) => {
            tracing::warn!(error = %e, "storing rebuilt bodies failed");
            (0, count)
        }
    }
}
