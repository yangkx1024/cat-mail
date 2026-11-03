//! On-demand download of message bodies and attachments.
//!
//! A message up to the whole-message limit (see
//! [`EngineTuning`](crate::EngineTuning)) is fetched whole (`BODY.PEEK[]`), kept as a
//! raw blob, and its attachments stored at once. A larger one is fetched as its
//! BODYSTRUCTURE plus the text parts; each attachment is fetched by section when it is
//! first opened. Either way the mailbox is opened with EXAMINE (unless the connection has
//! it open so already) and bodies are fetched with `.PEEK`, so nothing changes on the
//! server, and the stored UIDVALIDITY is checked before any UID is used.
//!
//! The background prefetch (`prefetch`) fetches bodies the same way ([`fetch_body`]), on a
//! connection of its own.
//!
//! Results are written only if the message row still exists. Message ids are never
//! reused and a UIDVALIDITY change deletes the rows of the old generation, so an
//! existing row is still the same (folder, UID) as when the download started.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use catmail_proto::imap::{self, Session};
use catmail_proto::mime::{self, Content};
use catmail_store::{AttachmentSource, Db, MessageLocation, NewAttachment, NewBody};
use catmail_types::{AttachmentId, BlobHash, Event, Message, MessageId};

use crate::engine::{Engine, blocking};
use crate::pool::with_session;
use crate::rebuild;
use crate::{Error, Result};

const GONE: Error = Error::OutOfSync("the message is no longer on the server");
const REBUILT: &str = "the folder was rebuilt on the server (UIDVALIDITY changed)";

/// Whether `e` says the message's folder has a new UIDVALIDITY on the server: none of its
/// messages can be fetched until a sync of it starts over.
pub(crate) fn folder_rebuilt(e: &Error) -> bool {
    matches!(e, Error::OutOfSync(text) if *text == REBUILT)
}

/// How large a message may be (RFC822.SIZE) to be fetched whole, and a text part of a
/// larger one to be fetched at all (a larger part is listed among the attachments).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub whole: u32,
    pub text: u32,
}

impl Limits {
    /// As for a message the user opens.
    fn opened(engine: &Engine) -> Self {
        let limit = engine.tuning.whole_message_limit;
        Self {
            whole: limit,
            text: limit,
        }
    }

    /// Whole, however large: for the raw message.
    const RAW: Self = Self {
        whole: u32::MAX,
        text: u32::MAX,
    };
}

pub(crate) async fn download_message(engine: Arc<Engine>, id: MessageId) -> Result<Message> {
    ensure_downloaded(&engine, id, false).await?;
    engine
        .read(move |db| db.messages().get(id))
        .await?
        .ok_or(Error::MessageNotFound(id))
}

/// The decoded content, downloading it first if needed.
pub(crate) async fn attachment_content(engine: Arc<Engine>, id: AttachmentId) -> Result<Vec<u8>> {
    let source = attachment_source(&engine, id).await?;
    if let Some(blob) = &source.blob
        && let Some(data) = read_blob(&engine, blob.clone()).await?
    {
        return Ok(data);
    }
    let section = source.section.clone();
    let encoded = on_server(
        &engine,
        source.message.clone(),
        async move |session, location| {
            Ok(session
                .fetch_sections(location.uid, std::slice::from_ref(&section))
                .await?
                .and_then(|mut parts| parts.remove(&section)))
        },
    )
    .await?;
    let (account, folder) = (source.message.account, source.message.folder);
    let data = blocking({
        let engine = engine.clone();
        move || {
            let data = mime::decode_transfer(source.transfer_encoding.as_deref(), &encoded);
            drop(encoded);
            let size = data.len() as u32;
            store_blob(
                &engine,
                &data,
                |db, hash| db.attachments().set_blob(id, hash, size),
                Error::AttachmentNotFound(id),
            )?;
            Ok(data)
        }
    })
    .await?;
    // Stored now, with its size as decoded.
    engine.emit(Event::MessagesChanged { account, folder });
    Ok(data)
}

/// The attachment's blob and size, downloading it first if needed. Its GC grace period
/// restarts, as with `put`, so a row referring to it may follow.
pub(crate) async fn attachment_blob(
    engine: Arc<Engine>,
    id: AttachmentId,
) -> Result<(BlobHash, u64)> {
    if let Some(stored) = stored_blob(&engine, id).await? {
        return Ok(stored);
    }
    attachment_content(engine.clone(), id).await?;
    stored_blob(&engine, id)
        .await?
        .ok_or(Error::AttachmentNotFound(id))
}

