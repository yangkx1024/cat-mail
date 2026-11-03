//! What the background prefetch of bodies keeps (`catmail_core::prefetch`): which
//! messages to fetch next, the ones that failed, and how much of the daily budget is used.

use std::time::Duration;

use catmail_types::{AccountId, FolderId, MessageId};
use rusqlite::{Connection, OptionalExtension, named_params, params};

use crate::Result;
use crate::columns::to_json;

/// How long a message whose body failed to fetch waits before it is tried again, after
/// its first failure; four times as long after each more, up to [`MAX_RETRY_AFTER`].
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What [`Prefetch::usage`] returns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub bytes: u64,
    pub paused_until: Option<i64>,
}

/// Of [`Prefetch::next`], in Trash and Junk or (`:discarded` 0) outside them. Reads
/// `messages_prefetch` newest first, which needs no sort.
const NEXT: &str = "SELECT m.id, m.folder_id FROM messages m JOIN folders f ON f.id = m.folder_id
    WHERE m.account_id = :account AND m.body_state = 'headers'
      AND m.uid IS NOT NULL
      AND f.kind = 'location' AND f.selectable
      AND (f.role IS 'trash' OR f.role IS 'junk') = :discarded
      AND f.id NOT IN (SELECT value FROM json_each(:skip))
      AND NOT EXISTS (SELECT 1 FROM prefetch_failures p
                      WHERE p.message_id = m.id AND p.retry_at > :now)
    ORDER BY m.list_date DESC, m.id DESC
    LIMIT :limit";

pub struct Prefetch<'c> {
    conn: &'c Connection,
}

