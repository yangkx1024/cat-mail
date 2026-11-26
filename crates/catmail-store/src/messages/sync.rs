//! What sync stores of the messages, changes and deletes: rows, flags and Gmail's labels,
//! as the server has them.

use std::collections::HashMap;

use catmail_types::{AccountId, Address, FlagState, Flags, FolderId, GmailLabel, MessageId};
use rusqlite::{Connection, OptionalExtension, named_params, params};

use super::{MessageLocation, Messages};
use crate::Result;
use crate::columns::{to_id_list, to_json};
use crate::folders::Folders;

/// A message as first seen during sync: header summary plus its current flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewMessage {
    pub state: FlagState,
    pub internal_date: Option<i64>,
    pub size: Option<u32>,
    pub message_id: Option<String>,
    pub references: Vec<String>,
    pub subject: Option<String>,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    /// The Date header; INTERNALDATE is used when it is missing.
    pub date: Option<i64>,
    pub has_attachments: bool,
    /// X-GM-MSGID, on Gmail.
    pub gm_msgid: Option<u64>,
    /// The server's preview came with it (see [`Messages::store_previews`]), as this
    /// snippet.
    pub previewed: bool,
    pub snippet: Option<String>,
}

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

impl Messages<'_> {
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

    #[cfg(test)]
    pub(crate) fn count(&self, folder: FolderId) -> Result<u32> {
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
                                   gm_msgid, size, message_id_hdr, references_hdr, subject,
                                   from_json, to_json, cc_json, reply_to_json, date,
                                   has_attachments, previewed, snippet)
             SELECT :account, :folder, :uid, :flags, :keywords, :modseq,
                    :gm_msgid, :size, :message_id, :references, :subject,
                    :from, :to, :cc, :reply_to, :date, :has_attachments, :previewed, :snippet
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
                ":size": m.size,
                ":message_id": m.message_id,
                ":references": to_id_list(&m.references),
                ":subject": m.subject,
                ":from": to_json(&m.from),
                ":to": to_json(&m.to),
                ":cc": to_json(&m.cc),
                ":reply_to": to_json(&m.reply_to),
                ":date": m.date.or(m.internal_date),
                ":has_attachments": m.has_attachments,
                ":previewed": m.previewed,
                ":snippet": m.snippet,
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

    /// Returns how many rows were deleted. Callers go through
    /// [`delete_uids_of`](Self::delete_uids_of), which checks the UIDVALIDITY.
    pub(crate) fn delete_uids(&self, folder: FolderId, uids: &[u32]) -> Result<usize> {
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
        self.note_shown_in(location.server_folder, Some(&[location.uid]))?;
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use catmail_types::{
        AccountId, FlagState, Flags, FolderId, FolderKind, FolderRole, GmailLabel, ListQuery,
        MessageId,
    };
    use rusqlite::params;

    use super::*;
    use crate::Store;
    use crate::folders::RemoteFolder;
    use crate::messages::fixtures::{
        body, copy, folders, labelled, message, setup, subjects, user,
    };
    use crate::messages::{MessageLocation, NewBody};
    use crate::test_util::{self, gmail};

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
            .read(|db| db.messages().list(folder.id, &ListQuery::new(10)))
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
            .read(|db| db.messages().list(f["INBOX"], &ListQuery::new(2)))
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
            .read(|db| db.messages().list(f["Kept"], &ListQuery::new(5)))
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(left, [ids[1]]);
        assert!(
            store
                .read(|db| db.messages().list(f["INBOX"], &ListQuery::new(5)))
                .unwrap()
                .is_empty()
        );
    }

    /// (total, unread) of every folder of the account, checked against counting anew.
    fn counts(store: &Store, account: AccountId) -> HashMap<String, (u32, u32)> {
        store
            .read(|db| {
                let mut counts = HashMap::new();
                for f in db.folders().list(account)? {
                    let shown = db.messages().list(f.id, &ListQuery::new(u32::MAX))?;
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
            .read(|db| db.messages().list(f["Work"], &ListQuery::new(10)))
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

        // Its body stored or rebuilt: its snippet and attachment marker show there too.
        let three = store
            .read(|db| {
                Ok(MessageId(db.conn.query_row(
                    "SELECT id FROM messages WHERE gm_msgid = 103",
                    [],
                    |r| r.get(0),
                )?))
            })
            .unwrap();
        let mut showing = vec![f["Work"], hidden.id, f["[Gmail]/Starred"]];
        showing.sort();
        let touched = store
            .write(|db| {
                let messages = db.messages();
                messages.store_body(three, &body())?;
                let stored = messages.touched_labels();
                let rebuilt = NewBody {
                    version: 2,
                    ..body()
                };
                messages.rebuild_body(three, 1, &rebuilt)?;
                Ok([stored, messages.touched_labels()])
            })
            .unwrap();
        assert_eq!(touched, [showing.clone(), showing]);

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

        // Gone from the server, and with it from its labels, whose lists change.
        let gone = store
            .read(|db| {
                let messages = db.messages();
                let listed = messages.list(all_mail, &ListQuery::new(50))?;
                let gone = listed
                    .iter()
                    .find(|m| m.subject.as_deref() == Some("subject 3"));
                messages.location(gone.unwrap().id)
            })
            .unwrap()
            .unwrap();
        let (deleted, touched) = store
            .write(|db| {
                let messages = db.messages();
                Ok((messages.delete_gone(&gone)?, messages.touched_labels()))
            })
            .unwrap();
        assert!(deleted);
        let mut expected = vec![f["Work"], hidden.id, f["[Gmail]/Starred"]];
        expected.sort();
        assert_eq!(touched, expected);
        let counted = counts(&store, account);
        assert_eq!(counted["Work"], (1, 0));
        assert_eq!(counted["Hidden"], (1, 0));
        assert_eq!(counted["[Gmail]/Starred"], (1, 0));

        // A message is one X-GM-MSGID: found in another location (another client moved
        // it), its row moves there, and the labels that showed it no longer do.
        let trash = f["[Gmail]/Trash"];
        let id = |folder| {
            store
                .read(|db| db.messages().list(folder, &ListQuery::new(50)))
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
            .read(|db| db.messages().list(all_mail, &ListQuery::new(1)))
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
                inferiors: true,
            })
            .collect();
        let hidden: Vec<_> = all
            .iter()
            .filter(|f| f.role != Some(FolderRole::Flagged))
            .cloned()
            .collect();
        let reconcile = |remote: &[RemoteFolder]| {
            store
                .write(|db| db.reconcile_folders(account, remote))
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
}