async fn stored_blob(engine: &Arc<Engine>, id: AttachmentId) -> Result<Option<(BlobHash, u64)>> {
    let Some(blob) = attachment_source(engine, id).await?.blob else {
        return Ok(None);
    };
    let engine = engine.clone();
    blocking(move || Ok(engine.blobs.refresh(&blob)?.map(|size| (blob, size)))).await
}

/// The raw message, fetching it if only its parts were downloaded.
pub(crate) async fn raw_message(engine: Arc<Engine>, id: MessageId) -> Result<Vec<u8>> {
    // Not downloaded yet: fetch it whole, which stores the raw message too.
    ensure_downloaded(&engine, id, true).await?;
    let location = locate(&engine, id).await?;
    if let Some(raw) = &location.raw
        && let Some(data) = read_blob(&engine, raw.clone()).await?
    {
        return Ok(data);
    }
    let (account, folder) = (location.account, location.folder);
    let raw = on_server(&engine, location, async move |session, location| {
        Ok(session.fetch_raw(location.uid).await?)
    })
    .await?;
    let raw = blocking({
        let engine = engine.clone();
        move || {
            store_blob(
                &engine,
                &raw,
                |db, hash| db.messages().set_raw(id, hash),
                Error::MessageNotFound(id),
            )?;
            Ok(raw)
        }
    })
    .await?;
    engine.emit(Event::MessagesChanged { account, folder });
    Ok(raw)
}

/// Runs `op` against the message's folder on the server (see [`in_folder`]), on one of
/// the account's pooled connections, cancelled if the account is removed meanwhile. Every
/// on-demand fetch goes through here.
///
/// The caller looked up `location` before calling, and the cancellation is obtained
/// after that: a removal that started earlier left the account marked removed, which
/// cancels at once, and one that starts later is noticed.
async fn on_server<T>(
    engine: &Arc<Engine>,
    location: MessageLocation,
    op: impl AsyncFn(&mut Session, &MessageLocation) -> Result<Option<T>>,
) -> Result<T> {
    let mut cancellation = engine.cancellation(location.account);
    let account = location.account;
    let owned = engine.clone();
    let began = Instant::now();
    tokio::select! {
        result = with_session(engine, account, async move |session| {
            // What the user waits for when a message opens: a connection (a new one
            // logs in, which is logged too), opening the folder unless it is open, then
            // the fetch.
            let connected = began.elapsed();
            let (done, took) = in_folder(&owned, session, &location, &op).await?;
            tracing::info!(%account, folder = %location.remote_name, uid = location.uid,
                ?connected, examine = ?took.examine, fetch = ?took.fetch,
                "fetched from the server");
            Ok(done)
        }) => result,
        () = cancellation.cancelled() => Err(Error::Cancelled),
    }
}

/// How long [`in_folder`] took to open the folder, if it did, and to fetch.
pub(crate) struct Took {
    pub examine: Option<Duration>,
    pub fetch: Duration,
}

/// Runs `op` on `session` once the message's folder is open with the UIDVALIDITY
/// confirmed (see [`examine`]).
///
/// `op` returns `None` if the server sent nothing of what it asked for. A folder that was
/// open already is opened again and `op` tried once more: this connection's view of it
/// may be older than the message, whatever [`examine`] made of it. If a UID SEARCH
/// then finds no such UID, the message is gone and the local copy is deleted, as the
/// next sync of the folder would: until then it would stay in the list (and in search,
/// from another Gmail label), failing to open. Otherwise the server left it out: a
/// protocol error, which drops the connection and its view of the folder.
async fn in_folder<T>(
    engine: &Arc<Engine>,
    session: &mut Session,
    location: &MessageLocation,
    op: &impl AsyncFn(&mut Session, &MessageLocation) -> Result<Option<T>>,
) -> Result<(T, Took)> {
    let account = location.account;
    let mut opened = examine(session, location).await?;
    let mut fetched = Instant::now();
    let mut done = op(session, location).await?;
    if done.is_none() && opened.is_none() {
        tracing::info!(%account, folder = %location.remote_name, uid = location.uid,
            "nothing from the folder as it was open; opening it again");
        opened = Some(reopen(session, location).await?);
        fetched = Instant::now();
        done = op(session, location).await?;
    }
    if let Some(done) = done {
        let took = Took {
            examine: opened,
            fetch: fetched.elapsed(),
        };
        return Ok((done, took));
    }
    // Gone, or only left out of the answer?
    if session.uid_search_uids(&[location.uid]).await?.is_empty() {
        forget(engine, location.clone()).await?;
        return Err(GONE);
    }
    Err(imap::Error::Protocol(format!(
        "the server has the message (UID {}) but left it out of its answer",
        location.uid
    ))
    .into())
}

