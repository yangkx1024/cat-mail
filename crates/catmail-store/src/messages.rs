use std::collections::HashMap;
use std::sync::LazyLock;

use catmail_types::{
    AccountId, Address, BlobHash, FlagState, Flags, FolderId, Message, MessageBody, MessageCursor,
    MessageId, MessageSummary, SearchQuery,
};
use rusqlite::{Connection, OptionalExtension, Row, named_params, params};

use crate::Result;
use crate::attachments::{Attachments, NewAttachment};
use crate::columns::{id_list, json, parse_opt_text, to_id_list, to_json};
use crate::fts;
use crate::ops::MOVING;

const SUMMARY_COLUMNS: &str = "id, account_id, folder_id, thread_id, subject, from_json, date, snippet, flags, has_attachments";

/// The order of lists and search results, as [`MessageCursor`] describes it.
/// `list_date` is `date` with a missing one sorted last.
const NEWEST_FIRST: &str = "list_date DESC, id DESC";

/// The rows after the cursor bound by [`cursor_params`], in [`NEWEST_FIRST`] order. Only
/// for search, which sorts all its matches anyway: in an index, SQLite seeks on
/// `list_date` alone and steps over every newer id of the cursor's date.
const AFTER: &str = "(list_date, id) < (:after_date, :after_id)";

/// A page of a folder ([`Messages::list`]) is two index seeks: the rest of the cursor's
/// date, then the older dates. With [`AFTER`], each page of a large group of equal dates
/// (or of messages without one) would step over the whole group above it.
static LIST_SAME_DATE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT {SUMMARY_COLUMNS} FROM messages
         WHERE folder_id = :folder AND list_date = :after_date AND id < :after_id
         ORDER BY id DESC LIMIT :limit"
    )
});

/// See [`LIST_SAME_DATE`].
static LIST_OLDER: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT {SUMMARY_COLUMNS} FROM messages
         WHERE folder_id = :folder AND list_date < :after_date
         ORDER BY {NEWEST_FIRST} LIMIT :limit"
    )
});

/// `:after_date` and `:after_id`; without a cursor, before every row.
fn cursor_params(cursor: Option<MessageCursor>) -> (i64, i64) {
    match cursor {
        // What `list_date` holds for a missing date.
        Some(c) => (c.date.unwrap_or(i64::MIN), c.id.0),
        None => (i64::MAX, i64::MAX),
    }
}

/// A message as first seen during sync: header summary plus its current flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewMessage {
    pub state: FlagState,
    pub internal_date: Option<i64>,
    pub size: Option<u32>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub subject: Option<String>,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    /// The Date header; INTERNALDATE is used when it is missing.
    pub date: Option<i64>,
    pub has_attachments: bool,
}

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
}

/// Where a message lives on the server, and what of it is stored locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageLocation {
    pub id: MessageId,
    pub account: AccountId,
    /// Where the message shows locally. For a message moved here and not yet on the
    /// server, the rest describes the folder it is moving from.
    pub folder: FolderId,
    /// The name on the server of the folder that has the message.
    pub remote_name: String,
    /// That folder's current UIDVALIDITY; `uid` is only meaningful within it.
    pub uidvalidity: Option<u32>,
    pub uid: u32,
    /// RFC822.SIZE.
    pub size: Option<u32>,
    /// Whether the body is stored.
    pub downloaded: bool,
    pub raw: Option<BlobHash>,
}

pub struct Messages<'c> {
    conn: &'c Connection,
}

