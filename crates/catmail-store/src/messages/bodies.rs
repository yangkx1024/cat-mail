//! The bodies: their previews, storing and rebuilding them with their attachments and
//! the search index, and where on the server a message or attachment is fetched from.

use catmail_types::{AccountId, Address, Attachment, AttachmentId, BlobHash, FolderId, MessageId};
use rusqlite::{OptionalExtension, named_params, params};

use super::attachments::{Attachments, NewAttachment, Part};
use super::{MOVING, Messages, fts};
use crate::Result;
use crate::columns::{json, parse_opt_text, to_json};

/// A downloaded body, stored in one go with its attachments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewBody {
    pub text: Option<String>,
    /// Already sanitized.
    pub html: Option<String>,
    /// The whole raw message, if it was fetched whole.
    pub raw: Option<BlobHash>,
    pub snippet: Option<String>,
    pub attachments: Vec<NewAttachment>,
    /// The `catmail_proto::mime::CONTENT_VERSION` that made it.
    pub version: u32,
}

/// A stored body made by an older content version, with what it can be made again from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleBody {
    pub id: MessageId,
    pub account: AccountId,
    pub folder: FolderId,
    pub version: u32,
    /// The whole raw message, if it was kept.
    pub raw: Option<BlobHash>,
    pub html: Option<String>,
}

/// What it takes to fetch an attachment that isn't stored yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSource {
    pub section: String,
    pub transfer_encoding: Option<String>,
    pub blob: Option<BlobHash>,
    pub message: MessageLocation,
}

/// Where a message lives on the server, and what of it is stored locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageLocation {
    pub id: MessageId,
    pub account: AccountId,
    /// Where the message shows locally. For a message moved here and not yet on the
    /// server, the rest describes the folder it is moving from.
    pub folder: FolderId,
    /// The folder that has the message on the server.
    pub server_folder: FolderId,
    pub remote_name: String,
    /// That folder's current UIDVALIDITY; `uid` is only meaningful within it.
    pub uidvalidity: Option<u32>,
    pub uid: u32,
    /// RFC822.SIZE.
    pub size: Option<u32>,
    pub downloaded: bool,
    pub body_version: Option<u32>,
    pub raw: Option<BlobHash>,
}

