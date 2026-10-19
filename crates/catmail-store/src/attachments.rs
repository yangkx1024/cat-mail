use catmail_types::{Attachment, AttachmentId, BlobHash, MessageId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Result;
use crate::columns::opt_blob_hash;
use crate::messages::{MessageLocation, Messages};

const COLUMNS: &str = "id, message_id, filename, mime_type, size, content_id, is_inline, blob_hash";

/// An attachment or inline part of a body being stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewAttachment {
    /// IMAP body section, e.g. `2` or `1.2`.
    pub section: String,
    /// Content-Transfer-Encoding, needed to decode the section when fetched later.
    pub transfer_encoding: Option<String>,
    pub filename: Option<String>,
    pub mime_type: String,
    /// Decoded size; an estimate if `blob` is `None`.
    pub size: Option<u32>,
    pub content_id: Option<String>,
    pub inline: bool,
    /// The decoded content, if it was downloaded with the body.
    pub blob: Option<BlobHash>,
}

/// What it takes to fetch an attachment that isn't stored yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSource {
    pub attachment: Attachment,
    pub section: String,
    pub transfer_encoding: Option<String>,
    pub blob: Option<BlobHash>,
    pub message: MessageLocation,
}

pub struct Attachments<'c> {
    conn: &'c Connection,
}

impl<'c> Attachments<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// In MIME order.
    pub fn list(&self, message: MessageId) -> Result<Vec<Attachment>> {
        let sql = format!("SELECT {COLUMNS} FROM attachments WHERE message_id = ?1 ORDER BY id");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([message.0], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn source(&self, id: AttachmentId) -> Result<Option<AttachmentSource>> {
        let sql = format!(
            "SELECT {COLUMNS}, part_section, transfer_encoding FROM attachments WHERE id = ?1"
        );
        let found = self
            .conn
            .query_row(&sql, [id.0], |row| {
                Ok((
                    from_row(row)?,
                    row.get::<_, String>("part_section")?,
                    row.get::<_, Option<String>>("transfer_encoding")?,
                    opt_blob_hash(row, "blob_hash")?,
                ))
            })
            .optional()?;
        let Some((attachment, section, transfer_encoding, blob)) = found else {
            return Ok(None);
        };
        let Some(message) = Messages::new(self.conn).location(attachment.message_id)? else {
            return Ok(None);
        };
        Ok(Some(AttachmentSource {
            attachment,
            section,
            transfer_encoding,
            blob,
            message,
        }))
    }

    /// Records downloaded content and its exact size. Returns false if the attachment
    /// is gone (its message was deleted meanwhile).
    pub fn set_blob(&self, id: AttachmentId, blob: &BlobHash, size: u32) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE attachments SET blob_hash = ?2, size = ?3 WHERE id = ?1",
            params![id.0, blob.as_str(), size],
        )? > 0)
    }

    pub(crate) fn insert(&self, message: MessageId, attachments: &[NewAttachment]) -> Result<()> {
        let mut insert = self.conn.prepare_cached(
            "INSERT INTO attachments (message_id, part_section, transfer_encoding, filename,
                                      mime_type, size, content_id, is_inline, blob_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for a in attachments {
            insert.execute(params![
                message.0,
                a.section,
                a.transfer_encoding,
                a.filename,
                a.mime_type,
                a.size,
                a.content_id,
                a.inline,
                a.blob.as_ref().map(BlobHash::as_str),
            ])?;
        }
        Ok(())
    }
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        id: AttachmentId(row.get("id")?),
        message_id: MessageId(row.get("message_id")?),
        filename: row.get("filename")?,
        mime_type: row.get("mime_type")?,
        size: row.get("size")?,
        content_id: row.get("content_id")?,
        inline: row.get("is_inline")?,
        downloaded: row.get::<_, Option<String>>("blob_hash")?.is_some(),
    })
}