impl<'c> Prefetch<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Up to `limit` messages of the account whose body to fetch next, newest first, with
    /// their folders: those not downloaded, where the server has them (a message moved
    /// locally has no UID until the move is made), not in the folders in `skip`, and not
    /// waiting to be tried again after a failure (as of `now`, unix seconds).
    ///
    /// Those in Trash and Junk only once there are no others: they are the least looked
    /// for, but a search asks the server about every folder with bodies missing, and once
    /// these are here it needn't. Never both in one call, as a batch is fetched a folder
    /// at a time, which could put them first.
    pub fn next(
        &self,
        account: AccountId,
        skip: &[FolderId],
        now: i64,
        limit: usize,
    ) -> Result<Vec<(MessageId, FolderId)>> {
        let skip = to_json(&skip.iter().map(|f| f.0).collect::<Vec<_>>());
        let mut stmt = self.conn.prepare_cached(NEXT)?;
        for discarded in [false, true] {
            let rows = stmt.query_map(
                named_params! {
                    ":account": account.0,
                    ":discarded": discarded,
                    ":skip": skip,
                    ":now": now,
                    ":limit": limit as i64,
                },
                |r| Ok((MessageId(r.get(0)?), FolderId(r.get(1)?))),
            )?;
            let next = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            if !next.is_empty() {
                return Ok(next);
            }
        }
        Ok(Vec::new())
    }

    /// Records that the message's body failed to fetch at `now`, so that [`next`]
    /// leaves it out for a while, longer with each failure. Nothing if the message is
    /// gone.
    ///
    /// [`next`]: Self::next
    pub fn failed(&self, id: MessageId, now: i64) -> Result<()> {
        let failures: u32 = self
            .conn
            .query_row(
                "SELECT failures FROM prefetch_failures WHERE message_id = ?1",
                [id.0],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0)
            + 1;
        let wait = RETRY_AFTER
            .saturating_mul(4u32.saturating_pow(failures - 1))
            .min(MAX_RETRY_AFTER);
        self.conn.execute(
            "INSERT INTO prefetch_failures (message_id, failures, retry_at)
             SELECT id, ?2, ?3 FROM messages WHERE id = ?1
             ON CONFLICT (message_id) DO UPDATE SET failures = excluded.failures,
                                                    retry_at = excluded.retry_at",
            params![id.0, failures, now + wait.as_secs() as i64],
        )?;
        Ok(())
    }

    /// What was fetched for the account on `day` (UTC, days since the epoch), and until
    /// when it is paused (see [`pause`](Self::pause)), if it ever was.
    pub fn usage(&self, account: AccountId, day: i64) -> Result<Usage> {
        Ok(self
            .conn
            .query_row(
                "SELECT iif(day = ?2, bytes, 0), paused_until FROM prefetch_usage
                 WHERE account_id = ?1",
                params![account.0, day],
                |r| {
                    Ok(Usage {
                        bytes: r.get::<_, i64>(0)? as u64,
                        paused_until: r.get(1)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    /// Pauses the prefetch of the account until `until` (unix seconds): the server said it
    /// went over its download limit. Call after [`add_used`](Self::add_used) for the day,
    /// which makes the row; nothing if the account is gone.
    pub fn pause(&self, account: AccountId, until: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE prefetch_usage SET paused_until = ?2 WHERE account_id = ?1",
            params![account.0, until],
        )?;
        Ok(())
    }

    /// Adds `bytes` to what was fetched for the account on `day`, forgetting earlier days.
    /// Nothing if the account is gone.
    pub fn add_used(&self, account: AccountId, day: i64, bytes: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO prefetch_usage (account_id, day, bytes)
             SELECT id, ?2, ?3 FROM accounts WHERE id = ?1
             ON CONFLICT (account_id) DO UPDATE SET
                 bytes = iif(day = excluded.day, bytes + excluded.bytes, excluded.bytes),
                 day = excluded.day",
            params![account.0, day, bytes as i64],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use catmail_types::{FlagState, FolderKind, FolderRole};

    use super::*;
    use crate::folders::RemoteFolder;
    use crate::messages::{NewBody, NewMessage};
    use crate::test_util::{self, TestStore};

    /// An account with INBOX, Archive, Trash and Junk, each with two messages (UIDs 1 and
    /// 2, dated by folder and UID); and the folders' ids.
    fn setup() -> (TestStore, AccountId, [FolderId; 4]) {
        let store = test_util::store();
        let account = test_util::account(&store, "a@example.com");
        let folder = |name: &str, role| RemoteFolder {
            remote_name: name.into(),
            name: name.into(),
            delimiter: Some('/'),
            role,
            kind: FolderKind::Location,
            selectable: true,
            inferiors: true,
        };
        let remote = [
            folder("INBOX", Some(FolderRole::Inbox)),
            folder("Archive", None),
            folder("Trash", Some(FolderRole::Trash)),
            folder("Junk", Some(FolderRole::Junk)),
        ];
        let folders = store
            .write(|db| {
                db.folders().reconcile(account, &remote)?;
                let listed = db.folders().list(account)?;
                let mut ids = Vec::new();
                for (i, name) in ["INBOX", "Archive", "Trash", "Junk"].iter().enumerate() {
                    let id = listed.iter().find(|f| f.name == *name).unwrap().id;
                    db.folders().reset_uidvalidity(id, 1)?;
                    let messages: Vec<_> = (1..=2)
                        .map(|uid| NewMessage {
                            state: FlagState {
                                uid,
                                ..Default::default()
                            },
                            date: Some(i as i64 * 10 + uid as i64),
                            ..Default::default()
                        })
                        .collect();
                    db.messages().upsert(account, id, &messages)?;
                    ids.push(id);
                }
                Ok(ids)
            })
            .unwrap();
        (store, account, folders.try_into().unwrap())
    }

    use test_util::message_id as id;

    fn next(
        store: &TestStore,
        account: AccountId,
        skip: &[FolderId],
        now: i64,
        limit: usize,
    ) -> Vec<MessageId> {
        let next = store.read(|db| db.prefetch().next(account, skip, now, limit));
        next.unwrap().into_iter().map(|(id, _)| id).collect()
    }

    #[test]
    fn next_is_newest_first_with_trash_once_the_rest_is_done_and_leaves_out_downloaded() {
        let (store, account, [inbox, archive, trash, junk]) = setup();
        // Not Trash's and Junk's, though the newest and there is room for them.
        assert_eq!(
            next(&store, account, &[], 0, 10),
            [
                id(&store, archive, 2),
                id(&store, archive, 1),
                id(&store, inbox, 2),
                id(&store, inbox, 1),
            ]
        );
        // Once the others are done (or left out).
        assert_eq!(
            next(&store, account, &[inbox, archive], 0, 3),
            [
                id(&store, junk, 2),
                id(&store, junk, 1),
                id(&store, trash, 2)
            ]
        );

        let downloaded = id(&store, archive, 2);
        store
            .write(|db| {
                db.messages()
                    .store_body(downloaded, &NewBody::default())
                    .map(drop)
            })
            .unwrap();
        assert_eq!(
            next(&store, account, &[inbox, trash, junk], 0, 10),
            [id(&store, archive, 1)]
        );
    }

    #[test]
    fn next_reads_its_index_rather_than_every_message() {
        let (store, account, _) = setup();
        for discarded in [false, true] {
            let plan: Vec<String> = store
                .read(|db| {
                    let mut stmt = db.conn.prepare(&format!("EXPLAIN QUERY PLAN {NEXT}"))?;
                    let rows = stmt.query_map(
                        named_params! {
                            ":account": account.0, ":discarded": discarded, ":skip": "[]",
                            ":now": 0, ":limit": 10,
                        },
                        |r| r.get::<_, String>(3),
                    )?;
                    Ok(rows.collect::<rusqlite::Result<_>>()?)
                })
                .unwrap();
            assert!(
                plan.iter().any(|step| step.contains("messages_prefetch")),
                "{plan:?}"
            );
            assert!(
                !plan.iter().any(|step| step.contains("TEMP B-TREE")),
                "{plan:?}"
            );
        }
    }

    #[test]
    fn a_failed_message_waits_longer_each_time_until_its_body_is_stored() {
        let (store, account, [inbox, ..]) = setup();
        let failing = id(&store, inbox, 2);
        let hour = 60 * 60;

        store.write(|db| db.prefetch().failed(failing, 0)).unwrap();
        assert!(!next(&store, account, &[], hour - 1, 10).contains(&failing));
        assert!(next(&store, account, &[], hour, 10).contains(&failing));

        store
            .write(|db| db.prefetch().failed(failing, hour))
            .unwrap();
        assert!(!next(&store, account, &[], 5 * hour - 1, 10).contains(&failing));
        assert!(next(&store, account, &[], 5 * hour, 10).contains(&failing));

        // Opened meanwhile: forgotten.
        store
            .write(|db| {
                db.messages()
                    .store_body(failing, &NewBody::default())
                    .map(drop)
            })
            .unwrap();
        let left: i64 = store
            .read(|db| {
                Ok(db
                    .conn
                    .query_row("SELECT count(*) FROM prefetch_failures", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn usage_counts_one_day_at_a_time() {
        let (store, account, _) = setup();
        let used = |day| {
            store
                .read(|db| db.prefetch().usage(account, day))
                .unwrap()
                .bytes
        };
        assert_eq!(used(7), 0);
        store
            .write(|db| {
                db.prefetch().add_used(account, 7, 100)?;
                db.prefetch().add_used(account, 7, 50)
            })
            .unwrap();
        assert_eq!(used(7), 150);
        store
            .write(|db| db.prefetch().add_used(account, 8, 10))
            .unwrap();
        assert_eq!((used(7), used(8)), (0, 10));

        // Removed while fetching: nothing to count it on.
        store
            .write(|db| db.prefetch().add_used(AccountId(account.0 + 1), 8, 10))
            .unwrap();
    }

    #[test]
    fn a_pause_outlives_the_day_and_the_usage() {
        let (store, account, _) = setup();
        let usage = |day| store.read(|db| db.prefetch().usage(account, day)).unwrap();
        assert_eq!(usage(7).paused_until, None);
        store
            .write(|db| {
                db.prefetch().add_used(account, 7, 100)?;
                db.prefetch().pause(account, 1_000)
            })
            .unwrap();
        assert_eq!(
            usage(7),
            Usage {
                bytes: 100,
                paused_until: Some(1_000)
            }
        );
        store
            .write(|db| db.prefetch().add_used(account, 8, 10))
            .unwrap();
        assert_eq!(usage(8).paused_until, Some(1_000));
    }
}
