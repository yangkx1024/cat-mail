use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use catmail_types::{
    AccountId, Address, BlobHash, FlagState, Flags, FolderId, FolderKind, FolderRole, GmailLabel,
    Message, MessageBody, MessageCursor, MessageId, MessageSummary, SearchQuery,
};
use rusqlite::{Connection, OptionalExtension, Row, named_params, params};

use crate::Result;
use crate::attachments::{Attachments, NewAttachment};
use crate::columns::{id_list, json, parse_opt_text, parse_text, to_id_list, to_json};
use crate::folders::Folders;
use crate::fts;
use crate::ops::MOVING;

/// Of `messages m`. Its labels are one lookup in the primary key of `message_labels`.
const SUMMARY_COLUMNS: &str = "m.id, m.account_id, m.folder_id, m.subject, m.from_json, m.date,
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
                 WHERE {filter} AND {date} = :after_date AND {id} < :after_id
                 ORDER BY {id} DESC LIMIT :limit"
            ),
            older: format!(
                "SELECT {SUMMARY_COLUMNS} FROM {from}
                 WHERE {filter} AND {date} < :after_date
                 ORDER BY {date} DESC, {id} DESC LIMIT :limit"
            ),
        }
    }
}

/// The folders that show message `id` besides its location, as (account, folder): its
/// labels, and Starred if it is flagged, or `flagged` (it was, or is about to be). None
/// on a standard account. A change to the message changes their lists too.
pub(crate) fn shown(
    conn: &Connection,
    id: i64,
    flagged: bool,
) -> Result<Vec<(AccountId, FolderId)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT account_id, folder_id FROM message_labels WHERE message_id = ?1
         UNION ALL
         SELECT s.account_id, s.id FROM messages m JOIN starred s ON s.account_id = m.account_id
         WHERE m.id = ?1 AND (?2 OR m.flags & 2 <> 0)",
    )?;
    let rows = stmt.query_map(params![id, flagged], |r| {
        Ok((AccountId(r.get(0)?), FolderId(r.get(1)?)))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
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
    (:folder IS NULL
     AND m.folder_id NOT IN (
         SELECT f.id FROM folders f JOIN accounts a ON a.id = f.account_id
         WHERE f.role IN ('all', 'flagged') AND a.mode IS NOT 'gmail'))
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
    /// X-GM-MSGID and X-GM-THRID, on Gmail.
    pub gm_msgid: Option<u64>,
    pub gm_thrid: Option<u64>,
}

/// What [`Messages::upsert`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Upserted {
    /// Rows inserted, in the order given.
    pub added: Vec<MessageId>,
    /// On Gmail, rows that moved here from another location, and the locations they
    /// left.
    pub moved: usize,
    pub moved_from: Vec<FolderId>,
    /// On Gmail, messages left out because their row is being moved locally from
    /// somewhere else: not stored here, though the server has them here.
    pub held: usize,
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
    /// The folder that has the message on the server.
    pub server_folder: FolderId,
    /// That folder's name on the server.
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
    /// The label folders (Starred too) whose lists the changes made through this have
    /// changed; see [`touched_labels`](Self::touched_labels).
    touched: RefCell<BTreeSet<FolderId>>,
}

