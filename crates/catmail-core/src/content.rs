//! What a fetched message becomes when stored: parsed into text, HTML, attachments and
//! snippet, with the raw message and the parts that came with it in the blob store. And
//! reading a stored blob back.
//!
//! Downloading ([`body`](crate::body), the prefetch) and rebuilding bodies made by an
//! older content version ([`rebuild`](crate::rebuild)) both make bodies this way. It
//! needs only the blob store, not the engine.

use std::collections::HashMap;

use catmail_proto::mime::{self, Content};
use catmail_store::{NewAttachment, NewBody};

use crate::Result;
use crate::storage::Storage;

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

impl Parts {
    pub fn new(
        structure: mime::MimeNode,
        text: HashMap<String, Vec<u8>>,
        headers: HashMap<String, Vec<u8>>,
    ) -> Self {
        Self {
            structure,
            text,
            headers,
        }
    }
}

/// Parses a fetched body into what is stored, putting the raw message and the parts
/// that came with it in the blob store. Blocking.
pub(crate) fn parse(storage: &Storage, fetched: Fetched) -> Result<NewBody> {
    let (content, raw) = match fetched {
        Fetched::Whole(raw) => (mime::parse_message(&raw), Some(raw)),
        Fetched::Parts(parts) => (parts.structure.content(&parts.text, &parts.headers), None),
    };
    new_body(storage, content, raw.as_deref())
}

/// Stores the raw message and every part that came with it in the blob store. Blocking.
pub(crate) fn new_body(storage: &Storage, content: Content, raw: Option<&[u8]>) -> Result<NewBody> {
    let raw = raw.map(|raw| storage.put_bytes(raw)).transpose()?;
    let attachments = content
        .parts
        .into_iter()
        .map(|part| {
            let blob = part
                .data
                .as_deref()
                .map(|data| storage.put_bytes(data))
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
