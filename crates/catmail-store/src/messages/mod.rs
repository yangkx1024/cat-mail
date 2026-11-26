//! The messages' rows: one for each message on the server, where it is there and what of
//! it is stored here. [`Messages`] is in three parts: what sync stores, changes and
//! deletes ([`sync`]); the lists, searches and messages read ([`lists`]); and the
//! bodies, with their attachments and the search index ([`bodies`]).

mod bodies;
#[cfg(test)]
mod fixtures;
mod lists;
mod sync;

use std::cell::RefCell;
use std::collections::BTreeSet;

use catmail_types::{AccountId, FolderId};
use rusqlite::{Connection, params};

use crate::Result;
use crate::columns::to_json;

pub use bodies::{AttachmentSource, MessageLocation, NewBody, StaleBody};
pub use sync::{NewMessage, Upserted};

/// Joins a message `m` to the pending move `o` that put it where it is, if it has no
/// UID yet: the server still has it at the move's (folder, UIDVALIDITY, UID).
pub(crate) const MOVING: &str = "LEFT JOIN pending_ops o ON m.uid IS NULL AND o.message_id = m.id
     AND o.kind = 'move' AND o.status = 'pending'";

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
    /// [`update_flags`](Self::update_flags), [`delete_uids`](Self::delete_uids),
    /// [`delete_gone`](Self::delete_gone) and the
    /// bodies stored ([`store_body`](Self::store_body), [`rebuild_body`](Self::rebuild_body),
    /// [`refresh_html`](Self::refresh_html)): snippets and attachment markers show there
    /// too. Only this `Messages`' changes: another from [`Db::messages`](crate::Db::messages)
    /// knows none of them.
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
        // Two statements, so that a few UIDs are looked up by (folder, UID) rather than
        // among every row of the folder: this runs for each message a sync removes.
        let which = match uids {
            Some(_) => "AND uid IN (SELECT value FROM json_each(?2))",
            None => "AND ?2 IS NULL",
        };
        let mut stmt = self.conn.prepare_cached(&format!(
            "WITH m AS MATERIALIZED (
                 SELECT id, flags FROM messages WHERE folder_id = ?1 {which})
             SELECT DISTINCT folder_id FROM message_labels
             WHERE message_id IN (SELECT id FROM m)
             UNION
             SELECT s.id FROM folders f JOIN starred s ON s.account_id = f.account_id
             WHERE f.id = ?1 AND EXISTS (SELECT 1 FROM m WHERE flags & 2 <> 0)"
        ))?;
        let uids = uids.map(to_json);
        let shown = stmt.query_map(params![folder.0, uids], |r| Ok(FolderId(r.get(0)?)))?;
        let mut touched = self.touched.borrow_mut();
        for folder in shown {
            touched.insert(folder?);
        }
        Ok(())
    }
}
