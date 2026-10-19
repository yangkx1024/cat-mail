use catmail_types::{
    AccountId, BlobHash, Draft, DraftAttachment, DraftAttachmentId, DraftContent, DraftId,
    MessageId, OutboxId,
};
use rusqlite::{Connection, OptionalExtension, Row, named_params, params};

use crate::columns::{id_list, json, parse_text, to_id_list, to_json};
use crate::{Error, Result};

const COLUMNS: &str = "d.id, d.account_id, d.reply_to_message_id, d.in_reply_to, d.references_hdr,
    d.subject, d.to_json, d.cc_json, d.bcc_json, d.body_text, d.revision, d.updated_at,
    o.id AS outbox_id";

/// What a reply draft keeps of the message it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftReply {
    pub message: MessageId,
    /// Message-IDs without angle brackets.
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
}

/// A file being attached, already in the blob store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDraftAttachment {
    pub blob: BlobHash,
    pub filename: String,
    pub mime_type: String,
    pub size: u64,
}

/// A draft as it is about to be sent, with what building the message needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSnapshot {
    pub draft: Draft,
    /// Queue only if the draft still has this revision.
    pub revision: i64,
    /// The attachments' content, in `draft.attachments` order.
    pub blobs: Vec<BlobHash>,
}

pub struct Drafts<'c> {
    conn: &'c Connection,
}

