//! The lists of a folder or label, page by page, search, the messages read, and which
//! arrivals are new mail.

use std::sync::LazyLock;

use catmail_types::{
    AccountId, Flags, FolderId, FolderKind, FolderRole, ListFilter, ListQuery, Message,
    MessageBody, MessageCursor, MessageId, MessageSummary, SearchQuery,
};
use rusqlite::{OptionalExtension, Row, named_params};

use super::Messages;
use crate::Result;
use crate::attachments::Attachments;
use crate::columns::{id_list, json, parse_opt_text, parse_text, to_json};
use crate::fts;

/// Of `messages m`. Its labels are one lookup in the primary key of `message_labels`.
const SUMMARY_COLUMNS: &str = "m.id, m.account_id, m.folder_id, m.subject, m.from_json, m.to_json,
    m.cc_json, m.date,
    m.snippet, m.flags, m.has_attachments,
    (SELECT group_concat(folder_id) FROM message_labels WHERE message_id = m.id) AS labels";

/// Of `messages m`, those among `:ids` that are unread (`:seen` is `\Seen`) in one of
/// `:inboxes` (JSON arrays both). On Gmail INBOX is a label, which shows messages in All
/// Mail.
const UNREAD_IN_INBOX: &str = "m.id IN (SELECT value FROM json_each(:ids)) AND m.flags & :seen = 0
    AND (m.folder_id IN (SELECT value FROM json_each(:inboxes))
         OR (m.folder_id IN all_mail
             AND EXISTS (SELECT 1 FROM message_labels
                         WHERE message_id = m.id
                           AND folder_id IN (SELECT value FROM json_each(:inboxes)))))";

/// The order of lists and search results, as [`MessageCursor`] describes it.
/// `list_date` is `date` with a missing one sorted last.
const NEWEST_FIRST: &str = "list_date DESC, id DESC";

/// The rows after the cursor bound by [`cursor_params`], in [`NEWEST_FIRST`] order. Only
/// for search, which sorts all its matches anyway: in an index, SQLite seeks on
/// `list_date` alone and steps over every newer id of the cursor's date.
const AFTER: &str = "(list_date, id) < (:after_date, :after_id)";

/// Of `messages m`, those a [`ListFilter`] takes, as [`filter_params`] bind it. Without
/// a filter the first comparison holds for every row.
const FILTERED: &str = "(m.flags & :mask = :want OR m.id IN (SELECT value FROM json_each(:also)))";

/// `:mask`, `:want` and `:also`, for [`FILTERED`].
fn filter_params(filter: &ListFilter) -> (u32, u32, String) {
    let (mask, want) = filter.flags.mask();
    (mask.bits(), want.bits(), to_json(&filter.also))
}

/// A page of a folder ([`Messages::list`]) is two index seeks: the rest of the cursor's
/// date, then the older dates. With [`AFTER`], each page of a large group of equal dates
/// (or of messages without one) would step over the whole group above it.
struct Page {
    same_date: String,
    older: String,
}

impl Page {
    /// The rows of `from` that `filter` takes, newest first by its `date` and `id`
    /// columns: those of the index the page seeks in.
    fn new(from: &str, filter: &str, date: &str, id: &str) -> Self {
        Self {
            same_date: format!(
                "SELECT {SUMMARY_COLUMNS} FROM {from}
                 WHERE {filter} AND {FILTERED} AND {date} = :after_date AND {id} < :after_id
                 ORDER BY {id} DESC LIMIT :limit"
            ),
            older: format!(
                "SELECT {SUMMARY_COLUMNS} FROM {from}
                 WHERE {filter} AND {FILTERED} AND {date} < :after_date
                 ORDER BY {date} DESC, {id} DESC LIMIT :limit"
            ),
        }
    }
}

/// See [`Messages::new_mail`].
static NEW_MAIL: LazyLock<String> = LazyLock::new(|| {
    format!("SELECT m.id FROM messages m WHERE {UNREAD_IN_INBOX} AND m.uid >= :from ORDER BY m.id")
});

/// See [`Messages::unread_in_inbox`].
static STILL_NEW: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT {SUMMARY_COLUMNS} FROM messages m WHERE {UNREAD_IN_INBOX} ORDER BY {NEWEST_FIRST}"
    )
});

