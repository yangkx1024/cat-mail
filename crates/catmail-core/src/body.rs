//! On-demand download of message bodies and attachments.
//!
//! A message up to [`WHOLE_MESSAGE_LIMIT`] is fetched whole (`BODY.PEEK[]`), kept as a
//! raw blob, and its attachments stored at once. A larger one is fetched as its
//! BODYSTRUCTURE plus the text parts; each attachment is fetched by section when it is
//! first opened. Either way the mailbox is opened with EXAMINE and bodies are fetched
//! with `.PEEK`, so nothing changes on the server, and the stored UIDVALIDITY is checked
//! before any UID is used.
//!
//! Results are written only if the message row still exists. Message ids are never
//! reused and a UIDVALIDITY change deletes the rows of the old generation, so an
//! existing row is still the same (folder, UID) as when the download started.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use catmail_proto::imap::{Error as ImapError, Session};
use catmail_proto::mime::{self, Content};
use catmail_store::{MessageLocation, NewAttachment, NewBody};
use catmail_types::{AttachmentId, BlobHash, Event, Message, MessageId};

use crate::connection::with_session;
use crate::engine::{Engine, blocking};
use crate::{Error, Result};

/// Messages up to this size (RFC822.SIZE) are fetched whole; above it, attachments are
/// left on the server until opened. Text parts above it are not fetched at all.
pub(crate) const WHOLE_MESSAGE_LIMIT: u32 = 2 * 1024 * 1024;

pub(crate) async fn download_message(engine: Arc<Engine>, id: MessageId) -> Result<Message> {
    let location = locate(&engine, id).await?;
    if !location.downloaded {
        let mut cancellation = engine.cancellation(location.account);
        // Again, now that a removal of the account would be noticed.
        let location = locate(&engine, id).await?;
        if !location.downloaded {
            tokio::select! {
                result = fetch_and_store(&engine, location) => result?,
                () = cancellation.cancelled() => return Err(Error::Cancelled),
            }
        }
    }
    engine
        .read(move |db| db.messages().get(id))
        .await?
        .ok_or(Error::MessageNotFound(id))
}

/// The decoded content, downloading it first if needed.
pub(crate) async fn attachment_content(engine: Arc<Engine>, id: AttachmentId) -> Result<Vec<u8>> {
    let source = || {
        let engine = engine.clone();
        async move {
            engine
                .read(move |db| db.attachments().source(id))
                .await?
                .ok_or(Error::AttachmentNotFound(id))
        }
    };
    let first = source().await?;
    if let Some(blob) = &first.blob
        && let Some(data) = read_blob(&engine, blob.clone()).await?
    {
        return Ok(data);
    }
    let mut cancellation = engine.cancellation(first.message.account);
    let source = source().await?;
    let location = source.message.clone();
    let section = source.section.clone();
    let encoded = tokio::select! {
        result = with_session(&engine, location.account, move |session| {
            let (location, section) = (location.clone(), section.clone());
            Box::pin(async move {
                examine(session, &location).await?;
                session
                    .fetch_sections(location.uid, std::slice::from_ref(&section))
                    .await?
                    .and_then(|mut parts| parts.remove(&section))
                    .ok_or(GONE)
            })
        }) => result?,
        () = cancellation.cancelled() => return Err(Error::Cancelled),
    };
    let engine2 = engine.clone();
    blocking(move || {
        let data = mime::decode_transfer(source.transfer_encoding.as_deref(), &encoded);
        let hash = engine2.blobs.put_bytes(&data)?;
        let size = data.len() as u32;
        if !engine2
            .store
            .write(|db| db.attachments().set_blob(id, &hash, size))?
        {
            return Err(Error::AttachmentNotFound(id));
        }
        Ok(data)
    })
    .await
}

/// The raw message, fetching it if only its parts were downloaded.
pub(crate) async fn raw_message(engine: Arc<Engine>, id: MessageId) -> Result<Vec<u8>> {
    download_message(engine.clone(), id).await?;
    let location = locate(&engine, id).await?;
    if let Some(raw) = &location.raw
        && let Some(data) = read_blob(&engine, raw.clone()).await?
    {
        return Ok(data);
    }
    let mut cancellation = engine.cancellation(location.account);
    let account = location.account;
    let raw = tokio::select! {
        result = with_session(&engine, account, move |session| {
            let location = location.clone();
            Box::pin(async move {
                examine(session, &location).await?;
                session.fetch_raw(location.uid).await?.ok_or(GONE)
            })
        }) => result?,
        () = cancellation.cancelled() => return Err(Error::Cancelled),
    };
    let engine2 = engine.clone();
    blocking(move || {
        let hash = engine2.blobs.put_bytes(&raw)?;
        if !engine2.store.write(|db| db.messages().set_raw(id, &hash))? {
            return Err(Error::MessageNotFound(id));
        }
        Ok(raw)
    })
    .await
}

