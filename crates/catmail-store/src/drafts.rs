use catmail_types::{
    AccountId, BlobHash, Draft, DraftAttachment, DraftAttachmentId, DraftContent, DraftId,
    FolderId, FolderRole, MessageId, OutboxId,
};
use rusqlite::{Connection, OptionalExtension, Row, named_params, params};

use crate::columns::{id_list, json, parse_text, to_id_list, to_json};
use crate::folders::Folders;
use crate::ops::{PendingOps, Touched};
use crate::{Error, Result};

const COLUMNS: &str = "d.id, d.account_id, d.reply_to_message_id, d.in_reply_to, d.references_hdr,
    d.subject, d.to_json, d.cc_json, d.bcc_json, d.body_text, d.revision, d.updated_at,
    d.remote_revision IS d.revision AS on_server, d.upload_error, o.id AS outbox_id";

/// Drafts whose copy on the server is missing or out of date, and that may be uploaded:
/// not being sent, and with no upload of theirs left to settle.
const STALE: &str = "SELECT d.id FROM drafts d
     WHERE d.account_id = ?1 AND d.remote_revision IS NOT d.revision
       AND NOT EXISTS (SELECT 1 FROM outbox o WHERE o.draft_id = d.id)
       AND NOT EXISTS (SELECT 1 FROM draft_uploads u WHERE u.draft_id = d.id)";

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

