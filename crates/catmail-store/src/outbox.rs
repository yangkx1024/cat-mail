use catmail_types::{AccountId, BlobHash, DraftId, OutboxEntry, OutboxId, OutboxStatus};
use rusqlite::{Connection, OptionalExtension, Row, named_params, params};

use crate::columns::{json, parse_text, to_json};
use crate::{Error, Result};

const COLUMNS: &str = "id, account_id, draft_id, subject, recipients_json, message_id_hdr, status,
    attempts, next_attempt_at, last_error, created_at";

/// A message to queue: its raw form is already in the blob store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOutboxEntry {
    pub account: AccountId,
    /// The draft it was built from. Queueing fails if the draft changed since `revision`,
    /// or is queued already.
    pub draft: DraftId,
    pub revision: i64,
    pub raw: BlobHash,
    /// Without angle brackets.
    pub message_id: String,
    pub subject: String,
    pub envelope_from: String,
    pub recipients: Vec<String>,
}

/// What an attempt at sending or saving needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxJob {
    pub entry: OutboxEntry,
    pub raw: BlobHash,
    pub envelope_from: String,
}

pub struct Outbox<'c> {
    conn: &'c Connection,
}

impl<'c> Outbox<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    pub fn queue(&self, entry: &NewOutboxEntry, now: i64) -> Result<OutboxId> {
        let (draft, revision) = (entry.draft, entry.revision);
        let current: Option<(i64, i64, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT d.account_id, d.revision, o.id
                 FROM drafts d LEFT JOIN outbox o ON o.draft_id = d.id WHERE d.id = ?1",
                [draft.0],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match current {
            None => return Err(Error::Conflict(format!("draft {draft} was deleted"))),
            Some((account, ..)) if account != entry.account.0 => {
                return Err(Error::Conflict(format!(
                    "draft {draft} belongs to another account"
                )));
            }
            Some((_, _, Some(_))) => {
                return Err(Error::Conflict(format!("draft {draft} is already queued")));
            }
            Some((_, current, None)) if current != revision => {
                return Err(Error::Conflict(format!(
                    "draft {draft} changed while it was being sent; send it again"
                )));
            }
            Some(_) => {}
        }
        self.conn
            .prepare_cached(
                "INSERT INTO outbox (account_id, draft_id, raw_blob_hash, message_id_hdr, subject,
                                     envelope_from, recipients_json, next_attempt_at,
                                     created_at, updated_at)
                 VALUES (:account, :draft, :raw, :message_id, :subject, :from, :recipients,
                         :now, :now, :now)",
            )?
            .execute(named_params! {
                ":account": entry.account.0,
                ":draft": entry.draft.0,
                ":raw": entry.raw.as_str(),
                ":message_id": entry.message_id,
                ":subject": entry.subject,
                ":from": entry.envelope_from,
                ":recipients": to_json(&entry.recipients),
                ":now": now,
            })?;
        Ok(OutboxId(self.conn.last_insert_rowid()))
    }

    pub fn get(&self, id: OutboxId) -> Result<Option<OutboxEntry>> {
        let sql = format!("SELECT {COLUMNS} FROM outbox WHERE id = ?1");
        Ok(self
            .conn
            .prepare_cached(&sql)?
            .query_row([id.0], from_row)
            .optional()?)
    }

    /// Oldest first; every account's with `None`.
    pub fn list(&self, account: Option<AccountId>) -> Result<Vec<OutboxEntry>> {
        let sql =
            format!("SELECT {COLUMNS} FROM outbox WHERE ?1 IS NULL OR account_id = ?1 ORDER BY id");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([account.map(|a| a.0)], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Entries of `account` with work left in `status` (queued, or accepted and not
    /// yet saved), oldest first. With `now`, only those due by then.
    pub fn pending(
        &self,
        account: AccountId,
        status: OutboxStatus,
        now: Option<i64>,
    ) -> Result<Vec<OutboxId>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM outbox
             WHERE account_id = ?1 AND status = ?2 AND (?3 IS NULL OR next_attempt_at <= ?3)
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![account.0, status.as_str(), now], |r| {
            Ok(OutboxId(r.get(0)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Accounts with entries due by `now`.
    pub fn accounts_due(&self, now: i64) -> Result<Vec<AccountId>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT account_id FROM outbox
             WHERE status IN ('queued', 'accepted') AND next_attempt_at <= ?1
             ORDER BY account_id",
        )?;
        let rows = stmt.query_map([now], |r| Ok(AccountId(r.get(0)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// When the next automatic attempt of any entry is due.
    pub fn next_attempt_at(&self) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT min(next_attempt_at) FROM outbox WHERE status IN ('queued', 'accepted')",
            [],
            |r| r.get(0),
        )?)
    }

    /// Starts sending a queued entry: it becomes `sending`, and the attempt is counted
    /// before it is made. `None` if the entry isn't queued (any more).
    pub fn begin_send(&self, id: OutboxId, now: i64) -> Result<Option<OutboxJob>> {
        self.begin(id, OutboxStatus::Queued, OutboxStatus::Sending, now)
    }

    /// Starts saving an accepted entry to Sent, counting the attempt before it is made.
    /// `None` if the entry isn't accepted (any more).
    pub fn begin_save(&self, id: OutboxId, now: i64) -> Result<Option<OutboxJob>> {
        self.begin(id, OutboxStatus::Accepted, OutboxStatus::Accepted, now)
    }

    fn begin(
        &self,
        id: OutboxId,
        from: OutboxStatus,
        to: OutboxStatus,
        now: i64,
    ) -> Result<Option<OutboxJob>> {
        let updated = self.conn.execute(
            "UPDATE outbox SET status = ?3, attempts = attempts + 1, updated_at = ?4
             WHERE id = ?1 AND status = ?2",
            params![id.0, from.as_str(), to.as_str(), now],
        )?;
        if updated == 0 {
            return Ok(None);
        }
        let sql =
            format!("SELECT {COLUMNS}, raw_blob_hash, envelope_from FROM outbox WHERE id = ?1");
        Ok(Some(self.conn.prepare_cached(&sql)?.query_row(
            [id.0],
            |r| {
                Ok(OutboxJob {
                    entry: from_row(r)?,
                    raw: parse_text(r, "raw_blob_hash")?,
                    envelope_from: r.get("envelope_from")?,
                })
            },
        )?))
    }

    /// Written right before the end of the message is sent, when the server may accept
    /// it at any moment: whatever happens next, the message must not be resent on its
    /// own. Written again with the error if no verdict arrives.
    pub fn mark_unknown(&self, id: OutboxId, error: Option<&str>, now: i64) -> Result<bool> {
        self.transition(
            id,
            &[OutboxStatus::Sending, OutboxStatus::Unknown],
            OutboxStatus::Unknown,
            now,
            None,
            error,
        )
    }

    /// The server accepted the message; saving to Sent comes next, at once.
    pub fn mark_accepted(&self, id: OutboxId, now: i64) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE outbox SET status = 'accepted', attempts = 0, next_attempt_at = ?2,
                               last_error = NULL, updated_at = ?2
             WHERE id = ?1 AND status IN ('sending', 'unknown')",
            params![id.0, now],
        )?;
        Ok(updated > 0)
    }

    /// A temporary failure before the server accepted anything: try again at `next`.
    pub fn requeue(&self, id: OutboxId, next: i64, error: &str, now: i64) -> Result<bool> {
        self.transition(
            id,
            &[OutboxStatus::Sending, OutboxStatus::Unknown],
            OutboxStatus::Queued,
            now,
            Some(next),
            Some(error),
        )
    }

    /// Puts off a queued entry that wasn't attempted, because the server couldn't be
    /// reached for an earlier one. Its attempts don't count up.
    pub fn postpone(&self, id: OutboxId, next: i64, error: &str, now: i64) -> Result<bool> {
        self.transition(
            id,
            &[OutboxStatus::Queued],
            OutboxStatus::Queued,
            now,
            Some(next),
            Some(error),
        )
    }

    /// The server refused the message for good.
    pub fn fail(&self, id: OutboxId, error: &str, now: i64) -> Result<bool> {
        self.transition(
            id,
            &[OutboxStatus::Sending, OutboxStatus::Unknown],
            OutboxStatus::Failed,
            now,
            None,
            Some(error),
        )
    }

    /// Saving to Sent failed; try again at `next`.
    pub fn save_later(&self, id: OutboxId, next: i64, error: &str, now: i64) -> Result<bool> {
        self.transition(
            id,
            &[OutboxStatus::Accepted],
            OutboxStatus::Accepted,
            now,
            Some(next),
            Some(error),
        )
    }

    /// Sent and saved: deletes the entry and the draft it came from. Returns false if
    /// the entry is gone (cancelled meanwhile).
    pub fn finish(&self, id: OutboxId) -> Result<bool> {
        let draft: Option<Option<i64>> = self
            .conn
            .query_row(
                "DELETE FROM outbox WHERE id = ?1 AND status = 'accepted' RETURNING draft_id",
                [id.0],
                |r| r.get(0),
            )
            .optional()?;
        let Some(draft) = draft else {
            return Ok(false);
        };
        if let Some(draft) = draft {
            self.conn
                .execute("DELETE FROM drafts WHERE id = ?1", [draft])?;
        }
        Ok(true)
    }

    /// Entries a process left `sending` never got to the end of their message (that
    /// is marked `unknown` first), so the server discarded them: back to the queue.
    pub fn recover_interrupted(&self, now: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE outbox SET status = 'queued', next_attempt_at = ?1, updated_at = ?1
             WHERE status = 'sending'",
            [now],
        )?)
    }

    /// The user asks for another attempt now: of the send if it failed or its outcome
    /// is unknown (which may deliver twice), of saving to Sent if it was accepted.
    pub fn retry(&self, id: OutboxId, now: i64) -> Result<Option<OutboxEntry>> {
        let Some(entry) = self.get(id)? else {
            return Ok(None);
        };
        let to = match entry.status {
            OutboxStatus::Failed | OutboxStatus::Unknown => OutboxStatus::Queued,
            s @ (OutboxStatus::Accepted | OutboxStatus::Queued) => s,
            other => return Err(conflict(id, other, "retried")),
        };
        self.conn.execute(
            // Saving keeps its count: past the first attempt, it checks Sent first.
            "UPDATE outbox SET status = ?2, next_attempt_at = ?3, updated_at = ?3,
                               attempts = CASE WHEN ?2 = 'accepted' THEN attempts ELSE 0 END
             WHERE id = ?1",
            params![id.0, to.as_str(), now],
        )?;
        self.get(id)
    }

    /// The user confirmed a message with an unknown outcome arrived: save it to Sent.
    pub fn mark_sent(&self, id: OutboxId, now: i64) -> Result<Option<OutboxEntry>> {
        let Some(entry) = self.get(id)? else {
            return Ok(None);
        };
        if entry.status != OutboxStatus::Unknown {
            return Err(conflict(id, entry.status, "marked sent"));
        }
        self.mark_accepted(id, now)?;
        self.get(id)
    }

    /// Removes the entry. One that never went out hands its draft back for editing;
    /// for an accepted one this gives up saving it to Sent, and its draft is deleted.
    /// A send in progress can't be cancelled (the engine waits for it to end first).
    pub fn cancel(&self, id: OutboxId) -> Result<Option<OutboxEntry>> {
        let Some(entry) = self.get(id)? else {
            return Ok(None);
        };
        match entry.status {
            OutboxStatus::Queued | OutboxStatus::Failed | OutboxStatus::Unknown => {
                self.conn
                    .execute("DELETE FROM outbox WHERE id = ?1", [id.0])?;
            }
            OutboxStatus::Accepted => {
                self.finish(id)?;
            }
            other => return Err(conflict(id, other, "cancelled")),
        }
        Ok(Some(entry))
    }

    fn transition(
        &self,
        id: OutboxId,
        from: &[OutboxStatus],
        to: OutboxStatus,
        now: i64,
        next: Option<i64>,
        error: Option<&str>,
    ) -> Result<bool> {
        let from = to_json(&from.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let updated = self.conn.execute(
            "UPDATE outbox SET status = :to, next_attempt_at = :next, last_error = :error,
                               updated_at = :now
             WHERE id = :id AND status IN (SELECT value FROM json_each(:from))",
            named_params! {
                ":id": id.0,
                ":from": from,
                ":to": to.as_str(),
                ":next": next,
                ":error": error,
                ":now": now,
            },
        )?;
        Ok(updated > 0)
    }
}

fn conflict(id: OutboxId, status: OutboxStatus, action: &str) -> Error {
    let state = match status {
        OutboxStatus::Sending => "is being sent",
        OutboxStatus::Done => "is done",
        OutboxStatus::Queued => "is queued",
        OutboxStatus::Accepted => "was sent",
        OutboxStatus::Unknown => "has an unknown outcome",
        OutboxStatus::Failed => "failed",
    };
    Error::Conflict(format!("outbox entry {id} {state} and can't be {action}"))
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<OutboxEntry> {
    Ok(OutboxEntry {
        id: OutboxId(row.get("id")?),
        account_id: AccountId(row.get("account_id")?),
        draft_id: row.get::<_, Option<i64>>("draft_id")?.map(DraftId),
        subject: row.get("subject")?,
        recipients: json(row, "recipients_json")?,
        message_id: row.get("message_id_hdr")?,
        status: parse_text(row, "status")?,
        attempts: row.get("attempts")?,
        next_attempt_at: row.get("next_attempt_at")?,
        last_error: row.get("last_error")?,
        created_at: row.get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use catmail_types::{Address, DraftContent};

    use super::*;
    use crate::{Store, test_util};

    fn hash(c: char) -> BlobHash {
        BlobHash::parse(&c.to_string().repeat(64)).unwrap()
    }

    /// A draft queued for sending.
    fn setup() -> (Store, AccountId, DraftId, OutboxId) {
        let store = Store::open_in_memory().unwrap();
        let account = test_util::account(&store, "a@example.com");
        let content = DraftContent {
            subject: "hi".into(),
            to: vec![Address {
                name: None,
                email: "b@example.com".into(),
            }],
            ..Default::default()
        };
        let (draft, outbox) = store
            .write(|db| {
                let draft = db.drafts().insert(account, &content, None, 1)?;
                let outbox = db.outbox().queue(
                    &NewOutboxEntry {
                        account,
                        draft,
                        revision: 0,
                        raw: hash('a'),
                        message_id: "m@example.com".into(),
                        subject: "hi".into(),
                        envelope_from: "a@example.com".into(),
                        recipients: vec!["b@example.com".into()],
                    },
                    10,
                )?;
                Ok((draft, outbox))
            })
            .unwrap();
        (store, account, draft, outbox)
    }

    fn status(store: &Store, id: OutboxId) -> Option<(OutboxStatus, u32)> {
        store
            .read(|db| db.outbox().get(id))
            .unwrap()
            .map(|e| (e.status, e.attempts))
    }

    #[test]
    fn send_then_save() {
        let (store, account, draft, id) = setup();
        let entry = store.read(|db| db.outbox().get(id)).unwrap().unwrap();
        assert_eq!(entry.next_attempt_at, Some(10), "due at once");
        let pending = |status, now| {
            store
                .read(|db| db.outbox().pending(account, status, now))
                .unwrap()
        };
        assert_eq!(pending(OutboxStatus::Queued, Some(9)), []);
        assert_eq!(pending(OutboxStatus::Queued, Some(10)), [id]);
        assert_eq!(pending(OutboxStatus::Queued, None), [id]);
        assert_eq!(pending(OutboxStatus::Accepted, None), []);
        assert_eq!(
            store.read(|db| db.outbox().accounts_due(10)).unwrap(),
            [account]
        );
        assert_eq!(
            store.read(|db| db.blob_references()).unwrap(),
            [hash('a')].into()
        );

        let job = store
            .write(|db| db.outbox().begin_send(id, 11))
            .unwrap()
            .unwrap();
        assert_eq!(job.raw, hash('a'));
        assert_eq!(job.envelope_from, "a@example.com");
        assert_eq!(status(&store, id), Some((OutboxStatus::Sending, 1)));
        assert!(
            store
                .write(|db| db.outbox().begin_send(id, 11))
                .unwrap()
                .is_none(),
            "one attempt at a time"
        );
        assert!(matches!(
            store.write(|db| db.outbox().cancel(id)),
            Err(Error::Conflict(_))
        ));
        assert_eq!(pending(OutboxStatus::Queued, None), []);

        assert!(
            store
                .write(|db| db.outbox().mark_unknown(id, None, 12))
                .unwrap()
        );
        assert!(store.write(|db| db.outbox().mark_accepted(id, 13)).unwrap());
        assert!(
            store
                .write(|db| db.outbox().begin_send(id, 13))
                .unwrap()
                .is_none(),
            "never sent again"
        );
        assert_eq!(status(&store, id), Some((OutboxStatus::Accepted, 0)));

        // Saving to Sent fails once, then works.
        store
            .write(|db| db.outbox().begin_save(id, 13))
            .unwrap()
            .unwrap();
        assert!(
            store
                .write(|db| db.outbox().save_later(id, 60, "NO quota", 13))
                .unwrap()
        );
        let entry = store.read(|db| db.outbox().get(id)).unwrap().unwrap();
        assert_eq!(
            (
                entry.status,
                entry.attempts,
                entry.next_attempt_at,
                entry.last_error.as_deref()
            ),
            (OutboxStatus::Accepted, 1, Some(60), Some("NO quota"))
        );
        assert_eq!(
            store.read(|db| db.outbox().next_attempt_at()).unwrap(),
            Some(60)
        );
        store
            .write(|db| db.outbox().begin_save(id, 60))
            .unwrap()
            .unwrap();
        assert_eq!(status(&store, id), Some((OutboxStatus::Accepted, 2)));

        assert!(store.write(|db| db.outbox().finish(id)).unwrap());
        assert_eq!(status(&store, id), None);
        assert!(
            store.read(|db| db.drafts().get(draft)).unwrap().is_none(),
            "sent"
        );
        assert!(store.read(|db| db.blob_references()).unwrap().is_empty());
        assert!(!store.write(|db| db.outbox().finish(id)).unwrap());
    }

    #[test]
    fn failures_and_user_decisions() {
        let (store, account, draft, id) = setup();
        store.write(|db| db.outbox().begin_send(id, 11)).unwrap();
        assert!(
            store
                .write(|db| db.outbox().requeue(id, 40, "421 busy", 11))
                .unwrap()
        );
        assert_eq!(status(&store, id), Some((OutboxStatus::Queued, 1)));
        assert!(
            store
                .read(|db| db.outbox().accounts_due(39))
                .unwrap()
                .is_empty()
        );

        store.write(|db| db.outbox().begin_send(id, 40)).unwrap();
        assert!(
            store
                .write(|db| db.outbox().fail(id, "550 no such user", 40))
                .unwrap()
        );
        assert_eq!(status(&store, id), Some((OutboxStatus::Failed, 2)));
        assert!(
            store
                .read(|db| db.outbox().next_attempt_at())
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            store.write(|db| db.outbox().mark_sent(id, 41)),
            Err(Error::Conflict(_))
        ));

        let retried = store
            .write(|db| db.outbox().retry(id, 41))
            .unwrap()
            .unwrap();
        assert_eq!(
            (retried.status, retried.attempts, retried.next_attempt_at),
            (OutboxStatus::Queued, 0, Some(41))
        );

        // Lost the connection after the final dot.
        store.write(|db| db.outbox().begin_send(id, 42)).unwrap();
        store
            .write(|db| db.outbox().mark_unknown(id, None, 42))
            .unwrap();
        for status in [OutboxStatus::Queued, OutboxStatus::Accepted] {
            assert!(
                store
                    .read(|db| db.outbox().pending(account, status, None))
                    .unwrap()
                    .is_empty(),
                "never retried on its own"
            );
        }
        store
            .write(|db| db.outbox().mark_unknown(id, Some("connection closed"), 42))
            .unwrap();
        let sent = store
            .write(|db| db.outbox().mark_sent(id, 43))
            .unwrap()
            .unwrap();
        assert_eq!(
            (sent.status, sent.next_attempt_at),
            (OutboxStatus::Accepted, Some(43))
        );

        // Saving keeps its attempt count across a retry, so it still checks Sent first.
        store.write(|db| db.outbox().begin_save(id, 43)).unwrap();
        store
            .write(|db| db.outbox().save_later(id, 99, "BYE", 43))
            .unwrap();
        let retried = store
            .write(|db| db.outbox().retry(id, 44))
            .unwrap()
            .unwrap();
        assert_eq!(
            (retried.status, retried.attempts),
            (OutboxStatus::Accepted, 1)
        );

        // Giving up on saving: the message went out, so the draft goes too.
        store.write(|db| db.outbox().cancel(id)).unwrap().unwrap();
        assert_eq!(status(&store, id), None);
        assert!(store.read(|db| db.drafts().get(draft)).unwrap().is_none());
        assert!(store.write(|db| db.outbox().cancel(id)).unwrap().is_none());
    }

    #[test]
    fn interrupted_sends_go_back_to_the_queue() {
        let (store, _, _, id) = setup();
        store.write(|db| db.outbox().begin_send(id, 11)).unwrap();
        assert_eq!(
            store
                .write(|db| db.outbox().recover_interrupted(20))
                .unwrap(),
            1
        );
        assert_eq!(status(&store, id), Some((OutboxStatus::Queued, 1)));

        // Past the point where the server may have accepted it, it stays unknown.
        store.write(|db| db.outbox().begin_send(id, 21)).unwrap();
        store
            .write(|db| db.outbox().mark_unknown(id, None, 21))
            .unwrap();
        assert_eq!(
            store
                .write(|db| db.outbox().recover_interrupted(30))
                .unwrap(),
            0
        );
        assert_eq!(status(&store, id), Some((OutboxStatus::Unknown, 2)));
    }

    #[test]
    fn removing_the_account_removes_its_outbox() {
        let (store, account, _, id) = setup();
        store.write(|db| db.accounts().delete(account)).unwrap();
        assert_eq!(status(&store, id), None);
        assert!(store.read(|db| db.outbox().list(None)).unwrap().is_empty());
    }
}