impl<'c> Drafts<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// The account must own the message replied to.
    pub fn insert(
        &self,
        account: AccountId,
        content: &DraftContent,
        reply: Option<&DraftReply>,
        now: i64,
    ) -> Result<DraftId> {
        if let Some(reply) = reply {
            let owner: Option<i64> = self
                .conn
                .query_row(
                    "SELECT account_id FROM messages WHERE id = ?1",
                    [reply.message.0],
                    |r| r.get(0),
                )
                .optional()?;
            if owner.is_some_and(|owner| owner != account.0) {
                return Err(Error::Conflict(format!(
                    "message {} belongs to another account",
                    reply.message
                )));
            }
        }
        self.conn
            .prepare_cached(
                "INSERT INTO drafts (account_id, reply_to_message_id, in_reply_to, references_hdr,
                                     subject, to_json, cc_json, bcc_json, body_text, updated_at)
                 VALUES (:account, (SELECT id FROM messages WHERE id = :reply), :in_reply_to,
                         :references, :subject, :to, :cc, :bcc, :text, :now)",
            )?
            .execute(named_params! {
                ":account": account.0,
                ":reply": reply.map(|r| r.message.0),
                ":in_reply_to": reply.and_then(|r| r.in_reply_to.as_deref()),
                ":references": reply.and_then(|r| to_id_list(&r.references)),
                ":subject": content.subject,
                ":to": to_json(&content.to),
                ":cc": to_json(&content.cc),
                ":bcc": to_json(&content.bcc),
                ":text": content.text,
                ":now": now,
            })?;
        Ok(DraftId(self.conn.last_insert_rowid()))
    }

    /// The account a draft belongs to, if the draft exists.
    pub fn account_of(&self, id: DraftId) -> Result<Option<AccountId>> {
        Ok(self
            .conn
            .prepare_cached("SELECT account_id FROM drafts WHERE id = ?1")?
            .query_row([id.0], |r| Ok(AccountId(r.get(0)?)))
            .optional()?)
    }

    pub fn get(&self, id: DraftId) -> Result<Option<Draft>> {
        Ok(self.snapshot(id)?.map(|s| s.draft))
    }

    /// The account's drafts that can be edited, most recently changed first. Drafts
    /// being sent are in the outbox instead.
    pub fn list(&self, account: AccountId) -> Result<Vec<Draft>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM drafts d LEFT JOIN outbox o ON o.draft_id = d.id
             WHERE d.account_id = ?1 AND o.id IS NULL
             ORDER BY d.updated_at DESC, d.id DESC"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([account.0], from_row)?;
        let mut drafts = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        for draft in &mut drafts {
            draft.attachments = self
                .attachments(draft.id)?
                .into_iter()
                .map(|a| a.0)
                .collect();
        }
        Ok(drafts)
    }

    /// The draft with its revision and attachment blobs.
    pub fn snapshot(&self, id: DraftId) -> Result<Option<DraftSnapshot>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM drafts d LEFT JOIN outbox o ON o.draft_id = d.id
             WHERE d.id = ?1"
        );
        let found = self
            .conn
            .prepare_cached(&sql)?
            .query_row([id.0], |row| Ok((from_row(row)?, row.get("revision")?)))
            .optional()?;
        let Some((mut draft, revision)) = found else {
            return Ok(None);
        };
        let (attachments, blobs) = self.attachments(id)?.into_iter().unzip();
        draft.attachments = attachments;
        Ok(Some(DraftSnapshot {
            draft,
            revision,
            blobs,
        }))
    }

    /// Replaces what the user wrote. Returns false if there is no such draft.
    pub fn update(&self, id: DraftId, content: &DraftContent, now: i64) -> Result<bool> {
        if !self.check_editable(id)? {
            return Ok(false);
        }
        self.conn
            .prepare_cached(
                "UPDATE drafts SET subject = :subject, to_json = :to, cc_json = :cc,
                                   bcc_json = :bcc, body_text = :text,
                                   revision = revision + 1, updated_at = :now
                 WHERE id = :id",
            )?
            .execute(named_params! {
                ":id": id.0,
                ":subject": content.subject,
                ":to": to_json(&content.to),
                ":cc": to_json(&content.cc),
                ":bcc": to_json(&content.bcc),
                ":text": content.text,
                ":now": now,
            })?;
        Ok(true)
    }

    /// Returns false if there is no such draft.
    pub fn delete(&self, id: DraftId) -> Result<bool> {
        if !self.check_editable(id)? {
            return Ok(false);
        }
        self.conn
            .execute("DELETE FROM drafts WHERE id = ?1", [id.0])?;
        Ok(true)
    }

    /// `None` if there is no such draft.
    pub fn add_attachment(
        &self,
        draft: DraftId,
        attachment: &NewDraftAttachment,
        now: i64,
    ) -> Result<Option<DraftAttachment>> {
        if !self.check_editable(draft)? {
            return Ok(None);
        }
        self.conn.execute(
            "INSERT INTO draft_attachments (draft_id, blob_hash, filename, mime_type, size)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                draft.0,
                attachment.blob.as_str(),
                attachment.filename,
                attachment.mime_type,
                attachment.size as i64,
            ],
        )?;
        let id = DraftAttachmentId(self.conn.last_insert_rowid());
        self.touch(draft, now)?;
        Ok(Some(DraftAttachment {
            id,
            filename: attachment.filename.clone(),
            mime_type: attachment.mime_type.clone(),
            size: attachment.size,
        }))
    }

    /// Returns the draft it was attached to, or `None` if there is no such attachment.
    pub fn remove_attachment(&self, id: DraftAttachmentId, now: i64) -> Result<Option<DraftId>> {
        let draft: Option<i64> = self
            .conn
            .query_row(
                "SELECT draft_id FROM draft_attachments WHERE id = ?1",
                [id.0],
                |r| r.get(0),
            )
            .optional()?;
        let Some(draft) = draft.map(DraftId) else {
            return Ok(None);
        };
        self.check_editable(draft)?;
        self.conn
            .execute("DELETE FROM draft_attachments WHERE id = ?1", [id.0])?;
        self.touch(draft, now)?;
        Ok(Some(draft))
    }

    /// False if there is no such draft; an error if it is being sent.
    fn check_editable(&self, id: DraftId) -> Result<bool> {
        let outbox: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT o.id FROM drafts d LEFT JOIN outbox o ON o.draft_id = d.id
                 WHERE d.id = ?1",
                [id.0],
                |r| r.get(0),
            )
            .optional()?;
        match outbox {
            None => Ok(false),
            Some(None) => Ok(true),
            Some(Some(outbox)) => Err(Error::Conflict(format!(
                "draft {id} is in the outbox (entry {outbox}); cancel the send to edit it"
            ))),
        }
    }

    fn touch(&self, id: DraftId, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE drafts SET revision = revision + 1, updated_at = ?2 WHERE id = ?1",
            params![id.0, now],
        )?;
        Ok(())
    }

    fn attachments(&self, draft: DraftId) -> Result<Vec<(DraftAttachment, BlobHash)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, filename, mime_type, size, blob_hash FROM draft_attachments
             WHERE draft_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([draft.0], |r| {
            Ok((
                DraftAttachment {
                    id: DraftAttachmentId(r.get("id")?),
                    filename: r.get("filename")?,
                    mime_type: r.get("mime_type")?,
                    size: r.get::<_, i64>("size")? as u64,
                },
                parse_text(r, "blob_hash")?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Without attachments.
fn from_row(row: &Row<'_>) -> rusqlite::Result<Draft> {
    Ok(Draft {
        id: DraftId(row.get("id")?),
        account_id: AccountId(row.get("account_id")?),
        content: DraftContent {
            subject: row.get("subject")?,
            to: json(row, "to_json")?,
            cc: json(row, "cc_json")?,
            bcc: json(row, "bcc_json")?,
            text: row.get("body_text")?,
        },
        attachments: Vec::new(),
        reply_to: row
            .get::<_, Option<i64>>("reply_to_message_id")?
            .map(MessageId),
        in_reply_to: row.get("in_reply_to")?,
        references: id_list(row, "references_hdr")?,
        outbox: row.get::<_, Option<i64>>("outbox_id")?.map(OutboxId),
        updated_at: row.get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use catmail_types::{Address, OutboxStatus};

    use super::*;
    use crate::outbox::NewOutboxEntry;
    use crate::{Store, test_util};

    fn content(subject: &str) -> DraftContent {
        DraftContent {
            subject: subject.into(),
            to: vec![Address {
                name: Some("张三".into()),
                email: "zhang@example.com".into(),
            }],
            bcc: vec![Address {
                name: None,
                email: "secret@example.com".into(),
            }],
            text: "你好".into(),
            ..Default::default()
        }
    }

    fn hash(c: char) -> BlobHash {
        BlobHash::parse(&c.to_string().repeat(64)).unwrap()
    }

    fn attachment(name: &str) -> NewDraftAttachment {
        NewDraftAttachment {
            blob: hash('a'),
            filename: name.into(),
            mime_type: "application/pdf".into(),
            size: 3,
        }
    }

    fn queue(store: &Store, account: AccountId, draft: DraftId) -> Result<catmail_types::OutboxId> {
        store.write(|db| {
            let revision = db.drafts().snapshot(draft)?.unwrap().revision;
            db.outbox().queue(
                &NewOutboxEntry {
                    account,
                    draft,
                    revision,
                    raw: hash('b'),
                    message_id: "m@example.com".into(),
                    subject: "s".into(),
                    envelope_from: "a@example.com".into(),
                    recipients: vec!["zhang@example.com".into()],
                },
                10,
            )
        })
    }

    #[test]
    fn lifecycle() {
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        let id = store
            .write(|db| db.drafts().insert(account, &content("one"), None, 1))
            .unwrap();
        let older = store
            .write(|db| db.drafts().insert(account, &content("two"), None, 0))
            .unwrap();

        let draft = store.read(|db| db.drafts().get(id)).unwrap().unwrap();
        assert_eq!(draft.content, content("one"));
        assert_eq!(
            (draft.reply_to, draft.outbox, draft.updated_at),
            (None, None, 1)
        );

        assert!(
            store
                .write(|db| db.drafts().update(id, &content("one, edited"), 5))
                .unwrap()
        );
        let pdf = store
            .write(|db| db.drafts().add_attachment(id, &attachment("报告.pdf"), 6))
            .unwrap()
            .unwrap();
        let snapshot = store.read(|db| db.drafts().snapshot(id)).unwrap().unwrap();
        assert_eq!(snapshot.revision, 2, "every change bumps the revision");
        assert_eq!(snapshot.draft.content.subject, "one, edited");
        assert_eq!(snapshot.draft.attachments, std::slice::from_ref(&pdf));
        assert_eq!(snapshot.blobs, [hash('a')]);
        assert_eq!(
            store.read(|db| db.blob_references()).unwrap(),
            [hash('a')].into()
        );

        let ids: Vec<_> = store
            .read(|db| db.drafts().list(account))
            .unwrap()
            .into_iter()
            .map(|d| (d.id, d.attachments.len()))
            .collect();
        assert_eq!(ids, [(id, 1), (older, 0)], "most recently changed first");

        assert_eq!(
            store
                .write(|db| db.drafts().remove_attachment(pdf.id, 7))
                .unwrap(),
            Some(id)
        );
        assert_eq!(
            store
                .write(|db| db.drafts().remove_attachment(pdf.id, 7))
                .unwrap(),
            None
        );
        assert!(store.write(|db| db.drafts().delete(id)).unwrap());
        assert!(!store.write(|db| db.drafts().delete(id)).unwrap());
        assert!(
            !store
                .write(|db| db.drafts().update(id, &content("gone"), 8))
                .unwrap()
        );
        assert!(
            store
                .write(|db| db.drafts().add_attachment(id, &attachment("x"), 8))
                .unwrap()
                .is_none()
        );
    }

    fn locked<T>(r: Result<T>) -> bool {
        matches!(r, Err(Error::Conflict(_)))
    }

    #[test]
    fn queued_drafts_are_locked() {
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        let id = store
            .write(|db| db.drafts().insert(account, &content("one"), None, 1))
            .unwrap();
        let pdf = store
            .write(|db| db.drafts().add_attachment(id, &attachment("a.pdf"), 1))
            .unwrap()
            .unwrap();
        let outbox = queue(&store, account, id).unwrap();

        assert_eq!(
            store
                .read(|db| db.drafts().get(id))
                .unwrap()
                .unwrap()
                .outbox,
            Some(outbox)
        );
        assert!(
            store
                .read(|db| db.drafts().list(account))
                .unwrap()
                .is_empty()
        );
        assert!(locked(store.write(|db| db.drafts().update(
            id,
            &content("x"),
            2
        ))));
        assert!(locked(store.write(|db| db.drafts().delete(id))));
        assert!(locked(store.write(|db| db.drafts().add_attachment(
            id,
            &attachment("b"),
            2
        ))));
        assert!(locked(
            store.write(|db| db.drafts().remove_attachment(pdf.id, 2))
        ));
        assert!(locked(queue(&store, account, id)), "queued once");

        // Cancelling a send that never went out hands the draft back.
        let entry = store
            .write(|db| db.outbox().cancel(outbox))
            .unwrap()
            .unwrap();
        assert_eq!(entry.status, OutboxStatus::Queued);
        assert_eq!(store.read(|db| db.drafts().list(account)).unwrap().len(), 1);
        assert!(
            store
                .write(|db| db.drafts().update(id, &content("x"), 3))
                .unwrap()
        );
    }

    #[test]
    fn queueing_needs_the_revision_it_was_built_from() {
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        let other = test_util::account(&store, "b@example.com");
        let id = store
            .write(|db| db.drafts().insert(account, &content("one"), None, 1))
            .unwrap();
        let entry = |account, revision| NewOutboxEntry {
            account,
            draft: id,
            revision,
            raw: hash('b'),
            message_id: "m@example.com".into(),
            subject: "s".into(),
            envelope_from: "a@example.com".into(),
            recipients: vec!["zhang@example.com".into()],
        };
        store
            .write(|db| db.drafts().update(id, &content("two"), 2))
            .unwrap();
        for (account, revision) in [(account, 0), (other, 1)] {
            let result = store.write(|db| db.outbox().queue(&entry(account, revision), 3));
            assert!(matches!(result, Err(Error::Conflict(_))), "{result:?}");
        }
        store
            .write(|db| db.outbox().queue(&entry(account, 1), 3))
            .unwrap();
    }

    #[test]
    fn replies_keep_their_threading_headers() {
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        let other = test_util::account(&store, "b@example.com");
        let message = store
            .write(|db| {
                db.conn.execute_batch(
                    "INSERT INTO folders (id, account_id, remote_name, name) VALUES (9, 1, 'INBOX', 'INBOX');
                     INSERT INTO messages (id, account_id, folder_id, uid) VALUES (42, 1, 9, 1);",
                )?;
                Ok(MessageId(42))
            })
            .unwrap();
        let reply = DraftReply {
            message,
            in_reply_to: Some("parent@x".into()),
            references: vec!["root@x".into(), "parent@x".into()],
        };
        let result = store.write(|db| db.drafts().insert(other, &content("re"), Some(&reply), 1));
        assert!(matches!(result, Err(Error::Conflict(_))), "{result:?}");

        let id = store
            .write(|db| db.drafts().insert(account, &content("re"), Some(&reply), 1))
            .unwrap();
        let draft = store.read(|db| db.drafts().get(id)).unwrap().unwrap();
        assert_eq!(draft.reply_to, Some(message));
        assert_eq!(draft.in_reply_to.as_deref(), Some("parent@x"));
        assert_eq!(draft.references, ["root@x", "parent@x"]);

        store
            .write(|db| Ok(db.conn.execute("DELETE FROM messages WHERE id = 42", [])?))
            .unwrap();
        let draft = store.read(|db| db.drafts().get(id)).unwrap().unwrap();
        assert_eq!(draft.reply_to, None);
        assert_eq!(draft.references, ["root@x", "parent@x"]);
    }
}