/// An upload of a draft to the Drafts folder (see the `draft_uploads` schema).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftUpload {
    pub id: i64,
    pub account: AccountId,
    /// `None` if the draft was deleted or sent since: a copy made is deleted.
    pub draft: Option<DraftId>,
    pub folder: FolderId,
    pub remote_name: String,
    /// Of the uploaded message, without brackets.
    pub message_id: String,
    pub revision: i64,
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

    /// How many drafts [`list`](Self::list) returns, without reading them.
    pub fn count(&self, account: AccountId) -> Result<usize> {
        let n: i64 = self
            .conn
            .prepare_cached(
                "SELECT COUNT(*) FROM drafts d LEFT JOIN outbox o ON o.draft_id = d.id
             WHERE d.account_id = ?1 AND o.id IS NULL",
            )?
            .query_row([account.0], |row| row.get(0))?;
        Ok(n as usize)
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

    /// Deletes the draft; its copy on the server is deleted by the next upload pass.
    /// Returns false if there is no such draft.
    pub fn delete(&self, id: DraftId) -> Result<bool> {
        if !self.check_editable(id)? {
            return Ok(false);
        }
        self.remove(id)?;
        Ok(true)
    }

    /// Deletes the draft, editable or not, leaving its copy on the server to the next
    /// upload pass to find and delete, like that of an upload of it still to settle.
    pub(crate) fn remove(&self, id: DraftId) -> Result<()> {
        self.orphan_copy(id)?;
        self.conn
            .execute("DELETE FROM drafts WHERE id = ?1", [id.0])?;
        Ok(())
    }

    /// Hands the draft's copy on the server, if it has one, to the next upload pass to
    /// find by its Message-ID and delete, as an upload without a draft. Its UID can't be
    /// used: the folder may have a new UIDVALIDITY that no sync has seen yet.
    fn orphan_copy(&self, id: DraftId) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT INTO draft_uploads (account_id, draft_id, folder_id, message_id_hdr,
                                            revision)
                 SELECT account_id, NULL, remote_folder_id, remote_message_id, remote_revision
                 FROM drafts WHERE id = ?1 AND remote_folder_id IS NOT NULL",
            )?
            .execute([id.0])?;
        Ok(())
    }

    /// Whether the account has uploads to settle, or drafts to upload and a Drafts
    /// folder to upload them to.
    pub fn need_upload(&self, account: AccountId) -> Result<bool> {
        let (unsettled, stale): (bool, bool) = self
            .conn
            .prepare_cached(&format!(
                "SELECT EXISTS (SELECT 1 FROM draft_uploads WHERE account_id = ?1),
                        EXISTS ({STALE})"
            ))?
            .query_row([account.0], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(unsettled
            || stale
                && Folders::new(self.conn)
                    .by_role(account, FolderRole::Drafts)?
                    .is_some())
    }

    /// Drafts to upload: their copy on the server is missing or out of date, they are
    /// not being sent, and no upload of theirs is left to settle. Least recently
    /// changed first.
    pub fn stale(&self, account: AccountId) -> Result<Vec<DraftId>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("{STALE} ORDER BY d.updated_at, d.id"))?;
        let rows = stmt.query_map([account.0], |r| Ok(DraftId(r.get(0)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The account's uploads still to settle, oldest first.
    pub fn uploads(&self, account: AccountId) -> Result<Vec<DraftUpload>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("{UPLOADS} WHERE u.account_id = ?1 ORDER BY u.id"))?;
        let rows = stmt.query_map([account.0], upload_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records an upload of `revision` of the draft to `folder` (of its account, named
    /// `remote_name` on the server) before the APPEND is sent. `None` if the draft is
    /// gone, being sent or changed since.
    pub fn begin_upload(
        &self,
        draft: DraftId,
        revision: i64,
        (folder, remote_name): (FolderId, &str),
        message_id: &str,
    ) -> Result<Option<DraftUpload>> {
        Ok(self
            .conn
            .prepare_cached(
                "INSERT INTO draft_uploads (account_id, draft_id, folder_id, message_id_hdr,
                                            revision)
                 SELECT d.account_id, d.id, ?3, ?4, d.revision FROM drafts d
                 WHERE d.id = ?1 AND d.revision = ?2
                   AND NOT EXISTS (SELECT 1 FROM outbox o WHERE o.draft_id = d.id)
                 RETURNING id, account_id",
            )?
            .query_row(params![draft.0, revision, folder.0, message_id], |r| {
                Ok(DraftUpload {
                    id: r.get(0)?,
                    account: AccountId(r.get(1)?),
                    draft: Some(draft),
                    folder,
                    remote_name: remote_name.to_owned(),
                    message_id: message_id.to_owned(),
                    revision,
                })
            })
            .optional()?)
    }

    /// Settles an upload: the server has its message at `uids` under `uidvalidity`, which
    /// it just reported (searched by its Message-ID; none if the APPEND didn't go
    /// through). The newest becomes the draft's copy, and the others are queued for
    /// deletion; all of them if the draft is gone, or has a newer copy already. The copy
    /// it replaces is queued for deletion too if it is of that same generation, else
    /// looked for by its Message-ID first ([`orphan_copy`](Self::orphan_copy)). Returns
    /// the folders whose messages or ops changed.
    pub fn uploaded(
        &self,
        upload: &DraftUpload,
        uidvalidity: u32,
        uids: &[u32],
        now: i64,
    ) -> Result<Touched> {
        self.conn
            .execute("DELETE FROM draft_uploads WHERE id = ?1", [upload.id])?;
        let mut delete: Vec<(FolderId, u32, u32)> = uids
            .iter()
            .map(|&uid| (upload.folder, uidvalidity, uid))
            .collect();
        delete.sort_unstable();
        // The draft's copy now and its revision, if the draft is still there.
        let current = match upload.draft {
            Some(draft) => self
                .conn
                .query_row(
                    "SELECT remote_folder_id, remote_uidvalidity, remote_uid, remote_revision
                     FROM drafts WHERE id = ?1",
                    [draft.0],
                    |r| {
                        let copy = match r.get::<_, Option<i64>>(0)? {
                            Some(folder) => Some((FolderId(folder), r.get(1)?, r.get(2)?)),
                            None => None,
                        };
                        Ok((draft, copy, r.get::<_, Option<i64>>(3)?))
                    },
                )
                .optional()?,
            None => None,
        };
        if let Some((draft, copy, revision)) = current
            && revision.is_none_or(|r| r <= upload.revision)
            && let Some((_, _, uid)) = delete.pop()
        {
            match copy {
                Some(copy) if copy.0 == upload.folder && copy.1 == uidvalidity => delete.push(copy),
                Some(_) => self.orphan_copy(draft)?,
                None => {}
            }
            self.conn
                .prepare_cached(
                    "UPDATE drafts SET remote_folder_id = ?2, remote_uidvalidity = ?3,
                                       remote_uid = ?4, remote_message_id = ?5,
                                       remote_revision = ?6, upload_error = NULL
                     WHERE id = ?1",
                )?
                .execute(params![
                    draft.0,
                    upload.folder.0,
                    uidvalidity,
                    uid,
                    upload.message_id,
                    upload.revision
                ])?;
        }
        let ops = PendingOps::new(self.conn);
        let mut touched = Touched::new();
        for (folder, uidvalidity, uid) in delete {
            touched.extend(ops.delete_on_server(upload.account, folder, uidvalidity, uid, now)?);
        }
        Ok(touched)
    }

    /// Forgets the copies in `folder` that aren't of generation `keep`: their UIDs are
    /// meaningless once the folder is gone or has a new UIDVALIDITY, and the drafts are
    /// uploaded again. In a new generation a copy is likely still there under another
    /// UID: it becomes an upload to settle, which looks for it by its Message-ID.
    pub(crate) fn end_generation(&self, folder: FolderId, keep: Option<u32>) -> Result<()> {
        if keep.is_some() {
            self.conn.execute(
                "INSERT INTO draft_uploads (account_id, draft_id, folder_id, message_id_hdr,
                                            revision)
                 SELECT account_id, id, remote_folder_id, remote_message_id, remote_revision
                 FROM drafts WHERE remote_folder_id = ?1 AND remote_uidvalidity IS NOT ?2
                 ORDER BY id",
                params![folder.0, keep],
            )?;
        }
        self.conn.execute(
            "UPDATE drafts
             SET remote_folder_id = NULL, remote_uidvalidity = NULL, remote_uid = NULL,
                 remote_message_id = NULL, remote_revision = NULL
             WHERE remote_folder_id = ?1 AND remote_uidvalidity IS NOT ?2",
            params![folder.0, keep],
        )?;
        Ok(())
    }

    /// The server refused the upload, which so stored nothing.
    pub fn upload_refused(&self, upload: &DraftUpload, error: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM draft_uploads WHERE id = ?1", [upload.id])?;
        match upload.draft {
            Some(draft) => self.upload_failed(draft, error),
            None => Ok(()),
        }
    }

    /// Records why the draft couldn't be uploaded; the next upload that succeeds clears
    /// it.
    pub fn upload_failed(&self, draft: DraftId, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE drafts SET upload_error = ?2 WHERE id = ?1",
            params![draft.0, error],
        )?;
        Ok(())
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

const UPLOADS: &str = "SELECT u.id, u.account_id, u.draft_id, u.folder_id, f.remote_name,
        u.message_id_hdr, u.revision
     FROM draft_uploads u JOIN folders f ON f.id = u.folder_id";

fn upload_from_row(r: &Row<'_>) -> rusqlite::Result<DraftUpload> {
    Ok(DraftUpload {
        id: r.get(0)?,
        account: AccountId(r.get(1)?),
        draft: r.get::<_, Option<i64>>(2)?.map(DraftId),
        folder: FolderId(r.get(3)?),
        remote_name: r.get(4)?,
        message_id: r.get(5)?,
        revision: r.get(6)?,
    })
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
        on_server: row.get("on_server")?,
        upload_error: row.get("upload_error")?,
        updated_at: row.get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use catmail_types::{Address, FlagState, OutboxStatus};

    use super::*;
    use crate::folders::RemoteFolder;
    use crate::messages::NewMessage;
    use crate::ops::ReplayKind;
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
        assert_eq!(store.read(|db| db.drafts().count(account)).unwrap(), 0);
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
        assert_eq!(store.read(|db| db.drafts().count(account)).unwrap(), 1);
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

    /// An account with a Drafts folder at UIDVALIDITY 7, and a draft in it.
    fn with_drafts_folder() -> (Store, AccountId, FolderId, DraftId) {
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        let (folder, draft) = store
            .write(|db| {
                let remote = RemoteFolder {
                    remote_name: "Drafts".into(),
                    name: "Drafts".into(),
                    delimiter: Some('/'),
                    role: Some(FolderRole::Drafts),
                    selectable: true,
                };
                db.folders().reconcile(account, &[remote])?;
                let folder = db.folders().list(account)?[0].id;
                db.folders().reset_uidvalidity(folder, 7)?;
                Ok((
                    folder,
                    db.drafts().insert(account, &content("one"), None, 1)?,
                ))
            })
            .unwrap();
        (store, account, folder, draft)
    }

    fn begin(store: &Store, draft: DraftId, folder: FolderId, message_id: &str) -> DraftUpload {
        store
            .write(|db| {
                let revision = db.drafts().snapshot(draft)?.unwrap().revision;
                db.drafts()
                    .begin_upload(draft, revision, (folder, "Drafts"), message_id)
            })
            .unwrap()
            .unwrap()
    }

    fn settle(store: &Store, upload: &DraftUpload, uidvalidity: u32, uids: &[u32]) -> Touched {
        store
            .write(|db| db.drafts().uploaded(upload, uidvalidity, uids, 5))
            .unwrap()
    }

    /// UIDs queued for deletion in the folder.
    fn deletions(store: &Store, folder: FolderId) -> Vec<u32> {
        store
            .read(|db| db.ops().pending(folder))
            .unwrap()
            .into_iter()
            .filter(|op| op.kind == ReplayKind::Delete)
            .map(|op| op.uid)
            .collect()
    }

    fn get(store: &Store, draft: DraftId) -> Draft {
        store.read(|db| db.drafts().get(draft)).unwrap().unwrap()
    }

    fn stale(store: &Store, account: AccountId) -> (Vec<DraftId>, bool) {
        store
            .read(|db| {
                Ok((
                    db.drafts().stale(account)?,
                    db.drafts().need_upload(account)?,
                ))
            })
            .unwrap()
    }

    #[test]
    fn uploads_replace_the_copy_on_the_server() {
        let (store, account, folder, draft) = with_drafts_folder();
        assert_eq!(stale(&store, account), (vec![draft], true));
        assert!(!get(&store, draft).on_server);

        let first = begin(&store, draft, folder, "one@x");
        assert_eq!(first.draft, Some(draft));
        assert_eq!(
            stale(&store, account),
            (vec![], true),
            "settled before uploading again"
        );
        assert!(settle(&store, &first, 7, &[5]).is_empty());
        assert!(get(&store, draft).on_server);
        assert_eq!(stale(&store, account), (vec![], false));
        assert!(deletions(&store, folder).is_empty());

        store
            .write(|db| db.drafts().update(draft, &content("two"), 2))
            .unwrap();
        assert!(!get(&store, draft).on_server);
        assert_eq!(stale(&store, account), (vec![draft], true));
        let second = begin(&store, draft, folder, "two@x");
        // A sync stored the first copy meanwhile.
        store
            .write(|db| {
                let copy = NewMessage {
                    state: FlagState {
                        uid: 5,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                db.messages().upsert(account, folder, &[copy])
            })
            .unwrap();
        let touched = settle(&store, &second, 7, &[6]);
        assert_eq!(touched, Touched::from([(account, folder)]));
        assert_eq!(store.read(|db| db.messages().count(folder)).unwrap(), 0);
        assert_eq!(deletions(&store, folder), [5], "the copy it replaced");
        assert!(get(&store, draft).on_server);

        // Deleted, the draft leaves its copy to be found by its Message-ID: no sync may
        // have seen the folder's current UIDVALIDITY.
        assert!(store.write(|db| db.drafts().delete(draft)).unwrap());
        let orphans: Vec<(Option<DraftId>, String)> = store
            .read(|db| db.drafts().uploads(account))
            .unwrap()
            .into_iter()
            .map(|u| (u.draft, u.message_id))
            .collect();
        assert_eq!(orphans, [(None, "two@x".into())]);
        assert_eq!(deletions(&store, folder), [5]);
    }

    #[test]
    fn a_copy_of_another_generation_is_found_by_its_message_id() {
        // The folder got a new UIDVALIDITY that no sync has seen, and the draft changed:
        // the APPEND reports the new generation, under which UID 5 means nothing.
        let (store, account, folder, draft) = with_drafts_folder();
        settle(&store, &begin(&store, draft, folder, "one@x"), 7, &[5]);
        store
            .write(|db| db.drafts().update(draft, &content("two"), 2))
            .unwrap();
        settle(&store, &begin(&store, draft, folder, "two@x"), 9, &[1]);
        assert!(get(&store, draft).on_server);
        assert!(deletions(&store, folder).is_empty());
        let orphan = store.read(|db| db.drafts().uploads(account)).unwrap();
        assert_eq!(
            orphan
                .iter()
                .map(|u| (u.draft, u.message_id.as_str()))
                .collect::<Vec<_>>(),
            [(None, "one@x")]
        );
        // Found in the new generation, and deleted there.
        settle(&store, &orphan[0], 9, &[2]);
        let ops = store.read(|db| db.ops().list(Some(account))).unwrap();
        assert_eq!(ops.len(), 1, "nothing under the old generation: {ops:?}");
        assert_eq!(deletions(&store, folder), [2]);
    }

    #[test]
    fn uploads_of_drafts_gone_since_delete_what_they_made() {
        let (store, account, folder, draft) = with_drafts_folder();
        begin(&store, draft, folder, "one@x");
        store.write(|db| db.drafts().delete(draft)).unwrap();
        let orphan = &store.read(|db| db.drafts().uploads(account)).unwrap()[0];
        assert_eq!(orphan.draft, None);
        assert_eq!(stale(&store, account), (vec![], true));
        settle(&store, orphan, 7, &[9, 8]);
        assert_eq!(deletions(&store, folder), [8, 9]);
        assert_eq!(stale(&store, account), (vec![], false));
    }

    #[test]
    fn an_older_upload_never_replaces_a_newer_copy() {
        let (store, _, folder, draft) = with_drafts_folder();
        let older = begin(&store, draft, folder, "one@x");
        store
            .write(|db| db.drafts().update(draft, &content("two"), 2))
            .unwrap();
        let newer = begin(&store, draft, folder, "two@x");
        settle(&store, &newer, 7, &[11]);
        settle(&store, &older, 7, &[10]);
        assert!(get(&store, draft).on_server);
        assert_eq!(deletions(&store, folder), [10]);
    }

    #[test]
    fn uploads_need_the_revision_and_no_send_under_way() {
        let (store, account, folder, draft) = with_drafts_folder();
        let begin_at = |revision| {
            store
                .write(|db| {
                    db.drafts()
                        .begin_upload(draft, revision, (folder, "Drafts"), "x@x")
                })
                .unwrap()
        };
        assert!(begin_at(1).is_none(), "changed since");

        let refused = begin_at(0).unwrap();
        store
            .write(|db| db.drafts().upload_refused(&refused, "NO [OVERQUOTA]"))
            .unwrap();
        let draft_now = get(&store, draft);
        assert_eq!(draft_now.upload_error.as_deref(), Some("NO [OVERQUOTA]"));
        assert!(!draft_now.on_server);
        assert_eq!(stale(&store, account), (vec![draft], true), "tried again");
        settle(&store, &begin_at(0).unwrap(), 7, &[3]);
        assert_eq!(get(&store, draft).upload_error, None);

        store
            .write(|db| db.drafts().update(draft, &content("two"), 2))
            .unwrap();
        queue(&store, account, draft).unwrap();
        assert!(begin_at(1).is_none(), "being sent");
        assert_eq!(stale(&store, account), (vec![], false));
    }

    #[test]
    fn without_a_drafts_folder_there_is_nothing_to_upload_to() {
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        let draft = store
            .write(|db| db.drafts().insert(account, &content("one"), None, 1))
            .unwrap();
        assert_eq!(stale(&store, account), (vec![draft], false));
    }

    #[test]
    fn a_new_generation_looks_for_the_copy_again() {
        let (store, account, folder, draft) = with_drafts_folder();
        settle(&store, &begin(&store, draft, folder, "one@x"), 7, &[5]);
        store
            .write(|db| db.folders().reset_uidvalidity(folder, 9))
            .unwrap();
        assert!(!get(&store, draft).on_server);
        let again = store.read(|db| db.drafts().uploads(account)).unwrap();
        assert_eq!(
            again
                .iter()
                .map(|u| (u.draft, u.message_id.as_str(), u.revision))
                .collect::<Vec<_>>(),
            [(Some(draft), "one@x", 0)]
        );
        assert_eq!(stale(&store, account), (vec![], true));
        settle(&store, &again[0], 9, &[1]);
        assert!(get(&store, draft).on_server, "found, not uploaded again");
        assert!(deletions(&store, folder).is_empty());
    }

    #[test]
    fn sending_deletes_the_copy() {
        let (store, account, folder, draft) = with_drafts_folder();
        settle(&store, &begin(&store, draft, folder, "one@x"), 7, &[5]);
        let outbox = queue(&store, account, draft).unwrap();
        assert!(
            store
                .write(|db| {
                    db.outbox().begin_send(outbox, 2)?;
                    db.outbox().mark_accepted(outbox, 2)?;
                    db.outbox().finish(outbox)
                })
                .unwrap()
        );
        assert!(store.read(|db| db.drafts().get(draft)).unwrap().is_none());
        let orphan = &store.read(|db| db.drafts().uploads(account)).unwrap()[0];
        assert_eq!(orphan.draft, None);
        settle(&store, orphan, 7, &[5]);
        assert_eq!(deletions(&store, folder), [5]);
    }
}