/// Deletes the local copy of a message the server no longer has at `location`.
async fn forget(engine: &Arc<Engine>, location: MessageLocation) -> Result<()> {
    let (account, folder) = (location.account, location.folder);
    let deleted = engine
        .write(move |db| db.messages().delete_gone(&location))
        .await?;
    if deleted {
        engine.emit(Event::MessagesChanged { account, folder });
    }
    Ok(())
}

/// A body as fetched, to be parsed with [`parse`].
pub(crate) enum Fetched {
    Whole(Vec<u8>),
    Parts(Box<Parts>),
}

impl Fetched {
    /// The bytes that came from the server, about.
    pub fn len(&self) -> u64 {
        match self {
            Self::Whole(raw) => raw.len() as u64,
            Self::Parts(parts) => parts
                .text
                .values()
                .chain(parts.headers.values())
                .map(|data| data.len() as u64)
                .sum(),
        }
    }
}

/// A message fetched in parts, by section: its text and the MIME headers of the parts
/// whose names lost bytes in the structure.
pub(crate) struct Parts {
    structure: mime::MimeNode,
    text: HashMap<String, Vec<u8>>,
    headers: HashMap<String, Vec<u8>>,
}

/// Downloads and stores the body unless it is stored already. `whole` fetches even a
/// large message whole, for its raw form.
async fn ensure_downloaded(engine: &Arc<Engine>, id: MessageId, whole: bool) -> Result<()> {
    let location = locate(engine, id).await?;
    if location.downloaded {
        if location
            .body_version
            .is_some_and(|v| v < mime::CONTENT_VERSION)
        {
            rebuild::on_open(engine, id).await;
        }
        return Ok(());
    }
    let (account, folder) = (location.account, location.folder);
    let limits = if whole {
        Limits::RAW
    } else {
        Limits::opened(engine)
    };
    let fetched = on_server(engine, location, async move |session, location| {
        fetch(session, location, limits).await
    })
    .await?;

    let stored = blocking({
        let engine = engine.clone();
        move || {
            let body = parse(&engine, fetched)?;
            let labels = engine.store.write(|db| {
                let messages = db.messages();
                Ok(messages
                    .store_body(id, &body)?
                    .then(|| messages.touched_labels()))
            })?;
            if labels.is_some() {
                return Ok(labels);
            }
            // Not stored: a concurrent download stored it first, or the message is gone.
            let location = engine.store.read(|db| db.messages().location(id))?;
            match location {
                Some(location) if location.downloaded => Ok(None),
                _ => Err(Error::MessageNotFound(id)),
            }
        }
    })
    .await?;
    if let Some(labels) = stored {
        // Snippet and attachment marker changed, where it is and on Gmail where it shows.
        for folder in labels.into_iter().chain([folder]) {
            engine.emit(Event::MessagesChanged { account, folder });
        }
    }
    Ok(())
}

/// Fetches the message's body on `session` (see [`in_folder`]), for the background
/// prefetch, which has a connection of its own.
pub(crate) async fn fetch_body(
    engine: &Arc<Engine>,
    session: &mut Session,
    location: &MessageLocation,
    limits: Limits,
) -> Result<(Fetched, Took)> {
    let op = async move |session: &mut Session, location: &MessageLocation| {
        fetch(session, location, limits).await
    };
    in_folder(engine, session, location, &op).await
}

/// Parses a fetched body into what is stored, putting the raw message and the parts
/// that came with it in the blob store. Blocking.
pub(crate) fn parse(engine: &Engine, fetched: Fetched) -> Result<NewBody> {
    let (content, raw) = match fetched {
        Fetched::Whole(raw) => (mime::parse_message(&raw), Some(raw)),
        Fetched::Parts(parts) => (parts.structure.content(&parts.text, &parts.headers), None),
    };
    new_body(engine, content, raw.as_deref())
}