const GONE: Error = Error::OutOfSync("the message is no longer on the server");

enum Fetched {
    Whole(Vec<u8>),
    Parts(mime::MimeNode, HashMap<String, Vec<u8>>),
}

async fn fetch_and_store(engine: &Arc<Engine>, location: MessageLocation) -> Result<()> {
    let (id, account, folder) = (location.id, location.account, location.folder);
    let fetched = with_session(engine, account, move |session| {
        let location = location.clone();
        Box::pin(async move { fetch(session, &location).await })
    })
    .await?;

    let engine2 = engine.clone();
    let stored = blocking(move || {
        let (content, raw) = match fetched {
            Fetched::Whole(raw) => (mime::parse_message(&raw), Some(raw)),
            Fetched::Parts(structure, parts) => (structure.content(&parts), None),
        };
        let body = new_body(&engine2, content, raw.as_deref())?;
        if engine2
            .store
            .write(|db| db.messages().store_body(id, &body))?
        {
            return Ok(true);
        }
        // Not stored: a concurrent download stored it first, or the message is gone.
        let location = engine2.store.read(|db| db.messages().location(id))?;
        match location {
            Some(location) if location.downloaded => Ok(false),
            _ => Err(Error::MessageNotFound(id)),
        }
    })
    .await?;
    if stored {
        // Snippet and attachment marker changed.
        engine.emit(Event::MessagesChanged { account, folder });
    }
    Ok(())
}

async fn fetch(session: &mut Session, location: &MessageLocation) -> Result<Fetched> {
    examine(session, location).await?;
    let uid = location.uid;
    if location.size.is_some_and(|size| size > WHOLE_MESSAGE_LIMIT) {
        match session.fetch_structure(uid).await {
            Ok(Some(structure)) => {
                let sections = structure.text_sections(WHOLE_MESSAGE_LIMIT);
                let parts = if sections.is_empty() {
                    HashMap::new()
                } else {
                    session.fetch_sections(uid, &sections).await?.ok_or(GONE)?
                };
                // A body stored without some of its text would never be fetched again.
                if sections.iter().all(|s| parts.contains_key(s)) {
                    return Ok(Fetched::Parts(structure, parts));
                }
                tracing::warn!(
                    uid,
                    "server left out text parts; fetching the whole message"
                );
            }
            Ok(None) => return Err(GONE),
            // Some servers send BODYSTRUCTUREs that can't be parsed; the raw message
            // can always be.
            Err(ImapError::Incomplete(_)) => {
                tracing::warn!(uid, "unparsable BODYSTRUCTURE; fetching the whole message");
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Fetched::Whole(session.fetch_raw(uid).await?.ok_or(GONE)?))
}

/// Opens the message's folder read-only and makes sure its UIDs still mean what they
/// meant when the message was stored.
async fn examine(session: &mut Session, location: &MessageLocation) -> Result<()> {
    let status = session.examine(&location.remote_name).await?;
    if Some(status.uidvalidity) != location.uidvalidity {
        return Err(Error::OutOfSync(
            "the folder was rebuilt on the server (UIDVALIDITY changed)",
        ));
    }
    Ok(())
}

/// Stores the raw message and every part that came with it in the blob store.
fn new_body(engine: &Engine, content: Content, raw: Option<&[u8]>) -> Result<NewBody> {
    let raw = raw.map(|raw| engine.blobs.put_bytes(raw)).transpose()?;
    let attachments = content
        .parts
        .into_iter()
        .map(|part| {
            let blob = part
                .data
                .as_deref()
                .map(|data| engine.blobs.put_bytes(data))
                .transpose()?;
            Ok(NewAttachment {
                section: part.section,
                transfer_encoding: part.transfer_encoding,
                filename: part.filename,
                mime_type: part.mime_type,
                size: part.size,
                content_id: part.content_id,
                inline: part.inline,
                blob,
            })
        })
        .collect::<Result<_>>()?;
    Ok(NewBody {
        text: content.text,
        html: content.html,
        raw,
        snippet: content.snippet,
        attachments,
    })
}

async fn locate(engine: &Arc<Engine>, id: MessageId) -> Result<MessageLocation> {
    engine
        .read(move |db| db.messages().location(id))
        .await?
        .ok_or(Error::MessageNotFound(id))
}

/// `None` if the blob has gone missing from disk, so the caller fetches it again.
async fn read_blob(engine: &Arc<Engine>, hash: BlobHash) -> Result<Option<Vec<u8>>> {
    let engine = engine.clone();
    blocking(move || match engine.blobs.open_blob(&hash) {
        Ok(mut file) => {
            let mut data = Vec::new();
            file.read_to_end(&mut data)?;
            Ok(Some(data))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(%hash, "referenced blob is missing; fetching again");
            Ok(None)
        }
        Err(e) => Err(e.into()),
    })
    .await
}