/// A location's messages, in `messages_folder_date`.
static LIST_FOLDER: LazyLock<Page> =
    LazyLock::new(|| Page::new("messages m", "m.folder_id = :key", "m.list_date", "m.id"));

/// A label's messages, in `message_labels_list`: those in All Mail.
static LIST_LABEL: LazyLock<Page> = LazyLock::new(|| {
    Page::new(
        "message_labels l JOIN messages m ON m.id = l.message_id",
        "l.folder_id = :key AND m.folder_id IN all_mail",
        "l.list_date",
        "l.message_id",
    )
});

/// Gmail's Starred, the account's flagged messages in All Mail, in `messages_flagged`.
static LIST_STARRED: LazyLock<Page> = LazyLock::new(|| {
    Page::new(
        "messages m",
        "m.account_id = :key AND m.flags & 2 <> 0 AND m.folder_id IN all_mail",
        "m.list_date",
        "m.id",
    )
});

/// The messages a search goes through, of `messages m`: on a standard account not those
/// of a folder that only gathers messages from the others (Dovecot's virtual `\All` and
/// `\Flagged`), each of which is also a row in its own folder. On Gmail All Mail is a
/// location, and Starred a label. A label or Starred searched on its own (`:folder`) is
/// what it lists.
const SEARCHED: &str = "
    (:folder IS NULL AND m.folder_id NOT IN gathering)
    OR m.folder_id = :folder
    OR (m.folder_id IN all_mail
        AND (EXISTS (SELECT 1 FROM message_labels
                     WHERE message_id = m.id AND folder_id = :folder)
             OR (m.flags & 2 <> 0
                 AND m.account_id IN (SELECT account_id FROM starred WHERE id = :folder))))";

/// `:after_date` and `:after_id`; without a cursor, before every row.
fn cursor_params(cursor: Option<MessageCursor>) -> (i64, i64) {
    match cursor {
        // What `list_date` holds for a missing date.
        Some(c) => (c.date.unwrap_or(i64::MIN), c.id.0),
        None => (i64::MAX, i64::MAX),
    }
}