/// Runs after [`examine`] (see [`in_folder`]). Fetches a message larger than
/// `limits.whole` in parts. `None` if the message is gone.
async fn fetch(
    session: &mut Session,
    location: &MessageLocation,
    limits: Limits,
) -> Result<Option<Fetched>> {
    let uid = location.uid;
    if location.size.is_some_and(|size| size > limits.whole) {
        match session.fetch_structure(uid).await {
            Ok(Some(structure)) => {
                let sections = structure.text_sections(limits.text);
                let Some(parts) = session.fetch_sections(uid, &sections).await? else {
                    return Ok(None);
                };
                // A body stored without some of its text would never be fetched again.
                if sections.iter().all(|s| parts.contains_key(s)) {
                    let Some(headers) = mime_headers(session, uid, &structure).await? else {
                        return Ok(None);
                    };
                    return Ok(Some(Fetched::Parts(Box::new(Parts {
                        structure,
                        text: parts,
                        headers,
                    }))));
                }
                tracing::warn!(
                    uid,
                    "server left out text parts; fetching the whole message"
                );
            }
            Ok(None) => return Ok(None),
            // Some servers send BODYSTRUCTUREs that can't be parsed; the raw message
            // can always be.
            Err(imap::Error::Incomplete(_)) => {
                tracing::warn!(uid, "unparsable BODYSTRUCTURE; fetching the whole message");
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(session.fetch_raw(uid).await?.map(Fetched::Whole))
}

/// The MIME headers of the parts whose file names lost bytes in the BODYSTRUCTURE (see
/// [`garbled_sections`](mime::MimeNode::garbled_sections)), so they can be read as in a
/// whole message. `None` if the message is gone.
async fn mime_headers(
    session: &mut Session,
    uid: u32,
    structure: &mime::MimeNode,
) -> Result<Option<HashMap<String, Vec<u8>>>> {
    match session
        .fetch_mime_headers(uid, &structure.garbled_sections())
        .await
    {
        Ok(headers) => Ok(headers),
        // Only the names stay garbled.
        Err(imap::Error::Incomplete(_)) => {
            tracing::warn!(
                uid,
                "unparsable MIME headers; keeping the names from BODYSTRUCTURE"
            );
            Ok(Some(HashMap::new()))
        }
        Err(e) => Err(e.into()),
    }
}

/// Opens the message's folder read-only and makes sure its UIDs still mean what they
/// meant when the message was stored. A connection that has it open read-only already,
/// with that UIDVALIDITY and since before the message arrived, is used as it is (see
/// [`Session::examined`]): on Gmail nearly every message is in All Mail. Returns how
/// long opening it took, if it was opened.
async fn examine(session: &mut Session, location: &MessageLocation) -> Result<Option<Duration>> {
    if session
        .examined(&location.remote_name, location.uid)
        .is_some_and(|open| location.uidvalidity == Some(open))
    {
        return Ok(None);
    }
    Ok(Some(reopen(session, location).await?))
}

/// Opens the message's folder read-only, open or not, and checks its UIDVALIDITY as
/// [`examine`] does. Returns how long that took.
async fn reopen(session: &mut Session, location: &MessageLocation) -> Result<Duration> {
    let began = Instant::now();
    let status = session.examine(&location.remote_name).await?;
    if !status.same_generation(location.uidvalidity) {
        return Err(Error::OutOfSync(REBUILT));
    }
    Ok(began.elapsed())
}

/// Stores the raw message and every part that came with it in the blob store.
pub(crate) fn new_body(engine: &Engine, content: Content, raw: Option<&[u8]>) -> Result<NewBody> {
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
        version: mime::CONTENT_VERSION,
    })
}

/// Stores `data` and records it with `record`, which returns false if the row it
/// belongs to is gone (then `gone` is the error).
fn store_blob(
    engine: &Engine,
    data: &[u8],
    record: impl FnOnce(&Db<'_>, &BlobHash) -> catmail_store::Result<bool>,
    gone: Error,
) -> Result<()> {
    let hash = engine.blobs.put_bytes(data)?;
    if engine.store.write(|db| record(db, &hash))? {
        Ok(())
    } else {
        Err(gone)
    }
}

async fn locate(engine: &Arc<Engine>, id: MessageId) -> Result<MessageLocation> {
    engine
        .read(move |db| db.messages().location(id))
        .await?
        .ok_or(Error::MessageNotFound(id))
}

async fn attachment_source(engine: &Arc<Engine>, id: AttachmentId) -> Result<AttachmentSource> {
    engine
        .read(move |db| db.attachments().source(id))
        .await?
        .ok_or(Error::AttachmentNotFound(id))
}

/// `None` if the blob has gone missing from disk, so the caller fetches it again.
pub(crate) async fn read_blob(engine: &Arc<Engine>, hash: BlobHash) -> Result<Option<Vec<u8>>> {
    let engine = engine.clone();
    blocking(move || {
        let data = stored(&engine, &hash)?;
        if data.is_none() {
            tracing::warn!(%hash, "referenced blob is missing; fetching again");
        }
        Ok(data)
    })
    .await
}

/// Blocking. The blob's content, `None` if it has gone missing from disk.
pub(crate) fn stored(engine: &Engine, hash: &BlobHash) -> Result<Option<Vec<u8>>> {
    match engine.blobs.read(hash) {
        Ok(data) => Ok(Some(data)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