impl<'c> Messages<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// UIDs stored for `folder`, ascending.
    pub fn uids(&self, folder: FolderId) -> Result<Vec<u32>> {
        self.uids_from(folder, 0)
    }

    /// UIDs from `first` on, ascending.
    pub fn uids_from(&self, folder: FolderId, first: u32) -> Result<Vec<u32>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT uid FROM messages WHERE folder_id = ?1 AND uid >= ?2 ORDER BY uid",
        )?;
        let rows = stmt.query_map(params![folder.0, first], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn count(&self, folder: FolderId) -> Result<u32> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM messages WHERE folder_id = ?1",
            [folder.0],
            |r| r.get(0),
        )?)
    }

    /// Inserts messages, or refreshes the flags of ones already stored (a repeated batch
    /// after an interrupted sync). Immutable header fields are never rewritten. A
    /// message moved or deleted locally, whose op is still pending, is not brought back.
    /// Returns how many rows were new.
    pub fn upsert(
        &self,
        account: AccountId,
        folder: FolderId,
        messages: &[NewMessage],
    ) -> Result<usize> {
        let mut insert = self.conn.prepare_cached(
            "INSERT INTO messages (account_id, folder_id, uid, flags, keywords_json, modseq,
                                   internal_date, size, message_id_hdr, in_reply_to,
                                   references_hdr, subject, from_json, to_json, cc_json,
                                   reply_to_json, date, has_attachments)
             SELECT :account, :folder, :uid, :flags, :keywords, :modseq,
                    :internal_date, :size, :message_id, :in_reply_to,
                    :references, :subject, :from, :to, :cc, :reply_to, :date,
                    :has_attachments
             WHERE NOT EXISTS (
                 SELECT 1 FROM pending_ops
                 WHERE folder_id = :folder AND status = 'pending' AND uid = :uid
                   AND kind IN ('move', 'delete'))
             ON CONFLICT (folder_id, uid) DO NOTHING",
        )?;
        let mut inserted = 0;
        // Refreshed in one go: `update_flags` reads the folder's pending flag ops each call.
        let mut existing = Vec::new();
        for m in messages {
            let added = insert.execute(named_params! {
                ":account": account.0,
                ":folder": folder.0,
                ":uid": m.state.uid,
                ":flags": m.state.flags.bits(),
                ":keywords": to_json(&m.state.keywords),
                ":modseq": m.state.modseq.map(|v| v as i64),
                ":internal_date": m.internal_date,
                ":size": m.size,
                ":message_id": m.message_id,
                ":in_reply_to": m.in_reply_to,
                ":references": to_id_list(&m.references),
                ":subject": m.subject,
                ":from": to_json(&m.from),
                ":to": to_json(&m.to),
                ":cc": to_json(&m.cc),
                ":reply_to": to_json(&m.reply_to),
                ":date": m.date.or(m.internal_date),
                ":has_attachments": m.has_attachments,
            })?;
            if added > 0 {
                inserted += 1;
                self.reindex(self.conn.last_insert_rowid())?;
            } else {
                existing.push(m.state.clone());
            }
        }
        if !existing.is_empty() {
            self.update_flags(folder, &existing)?;
        }
        Ok(inserted)
    }

    /// Applies the server's flags to stored messages; UIDs not stored are ignored.
    /// Flag changes made locally and not yet replayed are applied on top, so a sync
    /// doesn't undo them. Returns how many rows' flags or keywords changed.
    pub fn update_flags(&self, folder: FolderId, states: &[FlagState]) -> Result<usize> {
        let mut local: HashMap<u32, Vec<(Flags, Flags)>> = HashMap::new();
        let mut pending = self.conn.prepare_cached(
            "SELECT uid, flags_add, flags_remove FROM pending_ops
             WHERE folder_id = ?1 AND status = 'pending' AND kind = 'flags' ORDER BY id",
        )?;
        for row in pending.query_map([folder.0], |r| {
            Ok((
                r.get(0)?,
                Flags::from_bits(r.get(1)?),
                Flags::from_bits(r.get(2)?),
            ))
        })? {
            let (uid, add, remove) = row?;
            local.entry(uid).or_default().push((add, remove));
        }
        let mut update = self.conn.prepare_cached(
            "UPDATE messages SET flags = :flags, keywords_json = :keywords, modseq = :modseq
             WHERE folder_id = :folder AND uid = :uid
               AND (flags, keywords_json) IS NOT (:flags, :keywords)",
        )?;
        // A new MODSEQ alone is no change worth counting (a message moved here locally
        // has none until the server reports it).
        let mut update_modseq = self.conn.prepare_cached(
            "UPDATE messages SET modseq = :modseq
             WHERE folder_id = :folder AND uid = :uid AND modseq IS NOT :modseq",
        )?;
        let mut changed = 0;
        for s in states {
            let flags = local.get(&s.uid).map_or(s.flags, |changes| {
                changes
                    .iter()
                    .fold(s.flags, |flags, &(add, remove)| flags.apply(add, remove))
            });
            let modseq = s.modseq.map(|v| v as i64);
            let updated = update.execute(named_params! {
                ":folder": folder.0,
                ":uid": s.uid,
                ":flags": flags.bits(),
                ":keywords": to_json(&s.keywords),
                ":modseq": modseq,
            })?;
            if updated == 0 && modseq.is_some() {
                update_modseq.execute(named_params! {
                    ":folder": folder.0,
                    ":uid": s.uid,
                    ":modseq": modseq,
                })?;
            }
            changed += updated;
        }
        Ok(changed)
    }

    /// Returns how many rows were deleted.
    pub fn delete_uids(&self, folder: FolderId, uids: &[u32]) -> Result<usize> {
        let mut delete = self
            .conn
            .prepare_cached("DELETE FROM messages WHERE folder_id = ?1 AND uid = ?2")?;
        let mut deleted = 0;
        for uid in uids {
            deleted += delete.execute(params![folder.0, uid])?;
        }
        Ok(deleted)
    }

    /// Newest first (see [`MessageCursor`]): the first `limit`, or the `limit` after
    /// `after`.
    pub fn list(
        &self,
        folder: FolderId,
        limit: u32,
        after: Option<MessageCursor>,
    ) -> Result<Vec<MessageSummary>> {
        let (after_date, after_id) = cursor_params(after);
        let mut same_date = self.conn.prepare_cached(&LIST_SAME_DATE)?;
        let mut page = same_date
            .query_map(
                named_params! {
                    ":folder": folder.0,
                    ":after_date": after_date,
                    ":after_id": after_id,
                    ":limit": limit,
                },
                summary_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let left = limit - page.len() as u32;
        if left > 0 {
            let mut older = self.conn.prepare_cached(&LIST_OLDER)?;
            let rows = older.query_map(
                named_params! {
                    ":folder": folder.0,
                    ":after_date": after_date,
                    ":limit": left,
                },
                summary_from_row,
            )?;
            page.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        Ok(page)
    }

    /// Messages matching the query, newest first (see [`MessageCursor`]). Bodies are only searched once
    /// downloaded; subjects and addresses always.
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<MessageSummary>> {
        let Some(expr) = fts::match_expression(&query.text) else {
            return Ok(Vec::new());
        };
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} FROM messages
             WHERE id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH :expr)
               AND (:account IS NULL OR account_id = :account)
               AND (:folder IS NULL OR folder_id = :folder) AND {AFTER}
             ORDER BY {NEWEST_FIRST} LIMIT :limit"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let (after_date, after_id) = cursor_params(query.after);
        let rows = stmt.query_map(
            named_params! {
                ":expr": expr,
                ":account": query.account.map(|a| a.0),
                ":folder": query.folder.map(|f| f.0),
                ":after_date": after_date,
                ":after_id": after_id,
                ":limit": query.limit,
            },
            summary_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `None` if the message is gone. A message moved locally is where the server has
    /// it until the move is made: in the folder its pending move is from.
    pub fn location(&self, id: MessageId) -> Result<Option<MessageLocation>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT m.account_id, m.folder_id, f.remote_name,
                            iif(o.id IS NULL, f.uidvalidity, o.uidvalidity),
                            coalesce(m.uid, o.uid), m.size, m.body_state = 'full',
                            b.raw_blob_hash
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
                        remote_name: r.get(2)?,
                        uidvalidity: r.get(3)?,
                        uid: r.get(4)?,
                        size: r.get(5)?,
                        downloaded: r.get(6)?,
                        raw: parse_opt_text(r, "raw_blob_hash")?,
                    })
                },
            )
            .optional()?)
    }

    /// The message with its full headers, and its body and attachments if downloaded.
    pub fn get(&self, id: MessageId) -> Result<Option<Message>> {
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS}, to_json, cc_json, reply_to_json, message_id_hdr,
                    references_hdr, size,
                    b.message_id IS NOT NULL AS has_body, b.text_plain, b.text_html
             FROM messages LEFT JOIN message_bodies b ON b.message_id = messages.id
             WHERE id = ?1"
        );
        let found = self
            .conn
            .query_row(&sql, [id.0], |row| {
                let body = row
                    .get::<_, bool>("has_body")?
                    .then(|| -> rusqlite::Result<_> {
                        Ok(MessageBody {
                            text: row.get("text_plain")?,
                            html: row.get("text_html")?,
                            attachments: Vec::new(),
                        })
                    })
                    .transpose()?;
                Ok(Message {
                    summary: summary_from_row(row)?,
                    to: json(row, "to_json")?,
                    cc: json(row, "cc_json")?,
                    reply_to: json(row, "reply_to_json")?,
                    message_id: row.get("message_id_hdr")?,
                    references: id_list(row, "references_hdr")?,
                    size: row.get("size")?,
                    body,
                })
            })
            .optional()?;
        let Some(mut message) = found else {
            return Ok(None);
        };
        if let Some(body) = &mut message.body {
            body.attachments = Attachments::new(self.conn).list(id)?;
        }
        Ok(Some(message))
    }

    /// Stores the body, its attachments, the snippet and the recomputed attachment flag,
    /// and indexes the body text. Returns false, changing nothing, if the message is
    /// gone or its body is already stored.
    pub fn store_body(&self, id: MessageId, body: &NewBody) -> Result<bool> {
        let has_attachments = body.attachments.iter().any(|a| !a.inline);
        let updated = self.conn.execute(
            "UPDATE messages SET body_state = 'full', snippet = ?2, has_attachments = ?3
             WHERE id = ?1 AND body_state <> 'full'",
            params![id.0, body.snippet, has_attachments],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        self.conn.execute(
            "INSERT INTO message_bodies (message_id, text_plain, text_html, raw_blob_hash)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                id.0,
                body.text,
                body.html,
                body.raw.as_ref().map(BlobHash::as_str)
            ],
        )?;
        Attachments::new(self.conn).insert(id, &body.attachments)?;

        self.reindex(id.0)?;
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
    fn reindex(&self, id: i64) -> Result<()> {
        let mut read = self.conn.prepare_cached(
            "SELECT m.subject, m.from_json, m.to_json, m.cc_json, b.text_plain
             FROM messages m LEFT JOIN message_bodies b ON b.message_id = m.id
             WHERE m.id = ?1",
        )?;
        // Subject, sender, recipients, body.
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

fn summary_from_row(row: &Row<'_>) -> rusqlite::Result<MessageSummary> {
    Ok(MessageSummary {
        id: MessageId(row.get("id")?),
        account_id: AccountId(row.get("account_id")?),
        folder_id: FolderId(row.get("folder_id")?),
        thread_id: row.get("thread_id")?,
        subject: row.get("subject")?,
        from: json(row, "from_json")?,
        date: row.get("date")?,
        snippet: row.get("snippet")?,
        flags: Flags::from_bits(row.get("flags")?),
        has_attachments: row.get("has_attachments")?,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use catmail_types::Folder;

    use super::*;
    use crate::Store;
    use crate::folders::RemoteFolder;
    use crate::test_util::{self, TestStore};

    fn setup() -> (TestStore, AccountId, Folder) {
        let store = test_util::store();
        let account = test_util::account(&store, "a@example.com");
        let inbox = RemoteFolder {
            remote_name: "INBOX".into(),
            name: "INBOX".into(),
            delimiter: None,
            role: None,
            selectable: true,
        };
        store
            .write(|db| db.folders().reconcile(account, &[inbox]))
            .unwrap();
        let folder = store
            .read(|db| db.folders().list(account))
            .unwrap()
            .remove(0);
        (store, account, folder)
    }

    fn message(uid: u32, date: Option<i64>, flags: Flags) -> NewMessage {
        NewMessage {
            state: FlagState {
                uid,
                flags,
                ..Default::default()
            },
            subject: Some(format!("subject {uid}")),
            from: vec![Address {
                name: Some("Alice".into()),
                email: "alice@example.com".into(),
            }],
            date,
            internal_date: Some(1_000 + uid as i64),
            ..Default::default()
        }
    }

    #[test]
    fn upsert_is_idempotent_and_refreshes_flags() {
        let (store, account, folder) = setup();
        let batch = vec![
            message(1, Some(500), Flags::SEEN),
            message(2, None, Flags::empty()),
            message(3, Some(3_000), Flags::empty()),
        ];
        assert_eq!(
            store
                .write(|db| db.messages().upsert(account, folder.id, &batch))
                .unwrap(),
            3
        );

        let mut again = batch.clone();
        again[1].state.flags = Flags::SEEN | Flags::FLAGGED;
        again[1].subject = Some("rewritten".into());
        assert_eq!(
            store
                .write(|db| db.messages().upsert(account, folder.id, &again))
                .unwrap(),
            0
        );

        let list = store
            .read(|db| db.messages().list(folder.id, 10, None))
            .unwrap();
        // UID 2 has no Date header and sorts by its INTERNALDATE (1002).
        let order: Vec<_> = list
            .iter()
            .map(|m| (m.subject.clone().unwrap(), m.date))
            .collect();
        assert_eq!(
            order,
            [
                ("subject 3".into(), Some(3_000)),
                ("subject 2".into(), Some(1_002)),
                ("subject 1".into(), Some(500)),
            ]
        );
        assert_eq!(
            list[1].flags,
            Flags::SEEN | Flags::FLAGGED,
            "flags refreshed"
        );
        assert_eq!(list[0].from[0].email, "alice@example.com");
        assert_eq!(
            store.read(|db| db.messages().uids(folder.id)).unwrap(),
            [1, 2, 3]
        );
    }

    #[test]
    fn pages_follow_the_last_row_across_equal_and_missing_dates() {
        let (store, account, folder) = setup();
        // Stored as ids 1 to 6.
        let dates = [
            Some(2_000),
            None,
            Some(3_000),
            Some(2_000),
            None,
            Some(1_000),
        ];
        let batch: Vec<_> = (1..)
            .zip(dates)
            .map(|(uid, date)| NewMessage {
                internal_date: None,
                ..message(uid, date, Flags::empty())
            })
            .collect();
        store
            .write(|db| db.messages().upsert(account, folder.id, &batch))
            .unwrap();
        let ids = |rows: Vec<MessageSummary>| rows.iter().map(|m| m.id.0).collect::<Vec<_>>();
        let list = |limit, after| {
            store
                .read(|db| db.messages().list(folder.id, limit, after))
                .unwrap()
        };
        let search = |limit, after| {
            let mut query = SearchQuery::new("subject");
            query.limit = limit;
            query.after = after;
            ids(store.read(|db| db.messages().search(&query)).unwrap())
        };

        // Newest first, equal dates by id, no date last.
        let all = list(10, None);
        let order = [3, 4, 1, 6, 5, 2];
        assert_eq!(ids(all.clone()), order);
        assert_eq!(search(10, None), order);
        for (ix, m) in all.iter().enumerate() {
            let page = ids(list(2, Some(m.cursor())));
            assert_eq!(
                page,
                order[ix + 1..].iter().take(2).copied().collect::<Vec<_>>(),
                "after {}",
                m.id.0
            );
            assert_eq!(search(2, Some(m.cursor())), page, "after {}", m.id.0);
        }

        // A newer message comes in and one already shown goes away: the next page still
        // starts right after the last row shown.
        store
            .write(|db| {
                db.messages().upsert(
                    account,
                    folder.id,
                    &[message(7, Some(5_000), Flags::empty())],
                )?;
                db.messages().delete_uids(folder.id, &[3])
            })
            .unwrap();
        assert_eq!(ids(list(2, Some(all[1].cursor()))), [1, 6]);
        assert_eq!(ids(list(10, None)), [7, 4, 1, 6, 5, 2]);
    }

    /// Paging through a large group of equal dates, each page seeks to its cursor: the
    /// last page is no more work than the first.
    #[test]
    fn a_page_seeks_to_its_cursor() {
        let (store, account, folder) = setup();
        let batch: Vec<_> = (1..=2_000)
            .map(|uid| message(uid, Some(1_000), Flags::empty()))
            .chain([message(2_001, Some(500), Flags::empty())])
            .collect();
        store
            .write(|db| db.messages().upsert(account, folder.id, &batch))
            .unwrap();
        // The first row stored, the oldest of the group: one row of it is left after
        // the second, then the older row.
        let second = MessageCursor {
            date: Some(1_000),
            id: MessageId(2),
        };
        let page = store
            .read(|db| db.messages().list(folder.id, 2, Some(second)))
            .unwrap();
        let ids: Vec<_> = page.iter().map(|m| m.id.0).collect();
        assert_eq!(ids, [1, 2_001]);

        // VM steps `list` takes, at the start and at the end of the group: its cached
        // statements keep counting across runs until reset.
        let work = |after| {
            store
                .read(|db| {
                    let steps = |sql: &str| -> Result<i32> {
                        let stmt = db.conn.prepare_cached(sql)?;
                        Ok(stmt.reset_status(rusqlite::StatementStatus::VmStep))
                    };
                    steps(&LIST_SAME_DATE)?;
                    steps(&LIST_OLDER)?;
                    db.messages().list(folder.id, 2, Some(after))?;
                    Ok(steps(&LIST_SAME_DATE)? + steps(&LIST_OLDER)?)
                })
                .unwrap()
        };
        let first = MessageCursor {
            date: Some(1_000),
            id: MessageId(2_000),
        };
        let (start, end) = (work(first), work(second));
        assert!(
            end <= start * 2,
            "{end} steps at the end, {start} at the start"
        );
    }

    #[test]
    fn flags_deletes_and_counts() {
        let (store, account, folder) = setup();
        let batch: Vec<_> = (1..=3)
            .map(|uid| message(uid, None, Flags::empty()))
            .collect();
        store
            .write(|db| db.messages().upsert(account, folder.id, &batch))
            .unwrap();

        let states = vec![
            FlagState {
                uid: 1,
                flags: Flags::SEEN,
                keywords: vec!["$Junk".into()],
                modseq: Some(5),
            },
            FlagState {
                uid: 2,
                flags: Flags::empty(),
                keywords: vec![],
                modseq: None,
            },
            FlagState {
                uid: 99,
                flags: Flags::SEEN,
                keywords: vec![],
                modseq: None,
            },
        ];
        assert_eq!(
            store
                .write(|db| db.messages().update_flags(folder.id, &states))
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .write(|db| db.messages().delete_uids(folder.id, &[3, 42]))
                .unwrap(),
            1
        );

        let folder = store
            .read(|db| db.folders().get(folder.id))
            .unwrap()
            .unwrap();
        assert_eq!((folder.total_count, folder.unread_count), (2, 1));
        let keywords: String = store
            .read(|db| {
                Ok(db.conn.query_row(
                    "SELECT keywords_json FROM messages WHERE uid = 1",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(keywords, r#"["$Junk"]"#);
    }

    fn hash(c: char) -> BlobHash {
        BlobHash::parse(&c.to_string().repeat(64)).unwrap()
    }

    fn body() -> NewBody {
        NewBody {
            text: Some("请查收第三季度报告。\nThe numbers are final.".into()),
            html: Some("<p>请查收</p>".into()),
            raw: Some(hash('a')),
            snippet: Some("请查收第三季度报告。".into()),
            attachments: vec![
                NewAttachment {
                    section: "2".into(),
                    transfer_encoding: Some("base64".into()),
                    filename: Some("报告.pdf".into()),
                    mime_type: "application/pdf".into(),
                    size: Some(10),
                    blob: Some(hash('b')),
                    ..Default::default()
                },
                NewAttachment {
                    section: "3".into(),
                    mime_type: "image/png".into(),
                    content_id: Some("logo@x".into()),
                    inline: true,
                    ..Default::default()
                },
            ],
        }
    }

    fn search(store: &Store, text: &str) -> Vec<Option<String>> {
        store
            .read(|db| db.messages().search(&SearchQuery::new(text)))
            .unwrap()
            .into_iter()
            .map(|m| m.subject)
            .collect()
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
            .read(|db| db.messages().list(folder.id, 1, None))
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
        assert_eq!(message.cc[0].email, "wang@example.com");
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
            .read(|db| db.attachments().source(png))
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
                .read(|db| db.attachments().source(png))
                .unwrap()
                .is_none()
        );
        assert!(search(&store, "quarterly").is_empty());
    }

    #[test]
    fn search_filters_by_account_and_folder() {
        let (store, account, folder) = setup();
        let other = test_util::account(&store, "b@example.com");
        let inbox = RemoteFolder {
            remote_name: "INBOX".into(),
            name: "INBOX".into(),
            delimiter: None,
            role: None,
            selectable: true,
        };
        store
            .write(|db| db.folders().reconcile(other, &[inbox]))
            .unwrap();
        let other_folder = store.read(|db| db.folders().list(other)).unwrap()[0].id;
        store
            .write(|db| {
                db.messages()
                    .upsert(account, folder.id, &[message(1, Some(1), Flags::empty())])?;
                db.messages()
                    .upsert(other, other_folder, &[message(7, Some(2), Flags::empty())])
            })
            .unwrap();

        let run = |account, folder| {
            let mut q = SearchQuery::new("alice");
            q.account = account;
            q.folder = folder;
            store
                .read(|db| db.messages().search(&q))
                .unwrap()
                .into_iter()
                .map(|m| m.subject.unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(run(None, None), ["subject 7", "subject 1"]);
        assert_eq!(run(Some(account), None), ["subject 1"]);
        assert_eq!(run(None, Some(other_folder)), ["subject 7"]);
        assert!(run(Some(account), Some(other_folder)).is_empty());
    }
}