impl<'c> Messages<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self {
            conn,
            touched: RefCell::default(),
        }
    }

    /// On Gmail, the label folders whose lists the changes made through this so far
    /// changed, besides the locations changed (a label shows messages of All Mail, Starred
    /// the flagged ones), and forgets them. Covers [`upsert`](Self::upsert),
    /// [`update_flags`](Self::update_flags) and [`delete_uids`](Self::delete_uids).
    pub fn touched_labels(&self) -> Vec<FolderId> {
        self.touched.take().into_iter().collect()
    }

    /// Notes the folders that show the message besides its location ([`shown`]).
    fn note_shown(&self, id: i64, flagged: bool) -> Result<()> {
        let shown = shown(self.conn, id, flagged)?;
        self.touched
            .borrow_mut()
            .extend(shown.into_iter().map(|(_, folder)| folder));
        Ok(())
    }

    /// Notes the folders that show the messages at `uids` in `folder` (all of them if
    /// `None`) besides their location, as [`shown`] does, in one query.
    fn note_shown_in(&self, folder: FolderId, uids: Option<&[u32]>) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "WITH m AS MATERIALIZED (
                 SELECT id, flags FROM messages
                 WHERE folder_id = ?1
                   AND (?2 IS NULL OR uid IN (SELECT value FROM json_each(?2))))
             SELECT DISTINCT folder_id FROM message_labels
             WHERE message_id IN (SELECT id FROM m)
             UNION
             SELECT s.id FROM folders f JOIN starred s ON s.account_id = f.account_id
             WHERE f.id = ?1 AND EXISTS (SELECT 1 FROM m WHERE flags & 2 <> 0)",
        )?;
        let uids = uids.map(to_json);
        let shown = stmt.query_map(params![folder.0, uids], |r| Ok(FolderId(r.get(0)?)))?;
        let mut touched = self.touched.borrow_mut();
        for folder in shown {
            touched.insert(folder?);
        }
        Ok(())
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
    ///
    /// On Gmail a row is one X-GM-MSGID: a message stored in another location (moved
    /// there by another client) moves here with its row, keeping its body, attachments
    /// and index entry. One whose row is being moved locally is left out (`held`): until
    /// the move is replayed, the row says where the server had it.
    pub fn upsert(
        &self,
        account: AccountId,
        folder: FolderId,
        messages: &[NewMessage],
    ) -> Result<Upserted> {
        let mut insert = self.conn.prepare_cached(
            "INSERT INTO messages (account_id, folder_id, uid, flags, keywords_json, modseq,
                                   gm_msgid, gm_thrid, internal_date, size, message_id_hdr,
                                   in_reply_to, references_hdr, subject, from_json, to_json,
                                   cc_json, reply_to_json, date, has_attachments)
             SELECT :account, :folder, :uid, :flags, :keywords, :modseq,
                    :gm_msgid, :gm_thrid, :internal_date, :size, :message_id,
                    :in_reply_to, :references, :subject, :from, :to, :cc, :reply_to, :date,
                    :has_attachments
             WHERE NOT EXISTS (
                 SELECT 1 FROM pending_ops
                 WHERE folder_id = :folder AND status = 'pending' AND uid = :uid
                   AND kind IN ('move', 'delete'))
             ON CONFLICT (folder_id, uid) DO NOTHING",
        )?;
        let mut by_gm_msgid = self.conn.prepare_cached(
            "SELECT id, folder_id, uid FROM messages WHERE account_id = ?1 AND gm_msgid = ?2",
        )?;
        let mut moving_from_here = self.conn.prepare_cached(
            "SELECT 1 FROM pending_ops
             WHERE folder_id = ?1 AND status = 'pending' AND uid = ?2 AND kind = 'move'",
        )?;
        let mut relocate = self
            .conn
            .prepare_cached("UPDATE messages SET folder_id = ?2, uid = ?3 WHERE id = ?1")?;
        let mut done = Upserted::default();
        let mut labels = LabelFolders::new(self.conn, account);
        // Refreshed in one go: `update_flags` reads the folder's pending flag ops each call.
        let mut existing = Vec::new();
        for m in messages {
            let uid = m.state.uid;
            let stored: Option<(i64, i64, Option<u32>)> = match m.gm_msgid {
                Some(gm_msgid) => by_gm_msgid
                    .query_row(params![account.0, gm_msgid as i64], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .optional()?,
                None => None,
            };
            match stored {
                Some((_, f, Some(u))) if (f, u) == (folder.0, uid) => {
                    existing.push(m.state.clone());
                    continue;
                }
                Some((_, _, None)) => {
                    // Moved here locally: this is where the move is from.
                    if !moving_from_here.exists(params![folder.0, uid])? {
                        done.held += 1;
                    }
                    continue;
                }
                Some((id, from, Some(_))) => {
                    // Its labels show it no more, or again; flags and labels as here are
                    // refreshed with the others.
                    self.note_shown(id, false)?;
                    relocate.execute(params![id, folder.0, uid])?;
                    done.moved += 1;
                    if !done.moved_from.contains(&FolderId(from)) {
                        done.moved_from.push(FolderId(from));
                    }
                    existing.push(m.state.clone());
                    continue;
                }
                None => {}
            }
            let added = insert.execute(named_params! {
                ":account": account.0,
                ":folder": folder.0,
                ":uid": uid,
                ":flags": m.state.flags.bits(),
                ":keywords": to_json(&m.state.keywords),
                ":modseq": m.state.modseq.map(|v| v as i64),
                ":gm_msgid": m.gm_msgid.map(|v| v as i64),
                ":gm_thrid": m.gm_thrid.map(|v| v as i64),
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
                let id = self.conn.last_insert_rowid();
                done.added.push(MessageId(id));
                self.reindex(id)?;
                // Its labels are noted as they are set; Starred shows it if flagged.
                if let Some(names) = &m.state.labels {
                    self.set_labels(id, &labels.ids(names)?)?;
                }
                if m.state.flags.contains(Flags::FLAGGED) {
                    self.note_shown(id, false)?;
                }
            } else {
                existing.push(m.state.clone());
            }
        }
        if !existing.is_empty() {
            self.update_flags(folder, &existing)?;
        }
        Ok(done)
    }

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

    /// Applies the server's flags, and labels where given, to stored messages; UIDs not
    /// stored are ignored. Flag changes made locally and not yet replayed are applied on
    /// top, so a sync doesn't undo them. Returns how many rows' flags, keywords or labels
    /// changed.
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
        let mut read = self.conn.prepare_cached(
            "SELECT id, flags, keywords_json, modseq FROM messages WHERE folder_id = ?1 AND uid = ?2",
        )?;
        let mut update = self.conn.prepare_cached(
            "UPDATE messages SET flags = ?2, keywords_json = ?3, modseq = ?4 WHERE id = ?1",
        )?;
        let mut labels = None;
        let mut changed = 0;
        for s in states {
            let stored: Option<(i64, u32, String, Option<i64>)> = read
                .query_row(params![folder.0, s.uid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?;
            let Some((id, old_flags, old_keywords, old_modseq)) = stored else {
                continue;
            };
            let flags = local.get(&s.uid).map_or(s.flags, |changes| {
                changes
                    .iter()
                    .fold(s.flags, |flags, &(add, remove)| flags.apply(add, remove))
            });
            let keywords = to_json(&s.keywords);
            let differs = (old_flags, &old_keywords) != (flags.bits(), &keywords);
            // A new MODSEQ alone is no change worth counting (a message moved here locally
            // has none until the server reports it).
            let modseq = match s.modseq {
                Some(m) => Some(m as i64),
                None if differs => None,
                None => old_modseq,
            };
            if differs {
                // Starred shows it before or after.
                let flagged = (Flags::from_bits(old_flags) | flags).contains(Flags::FLAGGED);
                self.note_shown(id, flagged)?;
            }
            if differs || modseq != old_modseq {
                update.execute(params![id, flags.bits(), keywords, modseq])?;
            }
            let relabelled = match &s.labels {
                Some(names) => {
                    let labels = match &mut labels {
                        Some(labels) => labels,
                        None => labels.insert(LabelFolders::of_folder(self.conn, folder)?),
                    };
                    self.set_labels(id, &labels.ids(names)?)?
                }
                None => false,
            };
            changed += (differs || relabelled) as usize;
        }
        Ok(changed)
    }

    /// Makes the labels of message `id` the label folders the server reports, with the
    /// changes still queued for it made on top (as for flags: the server doesn't have them
    /// yet), adding and removing only what differs, and notes those and the rest of its
    /// labels. Returns whether that changed any.
    fn set_labels(&self, id: i64, labels: &[FolderId]) -> Result<bool> {
        let mut labels: Vec<i64> = labels.iter().map(|f| f.0).collect();
        let mut queued = self.conn.prepare_cached(
            "SELECT target_folder_id, label_add FROM pending_ops
             WHERE message_id = ?1 AND kind = 'label' AND status = 'pending' ORDER BY id",
        )?;
        for change in queued.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (label, add): (i64, bool) = change?;
            labels.retain(|&l| l != label);
            if add {
                labels.push(label);
            }
        }
        let wanted = to_json(&labels);
        let mut remove = self.conn.prepare_cached(
            "DELETE FROM message_labels
             WHERE message_id = ?1 AND folder_id NOT IN (SELECT value FROM json_each(?2))
             RETURNING folder_id",
        )?;
        let mut add = self.conn.prepare_cached(
            "INSERT OR IGNORE INTO message_labels (message_id, folder_id, account_id, list_date)
             SELECT m.id, j.value, m.account_id, m.list_date
             FROM messages m, json_each(?2) j WHERE m.id = ?1
             RETURNING folder_id",
        )?;
        let mut changed = false;
        for stmt in [&mut remove, &mut add] {
            for folder in stmt.query_map(params![id, wanted], |r| Ok(FolderId(r.get(0)?)))? {
                self.touched.borrow_mut().insert(folder?);
                changed = true;
            }
        }
        // Its other labels list it, with its labels.
        if changed {
            self.note_shown(id, false)?;
        }
        Ok(changed)
    }

    /// Notes the folders that show any message of `folder` besides it, before a change to
    /// all of them that goes around this, as a new UIDVALIDITY makes; see
    /// [`touched_labels`](Self::touched_labels).
    pub fn note_folder_shown(&self, folder: FolderId) -> Result<()> {
        self.note_shown_in(folder, None)
    }

    /// Those of `uids` still stored in `folder`, ascending.
    pub fn stored(&self, folder: FolderId, uids: &[u32]) -> Result<Vec<u32>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT uid FROM messages
             WHERE folder_id = ?1 AND uid IN (SELECT value FROM json_each(?2)) ORDER BY uid",
        )?;
        let rows = stmt.query_map(params![folder.0, to_json(uids)], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Returns how many rows were deleted.
    pub fn delete_uids(&self, folder: FolderId, uids: &[u32]) -> Result<usize> {
        self.note_shown_in(folder, Some(uids))?;
        let mut delete = self
            .conn
            .prepare_cached("DELETE FROM messages WHERE folder_id = ?1 AND uid = ?2")?;
        let mut deleted = 0;
        for uid in uids {
            deleted += delete.execute(params![folder.0, uid])?;
        }
        Ok(deleted)
    }

    /// [`delete_uids`](Self::delete_uids), if `folder` is still at `uidvalidity`: the
    /// UIDs of another generation name other messages.
    pub fn delete_uids_of(
        &self,
        folder: FolderId,
        uidvalidity: u32,
        uids: &[u32],
    ) -> Result<usize> {
        let current: Option<u32> = self
            .conn
            .prepare_cached("SELECT uidvalidity FROM folders WHERE id = ?1")?
            .query_row([folder.0], |r| r.get(0))
            .optional()?
            .flatten();
        if current != Some(uidvalidity) {
            return Ok(0);
        }
        self.delete_uids(folder, uids)
    }

    /// Deletes the message the server no longer has at `location`, as a sync would, if
    /// the row is still that (folder, UIDVALIDITY, UID) on the server. Not one moved
    /// locally, whose location is where the move is from, nor one whose move was made
    /// since `location` was looked up: it is in another folder now, though maybe under
    /// the same UID and UIDVALIDITY. Returns whether it was deleted.
    pub fn delete_gone(&self, location: &MessageLocation) -> Result<bool> {
        let deleted = self.conn.execute(
            "DELETE FROM messages
             WHERE id = ?1 AND folder_id = ?2 AND uid = ?3
               AND folder_id IN (SELECT id FROM folders WHERE uidvalidity = ?4)",
            params![
                location.id.0,
                location.server_folder.0,
                location.uid,
                location.uidvalidity
            ],
        )?;
        Ok(deleted > 0)
    }

    /// Newest first (see [`MessageCursor`]): the first `limit`, or the `limit` after
    /// `after`. A label lists the messages with it that are in All Mail; Starred, on
    /// Gmail, the flagged ones.
    pub fn list(
        &self,
        folder: FolderId,
        limit: u32,
        after: Option<MessageCursor>,
    ) -> Result<Vec<MessageSummary>> {
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
        let (after_date, after_id) = cursor_params(after);
        let mut same_date = self.conn.prepare_cached(&sql.same_date)?;
        let mut page = same_date
            .query_map(
                named_params! {
                    ":key": key,
                    ":after_date": after_date,
                    ":after_id": after_id,
                    ":limit": limit,
                },
                summary_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let left = limit - page.len() as u32;
        if left > 0 {
            let mut older = self.conn.prepare_cached(&sql.older)?;
            let rows = older.query_map(
                named_params! {
                    ":key": key,
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
        limit: u32,
        after: Option<MessageCursor>,
    ) -> Result<Vec<MessageSummary>> {
        let mut merged = Vec::new();
        for &folder in folders {
            merged.extend(self.list(folder, limit, after)?);
        }
        // `NEWEST_FIRST`, as the cursors bind it.
        merged.sort_unstable_by_key(|m| std::cmp::Reverse(cursor_params(Some(m.cursor()))));
        // Sorted by id within a date: a message two of the folders list is listed once.
        merged.dedup_by_key(|m| m.id);
        merged.truncate(limit as usize);
        Ok(merged)
    }

    /// Messages matching the query, newest first (see [`MessageCursor`]). Bodies are only
    /// searched once downloaded; subjects and addresses always.
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<MessageSummary>> {
        let Some(expr) = fts::match_expression(&query.text) else {
            return Ok(Vec::new());
        };
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} FROM messages m
             WHERE m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH :expr)
               AND (:account IS NULL OR m.account_id = :account)
               AND ({SEARCHED})
               AND {AFTER}
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
                    "SELECT m.account_id, m.folder_id, f.id, f.remote_name,
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
                        server_folder: FolderId(r.get(2)?),
                        remote_name: r.get(3)?,
                        uidvalidity: r.get(4)?,
                        uid: r.get(5)?,
                        size: r.get(6)?,
                        downloaded: r.get(7)?,
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
             FROM messages m LEFT JOIN message_bodies b ON b.message_id = m.id
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

/// The label folders of an account by the labels X-GM-LABELS names, each looked up once:
/// an account has tens of labels, a batch of messages hundreds.
struct LabelFolders<'c> {
    folders: Folders<'c>,
    account: AccountId,
    found: HashMap<GmailLabel, Option<FolderId>>,
}

impl<'c> LabelFolders<'c> {
    fn new(conn: &'c Connection, account: AccountId) -> Self {
        Self {
            folders: Folders::new(conn),
            account,
            found: HashMap::new(),
        }
    }

    fn of_folder(conn: &'c Connection, folder: FolderId) -> Result<Self> {
        let account = conn
            .prepare_cached("SELECT account_id FROM folders WHERE id = ?1")?
            .query_row([folder.0], |r| r.get(0))?;
        Ok(Self::new(conn, AccountId(account)))
    }

    /// The folders of `labels`, each once, in their order; those without one left out.
    fn ids(&mut self, labels: &[GmailLabel]) -> Result<Vec<FolderId>> {
        let mut ids = Vec::with_capacity(labels.len());
        for label in labels {
            let id = match self.found.get(label) {
                Some(&id) => id,
                None => {
                    let id = self.folders.label(self.account, label)?;
                    self.found.insert(label.clone(), id);
                    id
                }
            };
            if let Some(id) = id
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
        Ok(ids)
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
        subject: row.get("subject")?,
        from: json(row, "from_json")?,
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
    use std::collections::HashSet;

    use catmail_types::{AccountMode, Folder};

    use super::*;
    use crate::Store;
    use crate::folders::RemoteFolder;
    use crate::test_util::{self, TestStore, gmail};

    fn setup() -> (TestStore, AccountId, Folder) {
        let store = test_util::store();
        let account = test_util::account(&store, "a@example.com");
        let folder = inbox(&store, account);
        (store, account, folder)
    }

    /// Gives the account an INBOX, its only folder.
    fn inbox(store: &Store, account: AccountId) -> Folder {
        let inbox = RemoteFolder {
            remote_name: "INBOX".into(),
            name: "INBOX".into(),
            delimiter: None,
            role: None,
            kind: FolderKind::Location,
            selectable: true,
        };
        store
            .write(|db| db.folders().reconcile(account, &[inbox]))
            .unwrap();
        store
            .read(|db| db.folders().list(account))
            .unwrap()
            .remove(0)
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
                .unwrap()
                .added
                .len(),
            3
        );

        let mut again = batch.clone();
        again[1].state.flags = Flags::SEEN | Flags::FLAGGED;
        again[1].subject = Some("rewritten".into());
        assert_eq!(
            store
                .write(|db| db.messages().upsert(account, folder.id, &again))
                .unwrap()
                .added
                .len(),
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
            store
                .read(|db| db.messages().list_merged(&folders, limit, after))
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
            .read(|db| db.messages().list_merged(&[folder.id, folder.id], 10, None))
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
                    steps(&LIST_FOLDER.same_date)?;
                    steps(&LIST_FOLDER.older)?;
                    db.messages().list(folder.id, 2, Some(after))?;
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
                labels: None,
            },
            FlagState {
                uid: 2,
                flags: Flags::empty(),
                keywords: vec![],
                modseq: None,
                labels: None,
            },
            FlagState {
                uid: 99,
                flags: Flags::SEEN,
                keywords: vec![],
                modseq: None,
                labels: None,
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
        let other_folder = inbox(&store, other).id;
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

    /// The account's folders by name, after creating them with these roles (and
    /// UIDVALIDITY 1).
    fn folders(
        store: &Store,
        account: AccountId,
        roles: &[(&str, Option<FolderRole>)],
    ) -> HashMap<String, FolderId> {
        let remote: Vec<_> = roles
            .iter()
            .map(|&(name, role)| RemoteFolder {
                remote_name: name.into(),
                name: name.into(),
                delimiter: None,
                role,
                kind: FolderKind::Location,
                selectable: true,
            })
            .collect();
        store
            .write(|db| {
                db.folders().reconcile(account, &remote)?;
                for folder in db.folders().list(account)? {
                    db.folders().reset_uidvalidity(folder.id, 1)?;
                }
                Ok(())
            })
            .unwrap();
        let list = store.read(|db| db.folders().list(account)).unwrap();
        list.into_iter().map(|f| (f.name, f.id)).collect()
    }

    /// A message dated by its UID, with this Message-ID.
    fn copy(uid: u32, message_id: Option<&str>) -> NewMessage {
        NewMessage {
            message_id: message_id.map(Into::into),
            ..message(uid, Some(uid as i64), Flags::empty())
        }
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
                (q.account, q.folder, q.limit, q.after) = (Some(account), folder, limit, after);
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
    fn deletes_a_message_gone_from_the_server_only_if_unchanged() {
        let store = test_util::store();
        let account = test_util::account(&store, "a@example.com");
        let f = folders(
            &store,
            account,
            &[("INBOX", Some(FolderRole::Inbox)), ("Kept", None)],
        );
        store
            .write(|db| {
                db.messages()
                    .upsert(account, f["INBOX"], &[copy(1, None), copy(2, None)])
            })
            .unwrap();
        let ids: Vec<_> = store
            .read(|db| db.messages().list(f["INBOX"], 2, None))
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        let location = |id| {
            store
                .read(|db| db.messages().location(id))
                .unwrap()
                .unwrap()
        };
        let delete = |location: &MessageLocation| {
            store
                .write(|db| db.messages().delete_gone(location))
                .unwrap()
        };

        // Moved locally: its location is where it is moving from.
        store
            .write(|db| db.ops().move_messages(&[ids[1]], None, f["Kept"], 0))
            .unwrap();
        let moving = location(ids[1]);
        assert!(!delete(&moving));
        // The move made since, its new UID and UIDVALIDITY the same as where it was.
        store
            .write(|db| {
                db.conn.execute(
                    "UPDATE messages SET uid = ?2 WHERE id = ?1",
                    params![ids[1].0, moving.uid],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(!delete(&moving));

        let gone = location(ids[0]);
        let stale = MessageLocation {
            uidvalidity: Some(2),
            ..gone.clone()
        };
        assert!(!delete(&stale));
        assert!(delete(&gone));
        assert!(!delete(&gone));
        let left: Vec<_> = store
            .read(|db| db.messages().list(f["Kept"], 5, None))
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(left, [ids[1]]);
        assert!(
            store
                .read(|db| db.messages().list(f["INBOX"], 5, None))
                .unwrap()
                .is_empty()
        );
    }

    fn user(name: &str) -> GmailLabel {
        GmailLabel::user(name, name)
    }

    /// A Gmail message dated by its UID, its X-GM-MSGID 100 more.
    fn labelled(uid: u32, flags: Flags, labels: Vec<GmailLabel>) -> NewMessage {
        let mut m = message(uid, Some(uid as i64), flags);
        m.state.labels = Some(labels);
        (m.gm_msgid, m.gm_thrid) = (Some(100 + uid as u64), Some(100 + uid as u64));
        m
    }

    /// Upserts `messages` into `folder`, returning the rows inserted.
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

    fn subjects(store: &Store, folder: FolderId) -> Vec<String> {
        store
            .read(|db| db.messages().list(folder, 50, None))
            .unwrap()
            .into_iter()
            .map(|m| m.subject.unwrap())
            .collect()
    }

    /// (total, unread) of every folder of the account, checked against counting anew.
    fn counts(store: &Store, account: AccountId) -> HashMap<String, (u32, u32)> {
        store
            .read(|db| {
                let mut counts = HashMap::new();
                for f in db.folders().list(account)? {
                    let shown = db.messages().list(f.id, u32::MAX, None)?;
                    let unread = shown
                        .iter()
                        .filter(|m| !m.flags.contains(Flags::SEEN))
                        .count() as u32;
                    assert_eq!(
                        (f.total_count, f.unread_count),
                        (shown.len() as u32, unread),
                        "{}",
                        f.name
                    );
                    counts.insert(f.name, (f.total_count, f.unread_count));
                }
                Ok(counts)
            })
            .unwrap()
    }

    #[test]
    fn labels_show_and_count_what_is_in_all_mail() {
        let store = test_util::store();
        let (account, f) = gmail(&store);
        let inbox = GmailLabel::System(FolderRole::Inbox);
        let important = GmailLabel::System(FolderRole::Important);
        store
            .write(|db| {
                db.messages().upsert(
                    account,
                    f["[Gmail]/All Mail"],
                    &[
                        labelled(
                            1,
                            Flags::empty(),
                            vec![inbox.clone(), important, user("Work")],
                        ),
                        labelled(2, Flags::SEEN | Flags::FLAGGED, vec![user("Work")]),
                        labelled(3, Flags::FLAGGED, vec![user("Hidden")]),
                    ],
                )?;
                // Trashed messages keep their labels, but labels don't show them.
                db.messages().upsert(
                    account,
                    f["[Gmail]/Trash"],
                    &[labelled(4, Flags::FLAGGED, vec![user("Work")])],
                )
            })
            .unwrap();
        let s = |uids: &[u32]| {
            uids.iter()
                .map(|uid| format!("subject {uid}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(subjects(&store, f["INBOX"]), s(&[1]));
        assert_eq!(subjects(&store, f["Work"]), s(&[2, 1]));
        assert_eq!(subjects(&store, f["[Gmail]/Important"]), s(&[1]));
        assert_eq!(subjects(&store, f["[Gmail]/Starred"]), s(&[3, 2]));
        assert_eq!(subjects(&store, f["[Gmail]/All Mail"]), s(&[3, 2, 1]));
        // Each row says which labels it has, wherever it is listed.
        let listed = store
            .read(|db| db.messages().list(f["Work"], 10, None))
            .unwrap();
        let labels: BTreeSet<_> = listed[1].labels.iter().copied().collect();
        assert_eq!(
            labels,
            BTreeSet::from([f["INBOX"], f["Work"], f["[Gmail]/Important"]])
        );
        let one = store.read(|db| db.messages().get(listed[1].id)).unwrap();
        assert_eq!(one.unwrap().summary.labels.len(), 3);
        assert_eq!(listed[0].labels, [f["Work"]]);
        // A label X-GM-LABELS names that LIST doesn't is kept, not shown.
        let hidden = store
            .read(|db| db.folders().find_by_name(account, "Hidden"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (hidden.kind, hidden.listed, hidden.delimiter),
            (FolderKind::Label, false, Some('/'))
        );
        assert_eq!(subjects(&store, hidden.id), s(&[3]));
        let counted = counts(&store, account);
        assert_eq!(counted["Work"], (2, 1));
        assert_eq!(counted["[Gmail]/Starred"], (2, 1));
        assert_eq!(counted["[Gmail]/Trash"], (1, 1));

        // Labels change with flags; only a change counts.
        let state = |uid, flags, labels: Vec<GmailLabel>| FlagState {
            uid,
            flags,
            labels: Some(labels),
            ..Default::default()
        };
        let all_mail = f["[Gmail]/All Mail"];
        let update = |states: &[FlagState]| {
            store
                .write(|db| db.messages().update_flags(all_mail, states))
                .unwrap()
        };
        let refreshed = [
            state(1, Flags::SEEN, vec![inbox.clone(), user("Hidden")]),
            state(2, Flags::SEEN | Flags::FLAGGED, vec![user("Work")]),
            state(3, Flags::FLAGGED, vec![user("Hidden"), user("Work")]),
        ];
        assert_eq!(update(&refreshed), 2);
        assert_eq!(update(&refreshed), 0);
        assert_eq!(subjects(&store, f["Work"]), s(&[3, 2]));
        assert_eq!(subjects(&store, hidden.id), s(&[3, 1]));
        assert!(subjects(&store, f["[Gmail]/Important"]).is_empty());
        // A label added: the labels that list it already show its labels, so they change.
        let touched = store
            .write(|db| {
                let messages = db.messages();
                let relabelled = [state(
                    1,
                    Flags::SEEN,
                    vec![inbox.clone(), user("Hidden"), user("Work")],
                )];
                messages.update_flags(all_mail, &relabelled)?;
                Ok(messages.touched_labels())
            })
            .unwrap();
        let mut expected = vec![f["INBOX"], f["Work"], hidden.id];
        expected.sort();
        assert_eq!(touched, expected);
        update(&refreshed[..1]);
        // Labels left alone where none are given.
        let flags_only = FlagState {
            labels: None,
            ..state(3, Flags::SEEN | Flags::FLAGGED, vec![])
        };
        assert_eq!(update(&[flags_only]), 1);
        assert_eq!(subjects(&store, f["Work"]), s(&[3, 2]));
        let counted = counts(&store, account);
        assert_eq!(counted["Work"], (2, 0));
        assert_eq!(counted["[Gmail]/Starred"], (2, 0));

        // Moved to Trash and back, as sync will move a row by its X-GM-MSGID.
        let to = |folder: FolderId| {
            store
                .write(|db| {
                    db.conn.execute(
                        "UPDATE messages SET folder_id = ?1, flags = ?2 WHERE gm_msgid = 103",
                        params![folder.0, Flags::FLAGGED.bits()],
                    )?;
                    Ok(())
                })
                .unwrap();
        };
        to(f["[Gmail]/Trash"]);
        assert_eq!(subjects(&store, f["Work"]), s(&[2]));
        assert_eq!(counts(&store, account)["[Gmail]/Starred"], (1, 0));
        to(all_mail);
        assert_eq!(subjects(&store, f["Work"]), s(&[3, 2]));
        assert_eq!(counts(&store, account)["[Gmail]/Starred"], (2, 1));

        // Gone from the server, and with it from its labels.
        store
            .write(|db| db.messages().delete_uids(all_mail, &[3]))
            .unwrap();
        let counted = counts(&store, account);
        assert_eq!(counted["Work"], (1, 0));
        assert_eq!(counted["Hidden"], (1, 0));
        assert_eq!(counted["[Gmail]/Starred"], (1, 0));

        // A message is one X-GM-MSGID: found in another location (another client moved
        // it), its row moves there, and the labels that showed it no longer do.
        let trash = f["[Gmail]/Trash"];
        let id = |folder| {
            store
                .read(|db| db.messages().list(folder, 50, None))
                .unwrap()
                .into_iter()
                .find(|m| m.subject.as_deref() == Some("subject 2"))
                .map(|m| m.id)
        };
        let before = id(all_mail);
        let mut trashed = labelled(2, Flags::SEEN, vec![user("Work")]);
        trashed.state.uid = 7;
        let (upserted, touched) = store
            .write(|db| {
                let messages = db.messages();
                let upserted = messages.upsert(account, trash, &[trashed])?;
                Ok((upserted, messages.touched_labels()))
            })
            .unwrap();
        assert_eq!(
            upserted,
            Upserted {
                moved: 1,
                moved_from: vec![all_mail],
                ..Default::default()
            }
        );
        let mut showed = vec![f["Work"], f["[Gmail]/Starred"]];
        showed.sort();
        assert_eq!(touched, showed);
        assert_eq!((id(all_mail), id(trash)), (None, before));
        assert_eq!(subjects(&store, f["Work"]), Vec::<String>::new());
        assert_eq!(counts(&store, account)["[Gmail]/Starred"], (0, 0));

        // A folder deleted however it is takes its messages out of the labels first.
        store
            .write(|db| {
                db.conn
                    .execute("DELETE FROM folders WHERE id = ?1", [all_mail.0])?;
                Ok(())
            })
            .unwrap();
        let counted = counts(&store, account);
        assert_eq!(counted["Work"], (0, 0));
        assert_eq!(counted["[Gmail]/Starred"], (0, 0));
    }

    /// A message moved locally to Trash, not yet on the server, is where the move is from
    /// until replayed: sync finds it there, or (moved by another client) in Trash, and
    /// leaves its row alone either way.
    #[test]
    fn a_row_moving_locally_stays_until_the_move_is_replayed() {
        let store = test_util::store();
        let (account, f) = gmail(&store);
        let (all_mail, trash) = (f["[Gmail]/All Mail"], f["[Gmail]/Trash"]);
        let message = labelled(1, Flags::empty(), vec![user("Work")]);
        store
            .write(|db| {
                db.messages()
                    .upsert(account, all_mail, std::slice::from_ref(&message))
            })
            .unwrap();
        let id = store
            .read(|db| db.messages().list(all_mail, 1, None))
            .unwrap()[0]
            .id;
        store
            .write(|db| db.ops().move_messages(&[id], None, trash, 0))
            .unwrap();

        let upsert = |folder, uid| {
            let mut m = message.clone();
            m.state.uid = uid;
            store
                .write(|db| db.messages().upsert(account, folder, &[m]))
                .unwrap()
        };
        assert_eq!(upsert(all_mail, 1), Upserted::default());
        let held = Upserted {
            held: 1,
            ..Default::default()
        };
        assert_eq!(upsert(trash, 9), held);
        let row = store
            .read(|db| db.messages().location(id))
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.folder, row.server_folder, row.uid),
            (trash, all_mail, 1)
        );
    }

    /// Starred counts what is flagged in All Mail, also when it is listed only once
    /// messages are stored: Gmail lets it be hidden from IMAP.
    #[test]
    fn starred_listed_later_counts_what_is_flagged() {
        let store = test_util::store();
        let (account, f) = gmail(&store);
        let all: Vec<RemoteFolder> = store
            .read(|db| db.folders().list(account))
            .unwrap()
            .into_iter()
            .map(|f| RemoteFolder {
                remote_name: f.name.clone(),
                name: f.name,
                delimiter: f.delimiter,
                role: f.role,
                kind: f.kind,
                selectable: f.selectable,
            })
            .collect();
        let hidden: Vec<_> = all
            .iter()
            .filter(|f| f.role != Some(FolderRole::Flagged))
            .cloned()
            .collect();
        let reconcile = |remote: &[RemoteFolder]| {
            store
                .write(|db| db.folders().reconcile(account, remote))
                .unwrap()
        };
        reconcile(&hidden);
        let all_mail = f["[Gmail]/All Mail"];
        store
            .write(|db| {
                db.messages().upsert(
                    account,
                    all_mail,
                    &[
                        labelled(1, Flags::FLAGGED, vec![]),
                        labelled(2, Flags::FLAGGED | Flags::SEEN, vec![]),
                    ],
                )
            })
            .unwrap();

        reconcile(&all);
        assert_eq!(counts(&store, account)["[Gmail]/Starred"], (2, 1));
        let unflagged = FlagState {
            uid: 1,
            ..Default::default()
        };
        store
            .write(|db| db.messages().update_flags(all_mail, &[unflagged]))
            .unwrap();
        assert_eq!(counts(&store, account)["[Gmail]/Starred"], (1, 0));

        // Likewise a folder that takes on the role.
        let renamed: Vec<_> = all
            .iter()
            .map(|f| RemoteFolder {
                role: match f.role {
                    Some(FolderRole::Flagged) => None,
                    _ if f.remote_name == "Work" => Some(FolderRole::Flagged),
                    role => role,
                },
                ..f.clone()
            })
            .collect();
        reconcile(&renamed);
        let counted = counts(&store, account);
        assert_eq!(
            (counted["Work"], counted["[Gmail]/Starred"]),
            ((1, 0), (0, 0))
        );
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
