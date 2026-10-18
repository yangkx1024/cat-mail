use catmail_types::{AccountId, Address, FlagState, Flags, FolderId, MessageId, MessageSummary};
use rusqlite::{Connection, Row, named_params, params};

use crate::Result;
use crate::columns::{json, to_json};

const SUMMARY_COLUMNS: &str = "id, account_id, folder_id, thread_id, subject, from_json, date, snippet, flags, has_attachments";

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
    /// The Date header; INTERNALDATE is used when it is missing.
    pub date: Option<i64>,
    pub has_attachments: bool,
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
    /// after an interrupted sync). Immutable header fields are never rewritten.
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
                                   references_hdr, subject, from_json, to_json, cc_json, date,
                                   has_attachments)
             VALUES (:account, :folder, :uid, :flags, :keywords, :modseq,
                     :internal_date, :size, :message_id, :in_reply_to,
                     :references, :subject, :from, :to, :cc, :date, :has_attachments)
             ON CONFLICT (folder_id, uid) DO NOTHING",
        )?;
        let mut inserted = 0;
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
                ":references": (!m.references.is_empty()).then(|| m.references.join(" ")),
                ":subject": m.subject,
                ":from": to_json(&m.from),
                ":to": to_json(&m.to),
                ":cc": to_json(&m.cc),
                ":date": m.date.or(m.internal_date),
                ":has_attachments": m.has_attachments,
            })?;
            if added > 0 {
                inserted += 1;
            } else {
                self.update_flags(folder, std::slice::from_ref(&m.state))?;
            }
        }
        Ok(inserted)
    }

    /// Applies the server's flags to stored messages; UIDs not stored are ignored.
    /// Returns how many rows actually changed.
    pub fn update_flags(&self, folder: FolderId, states: &[FlagState]) -> Result<usize> {
        let mut update = self.conn.prepare_cached(
            "UPDATE messages SET flags = :flags, keywords_json = :keywords, modseq = :modseq
             WHERE folder_id = :folder AND uid = :uid
               AND (flags, keywords_json, modseq) IS NOT (:flags, :keywords, :modseq)",
        )?;
        let mut changed = 0;
        for s in states {
            changed += update.execute(named_params! {
                ":folder": folder.0,
                ":uid": s.uid,
                ":flags": s.flags.bits(),
                ":keywords": to_json(&s.keywords),
                ":modseq": s.modseq.map(|v| v as i64),
            })?;
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

    /// Newest first; ties (equal or missing dates) by UID, i.e. arrival order.
    pub fn list(&self, folder: FolderId, limit: u32, offset: u32) -> Result<Vec<MessageSummary>> {
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} FROM messages WHERE folder_id = ?1
             ORDER BY date DESC, uid DESC LIMIT ?2 OFFSET ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![folder.0, limit, offset], summary_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
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
    use catmail_types::Folder;

    use super::*;
    use crate::folders::RemoteFolder;
    use crate::{Store, test_util};

    fn setup() -> (Store, AccountId, Folder) {
        let store = Store::open_in_memory().unwrap();
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
            .read(|db| db.messages().list(folder.id, 10, 0))
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
            store
                .read(|db| db.messages().list(folder.id, 1, 1))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store.read(|db| db.messages().uids(folder.id)).unwrap(),
            [1, 2, 3]
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
}