impl Messages<'_> {
    /// Of the messages at `uids` in `folder` while its UIDVALIDITY is `uidvalidity`, those
    /// whose bodies aren't downloaded, at most `limit`, the highest UIDs: what a search on
    /// the server adds to the local one. A message moved locally isn't at its old UID any
    /// more, nor yet at a new one.
    pub fn without_body(
        &self,
        folder: FolderId,
        uidvalidity: u32,
        uids: &[u32],
        limit: usize,
    ) -> Result<Vec<MessageId>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT m.id FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE m.folder_id = :folder AND f.uidvalidity = :uidvalidity
               AND m.body_state = 'headers'
               AND m.uid IN (SELECT value FROM json_each(:uids))
             ORDER BY m.uid DESC LIMIT :limit",
        )?;
        let rows = stmt.query_map(
            named_params! {
                ":folder": folder.0,
                ":uidvalidity": uidvalidity,
                ":uids": to_json(uids),
                ":limit": limit as i64,
            },
            |r| Ok(MessageId(r.get(0)?)),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// UIDs of the messages in `folder` whose preview to ask the server for (RFC 8970
    /// PREVIEW): neither it nor the body came yet. At most `limit`, newest first, as
    /// lists show them. [`store_previews`](Self::store_previews) checks the UIDVALIDITY
    /// they were asked for in.
    pub fn unpreviewed(&self, folder: FolderId, limit: usize) -> Result<Vec<u32>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT uid FROM messages
             WHERE folder_id = ?1
               AND body_state = 'headers' AND previewed = 0 AND uid IS NOT NULL
             ORDER BY list_date DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![folder.0, limit as i64], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Stores the snippets the server's previews make of the messages at these UIDs in
    /// `folder`, while its UIDVALIDITY is `uidvalidity` (`None`: it had none to give),
    /// and that they came, so they aren't asked for again. Not where the body is stored:
    /// its snippet, made here, is kept. Returns how many snippets were stored.
    pub fn store_previews(
        &self,
        folder: FolderId,
        uidvalidity: u32,
        previews: &[(u32, Option<String>)],
    ) -> Result<usize> {
        let mut update = self.conn.prepare_cached(
            "UPDATE messages SET previewed = 1, snippet = ?4
             WHERE folder_id = ?1 AND uid = ?3 AND body_state = 'headers' AND previewed = 0
               AND EXISTS (SELECT 1 FROM folders WHERE id = ?1 AND uidvalidity = ?2)",
        )?;
        let mut shown = Vec::new();
        for (uid, snippet) in previews {
            let updated = update.execute(params![folder.0, uidvalidity, uid, snippet])?;
            if updated > 0 && snippet.is_some() {
                shown.push(*uid);
            }
        }
        if !shown.is_empty() {
            self.note_shown_in(folder, Some(&shown))?;
        }
        Ok(shown.len())
    }

    /// `None` if the message is gone. A message moved locally is where the server has
    /// it until the move is made: in the folder its pending move is from.
    pub fn location(&self, id: MessageId) -> Result<Option<MessageLocation>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT m.account_id, m.folder_id, f.id, f.remote_name,
                            iif(o.id IS NULL, f.uidvalidity, o.uidvalidity),
                            coalesce(m.uid, o.uid), m.size, m.body_state = 'full',
                            b.content_version, b.raw_blob_hash
                     FROM messages m
                     {MOVING}
                     JOIN folders f ON f.id = coalesce(o.folder_id, m.folder_id)
                     LEFT JOIN message_bodies b ON b.message_id = m.id
                     WHERE m.id = ?1 AND coalesce(m.uid, o.uid) IS NOT NULL"
                ),
                [id.0],
                |r| {
                    Ok(MessageLocation {
                        id,
                        account: AccountId(r.get(0)?),
                        folder: FolderId(r.get(1)?),
                        server_folder: FolderId(r.get(2)?),
                        remote_name: r.get(3)?,
                        uidvalidity: r.get(4)?,
                        uid: r.get(5)?,
                        size: r.get(6)?,
                        downloaded: r.get(7)?,
                        body_version: r.get(8)?,
                        raw: parse_opt_text(r, "raw_blob_hash")?,
                    })
                },
            )
            .optional()?)
    }

    /// Where the attachment's message is, and the attachment in it; `None` if either is
    /// gone.
    pub fn attachment_source(&self, id: AttachmentId) -> Result<Option<AttachmentSource>> {
        let Some(part) = Attachments::new(self.conn).part(id)? else {
            return Ok(None);
        };
        let Part {
            message,
            section,
            transfer_encoding,
            blob,
        } = part;
        Ok(self.location(message)?.map(|message| AttachmentSource {
            section,
            transfer_encoding,
            blob,
            message,
        }))
    }

    /// Stores the body, its attachments, the snippet and the recomputed attachment flag,
    /// and indexes the body text. Returns false, changing nothing, if the message is
    /// gone or its body is already stored.
    pub fn store_body(&self, id: MessageId, body: &NewBody) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE messages SET body_state = 'full' WHERE id = ?1 AND body_state <> 'full'",
            [id.0],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        self.conn.execute(
            "INSERT INTO message_bodies (message_id, text_plain, text_html, raw_blob_hash,
                                         content_version)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.0,
                body.text,
                body.html,
                body.raw.as_ref().map(BlobHash::as_str),
                body.version,
            ],
        )?;
        self.store_parts(id, body, true)?;
        self.conn.execute(
            "DELETE FROM prefetch_failures WHERE message_id = ?1",
            [id.0],
        )?;
        self.note_shown(id.0, false)?;
        Ok(true)
    }

    /// What goes with a body stored: snippet, attachment flag, attachments and, if
    /// `reindex`, the index entry.
    fn store_parts(&self, id: MessageId, body: &NewBody, reindex: bool) -> Result<()> {
        let has_attachments = body.attachments.iter().any(|a| !a.inline);
        self.conn.execute(
            "UPDATE messages SET snippet = ?2, has_attachments = ?3 WHERE id = ?1",
            params![id.0, body.snippet, has_attachments],
        )?;
        Attachments::new(self.conn).store(id, &body.attachments)?;
        if reindex {
            self.reindex(id.0)?;
        }
        Ok(())
    }

    /// The bodies made by a content version below `current`, newest message first.
    pub fn stale_bodies(&self, current: u32) -> Result<Vec<MessageId>> {
        let mut stmt = self.conn.prepare(
            "SELECT message_id FROM message_bodies WHERE content_version < ?1
             ORDER BY message_id DESC",
        )?;
        let rows = stmt.query_map([current], |r| Ok(MessageId(r.get(0)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The message's body if a content version below `current` made it.
    pub fn stale_body(&self, id: MessageId, current: u32) -> Result<Option<StaleBody>> {
        Ok(self
            .conn
            .query_row(
                "SELECT m.account_id, m.folder_id, b.content_version, b.raw_blob_hash,
                        b.text_html
                 FROM message_bodies b JOIN messages m ON m.id = b.message_id
                 WHERE b.message_id = ?1 AND b.content_version < ?2",
                params![id.0, current],
                |r| {
                    Ok(StaleBody {
                        id,
                        account: AccountId(r.get(0)?),
                        folder: FolderId(r.get(1)?),
                        version: r.get(2)?,
                        raw: parse_opt_text(r, "raw_blob_hash")?,
                        html: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Replaces a body made by content version `from` with `body`, made again from the
    /// same raw message, with its attachments, snippet and index entry. Attachments keep
    /// their ids by section (a reader may be showing them), and a downloaded part keeps
    /// its content where `body` has none. Returns false, changing nothing, if the
    /// message is gone or its body was rebuilt meanwhile.
    pub fn rebuild_body(&self, id: MessageId, from: u32, body: &NewBody) -> Result<bool> {
        let old_text: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT text_plain FROM message_bodies
                 WHERE message_id = ?1 AND content_version = ?2",
                params![id.0, from],
                |r| r.get(0),
            )
            .optional()?;
        let Some(old_text) = old_text else {
            return Ok(false);
        };
        self.conn.execute(
            "UPDATE message_bodies SET text_plain = ?2, text_html = ?3, content_version = ?4
             WHERE message_id = ?1",
            params![id.0, body.text, body.html, body.version],
        )?;
        // Mostly the sanitizing changed, which leaves the text as it was.
        self.store_parts(id, body, old_text != body.text)?;
        self.note_shown(id.0, false)?;
        Ok(true)
    }

    /// Replaces the HTML of a body made by content version `from` and kept without its
    /// raw message, cleaned again by content version `to`, with the attachments that
    /// `inline` picks shown within it. The text, and so the snippet and index entry,
    /// stay. Returns false, changing nothing, if the message is gone or its body was
    /// rebuilt meanwhile.
    pub fn refresh_html(
        &self,
        id: MessageId,
        from: u32,
        to: u32,
        html: Option<&str>,
        inline: impl Fn(&Attachment) -> bool,
    ) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE message_bodies SET text_html = ?3, content_version = ?4
             WHERE message_id = ?1 AND content_version = ?2",
            params![id.0, from, html, to],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        Attachments::new(self.conn).mark_inline(id, inline)?;
        self.conn.execute(
            "UPDATE messages SET has_attachments =
                 EXISTS (SELECT 1 FROM attachments WHERE message_id = ?1 AND NOT is_inline)
             WHERE id = ?1",
            [id.0],
        )?;
        self.note_shown(id.0, false)?;
        Ok(true)
    }

    /// Records the raw message of an already downloaded body. Returns false if the
    /// body is gone.
    pub fn set_raw(&self, id: MessageId, raw: &BlobHash) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE message_bodies SET raw_blob_hash = ?2 WHERE message_id = ?1",
            params![id.0, raw.as_str()],
        )? > 0)
    }

    /// Writes the message's index entry from what is stored now, replacing any old one.
    /// Every change to indexed text (a new message, its body) goes through here; deletes
    /// are handled by a trigger.
    pub(super) fn reindex(&self, id: i64) -> Result<()> {
        let mut read = self.conn.prepare_cached(
            "SELECT m.subject, m.from_json, m.to_json, m.cc_json, b.text_plain
             FROM messages m LEFT JOIN message_bodies b ON b.message_id = m.id
             WHERE m.id = ?1",
        )?;
        let [subject, from, to, body]: [String; 4] = read.query_row([id], |r| {
            let (to, cc): (Vec<Address>, Vec<Address>) = (json(r, "to_json")?, json(r, "cc_json")?);
            Ok([
                r.get::<_, Option<String>>("subject")?.unwrap_or_default(),
                address_text(&json::<Vec<Address>>(r, "from_json")?),
                address_text(to.iter().chain(&cc)),
                r.get::<_, Option<String>>("text_plain")?
                    .unwrap_or_default(),
            ])
        })?;
        self.conn
            .prepare_cached("DELETE FROM messages_fts WHERE rowid = ?1")?
            .execute([id])?;
        self.conn
            .prepare_cached(
                "INSERT INTO messages_fts (rowid, subject, from_text, to_text, body)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                id,
                fts::segment(&subject),
                fts::segment(&from),
                fts::segment(&to),
                fts::segment(&body),
            ])?;
        Ok(())
    }
}

/// Names and addresses, so either finds the message.
fn address_text<'a>(addresses: impl IntoIterator<Item = &'a Address>) -> String {
    let mut out = String::new();
    for a in addresses {
        if let Some(name) = &a.name {
            out.push_str(name);
            out.push(' ');
        }
        out.push_str(&a.email);
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use catmail_types::{Address, Flags, ListQuery, Message, MessageId};

    use super::*;
    use crate::messages::{NewAttachment, NewBody, NewMessage};

    use crate::messages::fixtures::{body, hash, message, search, setup};
    use crate::test_util::{self};

    #[test]
    fn previews_fill_snippets_until_the_body_comes() {
        let (store, account, folder) = setup();
        let previewed = NewMessage {
            previewed: true,
            snippet: Some("From the server".into()),
            ..message(4, Some(4_000), Flags::empty())
        };
        let batch = [
            message(1, Some(1_000), Flags::empty()),
            message(2, Some(2_000), Flags::empty()),
            message(3, Some(3_000), Flags::empty()),
            previewed,
        ];
        store
            .write(|db| {
                db.reset_uidvalidity(folder.id, 7)?;
                db.messages().upsert(account, folder.id, &batch)
            })
            .unwrap();
        let id = |uid| test_util::message_id(&store, folder.id, uid);
        let snippet = |uid| {
            store
                .read(|db| db.messages().get(id(uid)))
                .unwrap()
                .unwrap()
                .summary
                .snippet
        };
        assert_eq!(snippet(4).as_deref(), Some("From the server"));
        store
            .write(|db| db.messages().store_body(id(3), &body()))
            .unwrap();
        let unpreviewed = |limit| {
            store
                .read(|db| db.messages().unpreviewed(folder.id, limit))
                .unwrap()
        };
        // Newest first; not one whose body or preview came.
        assert_eq!(unpreviewed(9), [2, 1]);
        assert_eq!(unpreviewed(1), [2]);

        let store_previews = |uidvalidity, previews: &[(u32, Option<String>)]| {
            store
                .write(|db| {
                    db.messages()
                        .store_previews(folder.id, uidvalidity, previews)
                })
                .unwrap()
        };
        assert_eq!(store_previews(8, &[(2, Some("Stale".into()))]), 0);
        let previews = [
            (1, None),
            (2, Some("Two".into())),
            // The body's snippet is kept, and so is a preview that came.
            (3, Some("Three".into())),
            (4, Some("Four".into())),
        ];
        assert_eq!(store_previews(7, &previews), 1);
        assert_eq!(
            [snippet(1), snippet(2), snippet(3), snippet(4)],
            [
                None,
                Some("Two".into()),
                body().snippet,
                Some("From the server".into())
            ]
        );
        // Neither is asked for again: one had none to give.
        assert!(unpreviewed(9).is_empty());
        // The body's snippet replaces the server's.
        store
            .write(|db| db.messages().store_body(id(2), &body()))
            .unwrap();
        assert_eq!(snippet(2), body().snippet);
    }

    #[test]
    fn rebuilds_bodies_in_place() {
        let (store, account, folder) = setup();
        store
            .write(|db| {
                db.messages()
                    .upsert(account, folder.id, &[message(1, None, Flags::empty())])
            })
            .unwrap();
        let id = store
            .read(|db| db.messages().list(folder.id, &ListQuery::new(1)))
            .unwrap()[0]
            .id;
        store
            .write(|db| db.messages().store_body(id, &body()))
            .unwrap();
        let image = store.read(|db| Attachments::new(db.conn).list(id)).unwrap()[1].id;
        store
            .write(|db| db.attachments().set_blob(image, &hash('c'), 4))
            .unwrap();

        assert!(
            store
                .read(|db| db.messages().stale_bodies(1))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.read(|db| db.messages().stale_bodies(2)).unwrap(),
            [id]
        );
        let stale = store
            .read(|db| db.messages().stale_body(id, 2))
            .unwrap()
            .unwrap();
        assert_eq!(
            (stale.account, stale.folder, stale.version, stale.raw),
            (account, folder.id, 1, Some(hash('a')))
        );
        assert_eq!(stale.html.as_deref(), Some("<p>请查收</p>"));

        let rebuilt = NewBody {
            text: Some("The pelican report".into()),
            html: Some("<p>pelican</p>".into()),
            raw: Some(hash('a')),
            snippet: Some("The pelican report".into()),
            attachments: vec![
                // Not referred to any more, and fetched since.
                NewAttachment {
                    section: "3".into(),
                    mime_type: "image/png".into(),
                    content_id: Some("logo@x".into()),
                    size: Some(99),
                    ..Default::default()
                },
                NewAttachment {
                    section: "4".into(),
                    mime_type: "image/gif".into(),
                    content_id: Some("new@x".into()),
                    inline: true,
                    blob: Some(hash('d')),
                    ..Default::default()
                },
            ],
            version: 2,
        };
        assert!(
            !store
                .write(|db| db.messages().rebuild_body(id, 0, &rebuilt))
                .unwrap(),
            "made by another version"
        );
        assert!(
            store
                .write(|db| db.messages().rebuild_body(id, 1, &rebuilt))
                .unwrap()
        );
        assert!(
            !store
                .write(|db| db.messages().rebuild_body(id, 1, &rebuilt))
                .unwrap(),
            "rebuilt once"
        );
        let message = store.read(|db| db.messages().get(id)).unwrap().unwrap();
        let stored = message.body.unwrap();
        assert_eq!(stored.html.as_deref(), Some("<p>pelican</p>"));
        assert_eq!(
            message.summary.snippet.as_deref(),
            Some("The pelican report")
        );
        let parts: Vec<_> = stored
            .attachments
            .iter()
            .map(|a| (a.mime_type.as_str(), a.size, a.inline, a.downloaded))
            .collect();
        assert_eq!(
            parts,
            [
                ("image/png", Some(4), false, true),
                ("image/gif", None, true, true)
            ],
            "the PDF went, the image kept what was fetched of it"
        );
        assert_eq!(stored.attachments[0].id, image, "same id");
        assert!(
            message.summary.has_attachments,
            "the image isn't inline now"
        );
        assert_eq!(search(&store, "pelican"), [Some("subject 1".into())]);
        assert!(
            search(&store, "季度报告").is_empty(),
            "the old text is gone"
        );
        assert_eq!(
            store
                .read(|db| db.messages().location(id))
                .unwrap()
                .unwrap()
                .body_version,
            Some(2)
        );

        assert!(
            store
                .write(|db| {
                    db.messages()
                        .refresh_html(id, 2, 3, Some("<img src=\"cid:logo@x\">"), |a| {
                            a.content_id.as_deref() == Some("logo@x")
                        })
                })
                .unwrap()
        );
        let message = store.read(|db| db.messages().get(id)).unwrap().unwrap();
        let stored = message.body.unwrap();
        assert_eq!(stored.html.as_deref(), Some("<img src=\"cid:logo@x\">"));
        assert_eq!(stored.text.as_deref(), Some("The pelican report"), "kept");
        let inline: Vec<_> = stored.attachments.iter().map(|a| a.inline).collect();
        assert_eq!(inline, [true, false]);
        assert!(message.summary.has_attachments);
        assert_eq!(search(&store, "pelican"), [Some("subject 1".into())]);
        assert!(
            store
                .read(|db| db.messages().stale_bodies(3))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn stores_body_attachments_and_index() {
        let (store, account, folder) = setup();
        let mut m = message(1, Some(5_000), Flags::empty());
        m.subject = Some("Quarterly report".into());
        m.cc = vec![Address {
            name: Some("王五".into()),
            email: "wang@example.com".into(),
        }];
        store
            .write(|db| {
                db.messages()
                    .upsert(account, folder.id, &[m, message(2, None, Flags::empty())])
            })
            .unwrap();
        let id = store
            .read(|db| db.messages().list(folder.id, &ListQuery::new(1)))
            .unwrap()[0]
            .id;
        assert_eq!(
            search(&store, "quarterly"),
            [Some("Quarterly report".into())]
        );
        assert_eq!(search(&store, "to:王五"), [Some("Quarterly report".into())]);
        assert!(
            search(&store, "季度报告").is_empty(),
            "body not downloaded yet"
        );

        let location = store
            .read(|db| db.messages().location(id))
            .unwrap()
            .unwrap();
        assert_eq!(
            (location.uid, location.downloaded, location.raw),
            (1, false, None)
        );
        assert!(
            store
                .read(|db| db.messages().get(id))
                .unwrap()
                .unwrap()
                .body
                .is_none()
        );

        assert!(
            store
                .write(|db| db.messages().store_body(id, &body()))
                .unwrap()
        );
        assert!(
            !store
                .write(|db| db.messages().store_body(id, &body()))
                .unwrap(),
            "stored once"
        );
        assert!(
            !store
                .write(|db| db.messages().store_body(MessageId(999), &body()))
                .unwrap()
        );

        let message = store.read(|db| db.messages().get(id)).unwrap().unwrap();
        let headers = store.read(|db| db.messages().headers(id)).unwrap().unwrap();
        assert_eq!(
            headers,
            Message {
                body: None,
                ..message.clone()
            },
            "the same, but for the body"
        );
        assert_eq!(message.summary.cc[0].email, "wang@example.com");
        assert!(message.summary.has_attachments, "the PDF is not inline");
        assert_eq!(
            message.summary.snippet.as_deref(),
            Some("请查收第三季度报告。")
        );
        let body = message.body.unwrap();
        assert_eq!(body.html.as_deref(), Some("<p>请查收</p>"));
        let parts: Vec<_> = body
            .attachments
            .iter()
            .map(|a| (a.filename.clone(), a.inline, a.downloaded))
            .collect();
        assert_eq!(
            parts,
            [(Some("报告.pdf".into()), false, true), (None, true, false)]
        );

        assert_eq!(
            search(&store, "季度报告"),
            [Some("Quarterly report".into())]
        );
        assert_eq!(
            search(&store, "numbers final"),
            [Some("Quarterly report".into())]
        );
        assert_eq!(
            search(&store, "subject:report"),
            [Some("Quarterly report".into())]
        );
        assert!(search(&store, "from:report").is_empty());

        // The inline image is fetched later.
        let png = body.attachments[1].id;
        let source = store
            .read(|db| db.messages().attachment_source(png))
            .unwrap()
            .unwrap();
        assert_eq!((source.section.as_str(), source.blob.clone()), ("3", None));
        assert_eq!(source.message.uid, 1);
        assert!(
            store
                .write(|db| db.attachments().set_blob(png, &hash('c'), 42))
                .unwrap()
        );
        assert!(
            store
                .write(|db| db.messages().set_raw(id, &hash('d')))
                .unwrap()
        );
        assert_eq!(
            store.read(|db| db.blob_references()).unwrap(),
            HashSet::from([hash('b'), hash('c'), hash('d')])
        );

        store
            .write(|db| db.messages().delete_uids(folder.id, &[1]))
            .unwrap();
        assert!(store.read(|db| db.blob_references()).unwrap().is_empty());
        assert!(
            store
                .read(|db| db.messages().attachment_source(png))
                .unwrap()
                .is_none()
        );
        assert!(search(&store, "quarterly").is_empty());
    }
}