impl Messages<'_> {
    /// Of the rows `ids`, the new mail to tell the user about: at UID `from_uid` or above
    /// (the folder's UIDNEXT before they arrived), and [`UNREAD_IN_INBOX`].
    pub fn new_mail(
        &self,
        ids: &[MessageId],
        from_uid: u32,
        inbox: FolderId,
    ) -> Result<Vec<MessageId>> {
        let mut stmt = self.conn.prepare_cached(&NEW_MAIL)?;
        let rows = stmt.query_map(
            named_params! {
                ":ids": to_json(ids),
                ":seen": Flags::SEEN.bits(),
                ":inboxes": to_json(&[inbox]),
                ":from": from_uid,
            },
            |r| Ok(MessageId(r.get(0)?)),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Of the rows `ids`, those still [`UNREAD_IN_INBOX`] of `inboxes`, newest first: of
    /// new mail announced a moment ago, what is still worth telling the user about.
    pub fn unread_in_inbox(
        &self,
        ids: &[MessageId],
        inboxes: &[FolderId],
    ) -> Result<Vec<MessageSummary>> {
        let mut stmt = self.conn.prepare_cached(&STILL_NEW)?;
        let rows = stmt.query_map(
            named_params! {
                ":ids": to_json(ids),
                ":seen": Flags::SEEN.bits(),
                ":inboxes": to_json(inboxes),
            },
            summary_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The page `query` asks for, newest first (see [`MessageCursor`]). A label lists the
    /// messages with it that are in All Mail; Starred, on Gmail, the flagged ones.
    pub fn list(&self, folder: FolderId, query: &ListQuery) -> Result<Vec<MessageSummary>> {
        let Some((account, kind, role)) = self
            .conn
            .prepare_cached("SELECT account_id, kind, role FROM folders WHERE id = ?1")?
            .query_row([folder.0], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    parse_text::<FolderKind>(r, "kind")?,
                    parse_opt_text::<FolderRole>(r, "role")?,
                ))
            })
            .optional()?
        else {
            return Ok(Vec::new());
        };
        let (sql, key) = match (kind, role) {
            (FolderKind::Location, _) => (&*LIST_FOLDER, folder.0),
            (FolderKind::Label, Some(FolderRole::Flagged)) => (&*LIST_STARRED, account),
            (FolderKind::Label, _) => (&*LIST_LABEL, folder.0),
        };
        let (after_date, after_id) = cursor_params(query.after);
        let (mask, want, also) = filter_params(&query.filter);
        let mut same_date = self.conn.prepare_cached(&sql.same_date)?;
        let mut page = same_date
            .query_map(
                named_params! {
                    ":key": key,
                    ":mask": mask,
                    ":want": want,
                    ":also": also,
                    ":after_date": after_date,
                    ":after_id": after_id,
                    ":limit": query.limit,
                },
                summary_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let left = query.limit - page.len() as u32;
        if left > 0 {
            let mut older = self.conn.prepare_cached(&sql.older)?;
            let rows = older.query_map(
                named_params! {
                    ":key": key,
                    ":mask": mask,
                    ":want": want,
                    ":also": also,
                    ":after_date": after_date,
                    ":limit": left,
                },
                summary_from_row,
            )?;
            page.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        Ok(page)
    }

    /// The messages of all of `folders` as one list, as [`list`](Self::list) pages each:
    /// the first `limit` of them all are among the first `limit` of each.
    pub fn list_merged(
        &self,
        folders: &[FolderId],
        query: &ListQuery,
    ) -> Result<Vec<MessageSummary>> {
        let mut merged = Vec::new();
        for &folder in folders {
            merged.extend(self.list(folder, query)?);
        }
        // `NEWEST_FIRST`, as the cursors bind it.
        merged.sort_unstable_by_key(|m| std::cmp::Reverse(cursor_params(Some(m.cursor()))));
        // Sorted by id within a date: a message two of the folders list is listed once.
        merged.dedup_by_key(|m| m.id);
        merged.truncate(query.limit as usize);
        Ok(merged)
    }

    /// Messages matching the query, newest first (see [`MessageCursor`]). Bodies are only
    /// searched once downloaded; subjects and addresses always. The query's
    /// `found_on_server` match as well, in its account and folder.
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<MessageSummary>> {
        let Some(expr) = fts::match_expression(&query.text) else {
            return Ok(Vec::new());
        };
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} FROM messages m
             WHERE (m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH :expr)
                    OR m.id IN (SELECT value FROM json_each(:found)))
               AND (:accounts IS NULL
                    OR m.account_id IN (SELECT value FROM json_each(:accounts)))
               AND ({SEARCHED})
               AND {FILTERED}
               AND {AFTER}
             ORDER BY {NEWEST_FIRST} LIMIT :limit"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let (after_date, after_id) = cursor_params(query.page.after);
        let (mask, want, also) = filter_params(&query.page.filter);
        let rows = stmt.query_map(
            named_params! {
                ":expr": expr,
                ":mask": mask,
                ":want": want,
                ":also": also,
                ":found": to_json(&query.found_on_server),
                ":accounts": query.accounts.as_deref().map(to_json),
                ":folder": query.folder.map(|f| f.0),
                ":after_date": after_date,
                ":after_id": after_id,
                ":limit": query.page.limit,
            },
            summary_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Those of `ids` that exist, in no particular order.
    pub fn summaries(&self, ids: &[MessageId]) -> Result<Vec<MessageSummary>> {
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} FROM messages m
             WHERE m.id IN (SELECT value FROM json_each(?1))"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([to_json(ids)], summary_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The message with its full headers, and its body and attachments if downloaded.
    pub fn get(&self, id: MessageId) -> Result<Option<Message>> {
        self.load(id, true)
    }

    /// The message with its full headers, without reading its body (`None` either way).
    pub fn headers(&self, id: MessageId) -> Result<Option<Message>> {
        self.load(id, false)
    }

    fn load(&self, id: MessageId, with_body: bool) -> Result<Option<Message>> {
        let body = if with_body {
            "b.message_id IS NOT NULL AS has_body, b.text_plain, b.text_html
             FROM messages m LEFT JOIN message_bodies b ON b.message_id = m.id"
        } else {
            "FALSE AS has_body FROM messages m"
        };
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS}, reply_to_json, message_id_hdr, references_hdr, size, {body}
             WHERE m.id = ?1"
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
}

fn summary_from_row(row: &Row<'_>) -> rusqlite::Result<MessageSummary> {
    Ok(MessageSummary {
        id: MessageId(row.get("id")?),
        account_id: AccountId(row.get("account_id")?),
        folder_id: FolderId(row.get("folder_id")?),
        subject: row.get("subject")?,
        from: json(row, "from_json")?,
        to: json(row, "to_json")?,
        cc: json(row, "cc_json")?,
        date: row.get("date")?,
        snippet: row.get("snippet")?,
        flags: Flags::from_bits(row.get("flags")?),
        has_attachments: row.get("has_attachments")?,
        labels: row
            .get::<_, Option<String>>("labels")?
            .map(|ids| {
                ids.split(',')
                    .filter_map(|id| id.parse().ok())
                    .map(FolderId)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use catmail_types::{
        AccountId, AccountMode, Flags, FolderId, FolderRole, GmailLabel, ListFilter, ListQuery,
        MessageCursor, MessageId, MessageSummary, SearchQuery,
    };

    use super::*;
    use crate::Store;
    use crate::messages::NewMessage;

    use crate::messages::fixtures::{body, copy, folders, inbox, labelled, message, setup, user};
    use crate::test_util::{self, gmail};

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
            let query = ListQuery {
                after,
                ..ListQuery::new(limit)
            };
            store
                .read(|db| db.messages().list(folder.id, &query))
                .unwrap()
        };
        let search = |limit, after| {
            let page = ListQuery {
                after,
                ..ListQuery::new(limit)
            };
            let query = SearchQuery::paged("subject", page);
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

    #[test]
    fn folders_listed_together_page_as_one_list() {
        let (store, account, folder) = setup();
        let other = test_util::account(&store, "b@example.com");
        let other_folder = inbox(&store, other).id;
        // Ids 1 to 3 in the first folder, 4 to 6 in the other.
        let batch = |dates: [Option<i64>; 3]| -> Vec<NewMessage> {
            (1..)
                .zip(dates)
                .map(|(uid, date)| NewMessage {
                    internal_date: None,
                    ..message(uid, date, Flags::empty())
                })
                .collect()
        };
        store
            .write(|db| {
                db.messages().upsert(
                    account,
                    folder.id,
                    &batch([Some(3_000), None, Some(1_000)]),
                )?;
                db.messages().upsert(
                    other,
                    other_folder,
                    &batch([Some(2_000), Some(3_000), None]),
                )
            })
            .unwrap();
        let folders = [folder.id, other_folder];
        let list = |limit, after| {
            let query = ListQuery {
                after,
                ..ListQuery::new(limit)
            };
            store
                .read(|db| db.messages().list_merged(&folders, &query))
                .unwrap()
        };
        let ids = |rows: &[MessageSummary]| rows.iter().map(|m| m.id.0).collect::<Vec<_>>();

        let all = list(10, None);
        let order = [5, 1, 4, 3, 6, 2];
        assert_eq!(ids(&all), order);
        for (ix, m) in all.iter().enumerate() {
            let page = list(2, Some(m.cursor()));
            let expected: Vec<_> = order[ix + 1..].iter().take(2).copied().collect();
            assert_eq!(ids(&page), expected, "after {}", m.id.0);
        }
        // A folder given twice lists its messages once.
        let twice = store
            .read(|db| {
                db.messages()
                    .list_merged(&[folder.id, folder.id], &ListQuery::new(10))
            })
            .unwrap();
        assert_eq!(ids(&twice), [1, 3, 2]);
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
        let query = ListQuery {
            after: Some(second),
            ..ListQuery::new(2)
        };
        let page = store
            .read(|db| db.messages().list(folder.id, &query))
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
                    steps(&LIST_FOLDER.same_date)?;
                    steps(&LIST_FOLDER.older)?;
                    let query = ListQuery {
                        after: Some(after),
                        ..ListQuery::new(2)
                    };
                    db.messages().list(folder.id, &query)?;
                    Ok(steps(&LIST_FOLDER.same_date)? + steps(&LIST_FOLDER.older)?)
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
    fn lists_and_searches_only_what_the_filter_takes() {
        use catmail_types::FlagFilter;

        let (store, account, folder) = setup();
        let batch = [
            message(1, Some(100), Flags::SEEN),
            message(2, Some(200), Flags::empty()),
            message(3, Some(300), Flags::SEEN | Flags::FLAGGED),
            message(4, Some(400), Flags::empty()),
        ];
        store
            .write(|db| db.messages().upsert(account, folder.id, &batch))
            .unwrap();
        let subjects = |messages: Vec<MessageSummary>| -> Vec<String> {
            messages.into_iter().map(|m| m.subject.unwrap()).collect()
        };
        let page = |filter: &ListFilter, limit, after| {
            let query = ListQuery {
                limit,
                after,
                filter: filter.clone(),
            };
            store
                .read(|db| db.messages().list(folder.id, &query))
                .unwrap()
        };
        let list = |filter: &ListFilter, limit, after| subjects(page(filter, limit, after));
        let only = |flags| list(&ListFilter::new(flags), 10, None);
        assert_eq!(only(FlagFilter::All).len(), 4);
        assert_eq!(only(FlagFilter::Unread), ["subject 4", "subject 2"]);
        assert_eq!(only(FlagFilter::Read), ["subject 3", "subject 1"]);
        assert_eq!(only(FlagFilter::Flagged), ["subject 3"]);
        assert_eq!(
            only(FlagFilter::Unflagged),
            ["subject 4", "subject 2", "subject 1"]
        );

        // Pages follow the rows the filter takes.
        let unread = ListFilter::new(FlagFilter::Unread);
        let first = page(&unread, 1, None);
        assert_eq!(list(&unread, 1, Some(first[0].cursor())), ["subject 2"]);

        // A row shown that was read since stays, in its place.
        let read_since = page(&ListFilter::default(), 10, None)
            .into_iter()
            .find(|m| m.subject.as_deref() == Some("subject 3"))
            .unwrap()
            .id;
        let kept = ListFilter {
            flags: FlagFilter::Unread,
            also: vec![read_since],
        };
        assert_eq!(
            list(&kept, 10, None),
            ["subject 4", "subject 3", "subject 2"]
        );
        let query = ListQuery {
            filter: kept,
            ..ListQuery::new(10)
        };
        let merged = store
            .read(|db| db.messages().list_merged(&[folder.id], &query))
            .unwrap();
        assert_eq!(subjects(merged).len(), 3);

        let mut query = SearchQuery::new("alice");
        query.page.filter = ListFilter::new(FlagFilter::Flagged);
        let found = store.read(|db| db.messages().search(&query)).unwrap();
        assert_eq!(subjects(found), ["subject 3"]);
    }

    #[test]
    fn search_filters_by_account_and_folder() {
        let (store, account, folder) = setup();
        let other = test_util::account(&store, "b@example.com");
        let other_folder = inbox(&store, other).id;
        store
            .write(|db| {
                db.messages()
                    .upsert(account, folder.id, &[message(1, Some(1), Flags::empty())])?;
                db.messages()
                    .upsert(other, other_folder, &[message(7, Some(2), Flags::empty())])
            })
            .unwrap();

        let run = |account: Option<AccountId>, folder| {
            let mut q = SearchQuery::new("alice");
            q.accounts = account.map(|a| vec![a]);
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

        let within = |accounts: Vec<AccountId>| {
            let mut q = SearchQuery::new("alice");
            q.accounts = Some(accounts);
            store.read(|db| db.messages().search(&q)).unwrap().len()
        };
        assert_eq!(within(vec![other]), 1);
        assert_eq!(within(vec![account, other]), 2);
        // Narrowed to none: nothing, not everything.
        assert_eq!(within(Vec::new()), 0);
    }

    #[test]
    fn summaries_of_the_messages_that_exist() {
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
        let found = store
            .read(|db| db.messages().summaries(&[id, MessageId(999)]))
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].id, found[0].account_id), (id, account));
    }

    #[test]
    fn search_finds_each_message_once_where_it_is() {
        let store = test_util::store();
        // Dovecot's virtual \All and \Flagged gather the messages of other folders.
        let standard = test_util::account(&store, "b@example.com");
        let o = folders(
            &store,
            standard,
            &[
                ("INBOX", Some(FolderRole::Inbox)),
                ("Kept", None),
                ("All", Some(FolderRole::All)),
                ("Flagged", Some(FolderRole::Flagged)),
            ],
        );
        let (gmail, g) = gmail(&store);
        store
            .write(|db| db.accounts().set_mode(gmail, AccountMode::Gmail))
            .unwrap();
        let put = |account, folder: FolderId, messages: &[NewMessage]| {
            store
                .write(|db| db.messages().upsert(account, folder, messages))
                .unwrap();
        };
        for folder in ["INBOX", "Kept", "All", "Flagged"] {
            put(standard, o[folder], &[copy(10, Some("same"))]);
        }
        let work = || vec![user("Work")];
        put(
            gmail,
            g["[Gmail]/All Mail"],
            &[
                labelled(20, Flags::empty(), work()),
                labelled(30, Flags::FLAGGED, vec![]),
            ],
        );
        put(
            gmail,
            g["[Gmail]/Trash"],
            &[labelled(40, Flags::FLAGGED, work())],
        );

        let names: HashMap<FolderId, &str> = g
            .iter()
            .map(|(&name, &id)| (id, name))
            .chain(o.iter().map(|(name, &id)| (id, name.as_str())))
            .collect();
        // Every page of `limit`, as (subject, folder).
        let run = |account, folder, limit| {
            let mut found = Vec::new();
            let mut after = None;
            loop {
                let mut q = SearchQuery::new("alice");
                (q.accounts, q.folder, q.page.limit, q.page.after) =
                    (Some(vec![account]), folder, limit, after);
                let page = store.read(|db| db.messages().search(&q)).unwrap();
                let Some(last) = page.last() else {
                    return found;
                };
                after = Some(last.cursor());
                found.extend(
                    page.iter()
                        .map(|m| (m.subject.clone().unwrap(), names[&m.folder_id])),
                );
            }
        };
        let s = |uid: u32, folder| (format!("subject {uid}"), folder);
        // Copies in two folders are two messages, but not those of a folder that only
        // gathers the others; unless that is where the search looks.
        let both = [s(10, "Kept"), s(10, "INBOX")];
        assert_eq!(run(standard, None, 50), both);
        assert_eq!(run(standard, None, 1), both);
        assert_eq!(run(standard, Some(o["All"]), 50), [s(10, "All")]);
        // On Gmail All Mail is where messages are; a label, or Starred, finds what it
        // lists.
        let all_mail = "[Gmail]/All Mail";
        let everything = [s(40, "[Gmail]/Trash"), s(30, all_mail), s(20, all_mail)];
        assert_eq!(run(gmail, None, 50), everything);
        assert_eq!(run(gmail, None, 1), everything);
        assert_eq!(run(gmail, Some(g["Work"]), 50), [s(20, all_mail)]);
        assert_eq!(
            run(gmail, Some(g["[Gmail]/Starred"]), 50),
            [s(30, all_mail)]
        );
    }

    #[test]
    fn what_the_server_found_is_searched_where_search_looks() {
        let store = test_util::store();
        let standard = test_util::account(&store, "b@example.com");
        let o = folders(
            &store,
            standard,
            &[
                ("INBOX", Some(FolderRole::Inbox)),
                ("Kept", None),
                ("All", Some(FolderRole::All)),
                ("Flagged", Some(FolderRole::Flagged)),
            ],
        );
        let (gmail, g) = gmail(&store);
        store
            .write(|db| {
                db.accounts().set_mode(gmail, AccountMode::Gmail)?;
                let messages = db.messages();
                messages.upsert(standard, o["INBOX"], &[copy(10, None), copy(11, None)])?;
                messages.upsert(standard, o["Kept"], &[copy(12, None)])?;
                messages.upsert(standard, o["All"], &[copy(13, None)])?;
                messages.upsert(
                    gmail,
                    g["[Gmail]/All Mail"],
                    &[labelled(20, Flags::empty(), vec![user("Work")])],
                )?;
                messages.upsert(
                    gmail,
                    g["[Gmail]/Trash"],
                    &[labelled(21, Flags::empty(), vec![])],
                )?;
                messages.upsert(
                    gmail,
                    g["[Gmail]/Spam"],
                    &[labelled(22, Flags::empty(), vec![])],
                )
            })
            .unwrap();
        let id = |folder, uid| test_util::message_id(&store, folder, uid);
        let (ten, eleven, twelve) = (id(o["INBOX"], 10), id(o["INBOX"], 11), id(o["Kept"], 12));
        let twenty = id(g["[Gmail]/All Mail"], 20);

        // UIDs the server found, as rows: not those with a body, nor of another
        // UIDVALIDITY, nor UIDs not stored.
        store
            .write(|db| db.messages().store_body(eleven, &body()))
            .unwrap();
        let without_body = |folder, uidvalidity, uids: &[u32], limit| {
            store
                .read(|db| db.messages().without_body(folder, uidvalidity, uids, limit))
                .unwrap()
        };
        assert_eq!(without_body(o["INBOX"], 1, &[10, 11, 99], 9), [ten]);
        assert!(without_body(o["INBOX"], 2, &[10], 9).is_empty());
        assert!(without_body(o["Kept"], 1, &[10], 9).is_empty());
        // The limit counts only what is left: a newer message with its body takes none.
        assert_eq!(without_body(o["INBOX"], 1, &[10, 11], 1), [ten]);

        // They match whatever the text, within the account and folder.
        let run = |account: Option<AccountId>, folder, found: Vec<MessageId>| {
            let mut q = SearchQuery::new("pelican");
            (q.accounts, q.folder, q.found_on_server) = (account.map(|a| vec![a]), folder, found);
            let found = store.read(|db| db.messages().search(&q)).unwrap();
            found.into_iter().map(|m| m.id).collect::<Vec<_>>()
        };
        let all = vec![ten, twelve, twenty];
        assert!(run(None, None, Vec::new()).is_empty());
        assert_eq!(run(None, None, all.clone()), [twenty, twelve, ten]);
        assert_eq!(run(Some(standard), None, all.clone()), [twelve, ten]);
        assert_eq!(run(None, Some(o["Kept"]), all.clone()), [twelve]);
        assert_eq!(run(None, Some(g["Work"]), all.clone()), [twenty]);
        assert!(run(None, Some(o["All"]), all).is_empty());

        // The server is asked about the folders search covers, by account.
        let searched = |account: Option<AccountId>, folder| {
            let found = store
                .read(|db| {
                    db.folders()
                        .searched_on_server(account.as_ref().map(std::slice::from_ref), folder)
                })
                .unwrap();
            found
                .into_iter()
                .map(|(account, states)| {
                    let names = states.into_iter().map(|s| s.remote_name).collect();
                    (account, names)
                })
                .collect::<Vec<(AccountId, Vec<String>)>>()
        };
        let names = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let locations = names(&["[Gmail]/All Mail", "[Gmail]/Trash", "[Gmail]/Spam"]);
        assert_eq!(
            searched(None, None),
            [
                (standard, names(&["INBOX", "Kept"])),
                (gmail, locations.clone())
            ]
        );
        assert_eq!(searched(Some(gmail), None), [(gmail, locations)]);
        assert_eq!(
            searched(None, Some(o["All"])),
            [(standard, names(&["All"]))]
        );
        assert_eq!(
            searched(Some(standard), Some(o["Kept"])),
            [(standard, names(&["Kept"]))]
        );
        assert!(searched(Some(gmail), Some(o["Kept"])).is_empty());
        let trash = names(&["[Gmail]/Trash"]);
        assert_eq!(searched(None, Some(g["[Gmail]/Trash"])), [(gmail, trash)]);
        let all_mail = names(&["[Gmail]/All Mail"]);
        assert_eq!(searched(None, Some(g["Work"])), [(gmail, all_mail.clone())]);
        assert_eq!(
            searched(None, Some(g["[Gmail]/Starred"])),
            [(gmail, all_mail)]
        );
        // Not where every body is here: local search finds whatever the server would.
        store
            .write(|db| db.messages().store_body(ten, &body()).map(drop))
            .unwrap();
        assert_eq!(
            searched(Some(standard), None),
            [(standard, names(&["Kept"]))]
        );
        // Nor before the folder was synced: there is nothing to map UIDs to.
        store
            .write(|db| {
                db.conn.execute(
                    "UPDATE folders SET uidvalidity = NULL WHERE id = ?1",
                    [o["Kept"].0],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(searched(Some(standard), None).is_empty());
    }

    fn inserted(
        store: &Store,
        account: AccountId,
        folder: FolderId,
        messages: &[NewMessage],
    ) -> Vec<MessageId> {
        store
            .write(|db| db.messages().upsert(account, folder, messages))
            .unwrap()
            .added
    }

    /// A page of a label or of Starred seeks in its index, as one of a folder does.
    #[test]
    fn label_pages_seek_in_their_index() {
        let store = test_util::store();
        store
            .read(|db| {
                for (page, index) in [
                    (&*LIST_LABEL, "message_labels_list"),
                    (&*LIST_STARRED, "messages_flagged"),
                    (&*LIST_FOLDER, "messages_folder_date"),
                ] {
                    for sql in [&page.same_date, &page.older] {
                        let mut stmt = db.conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                        for i in 1..=stmt.parameter_count() {
                            stmt.raw_bind_parameter(i, 1)?;
                        }
                        let mut rows = stmt.raw_query();
                        let mut plan = Vec::new();
                        while let Some(row) = rows.next()? {
                            plan.push(row.get::<_, String>(3)?);
                        }
                        let plan = plan.join("\n");
                        assert!(plan.contains(index), "{sql}\n{plan}");
                        assert!(!plan.contains("TEMP B-TREE"), "{sql}\n{plan}");
                    }
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn new_mail_is_unread_arrivals_in_inbox() {
        let store = test_util::store();
        let account = test_util::account(&store, "a@example.com");
        let f = folders(
            &store,
            account,
            &[("INBOX", Some(FolderRole::Inbox)), ("Archive", None)],
        );
        let (inbox, archive) = (f["INBOX"], f["Archive"]);
        let added = inserted(
            &store,
            account,
            inbox,
            &[
                message(4, None, Flags::empty()),
                message(5, None, Flags::empty()),
                message(6, None, Flags::SEEN),
                message(7, None, Flags::FLAGGED),
            ],
        );
        let elsewhere = inserted(
            &store,
            account,
            archive,
            &[message(8, None, Flags::empty())],
        );
        let new = store
            .read(|db| {
                db.messages()
                    .new_mail(&[added.clone(), elsewhere].concat(), 5, inbox)
            })
            .unwrap();
        // Not 4, from before UIDNEXT; not the read one, nor the one in Archive.
        assert_eq!(new, [added[1], added[3]]);
    }

    #[test]
    fn mail_still_new_is_unread_in_inbox_newest_first() {
        let store = test_util::store();
        let account = test_util::account(&store, "a@example.com");
        let f = folders(
            &store,
            account,
            &[("INBOX", Some(FolderRole::Inbox)), ("Archive", None)],
        );
        let (inbox, archive) = (f["INBOX"], f["Archive"]);
        let added = inserted(
            &store,
            account,
            inbox,
            &[
                message(4, Some(40), Flags::empty()),
                message(5, Some(50), Flags::SEEN),
                message(6, Some(60), Flags::empty()),
            ],
        );
        let elsewhere = inserted(
            &store,
            account,
            archive,
            &[message(7, Some(70), Flags::empty())],
        );
        let still_new = store
            .read(|db| {
                db.messages()
                    .unread_in_inbox(&[added.clone(), elsewhere].concat(), &[inbox])
            })
            .unwrap();
        let ids: Vec<_> = still_new.iter().map(|m| m.id).collect();
        assert_eq!(ids, [added[2], added[0]]);
    }

    #[test]
    fn new_mail_on_gmail_is_in_all_mail_with_the_inbox_label() {
        let store = test_util::store();
        let (account, f) = gmail(&store);
        let inbox = || GmailLabel::System(FolderRole::Inbox);
        let all_mail = inserted(
            &store,
            account,
            f["[Gmail]/All Mail"],
            &[
                labelled(1, Flags::empty(), vec![inbox(), user("Work")]),
                labelled(2, Flags::empty(), vec![user("Work")]),
            ],
        );
        let spam = inserted(
            &store,
            account,
            f["[Gmail]/Spam"],
            &[labelled(3, Flags::empty(), vec![inbox()])],
        );
        let new = store
            .read(|db| {
                db.messages()
                    .new_mail(&[all_mail.clone(), spam].concat(), 1, f["INBOX"])
            })
            .unwrap();
        assert_eq!(new, [all_mail[0]]);
    }
}
