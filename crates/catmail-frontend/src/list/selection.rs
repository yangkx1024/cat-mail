//! Where the list's selection goes when the rows are read again: on with the row, on to
//! the next once a message the user moved or deleted has left ([`Removal`]), or to a
//! message a notification asked for once it is listed ([`Reveal`]). [`ListModel`] keeps
//! them.
//!
//! [`ListModel`]: super::ListModel

use catmail_core::MessageId;

use super::{Row, RowId, Select};

/// Where the selection goes when the rows are replaced.
#[derive(Debug, PartialEq, Eq)]
enum Reselect {
    /// The selected row, at its index among the new rows; nothing if nothing was
    /// selected. `removing` is still waited for: the row is still there (a page read
    /// before the change committed, or the change failed).
    Keep {
        ix: Option<usize>,
        removing: Option<MessageId>,
    },
    /// The user moved or deleted the selected message: on to the next row still there,
    /// at this index among the new rows.
    Next(usize),
    /// The selected row is gone (expunged, sent, or a new UIDVALIDITY).
    Gone,
}

/// `selected` is the selected row's index and identity in `before`, the rows replaced,
/// `removing` the message the user moved or deleted (the selection moves on to the next
/// row once it is gone).
fn reselect(
    selected: Option<(usize, RowId)>,
    removing: Option<MessageId>,
    before: &[Row],
    rows: &[Row],
) -> Reselect {
    let Some((at, id)) = selected else {
        return Reselect::Keep {
            ix: None,
            removing: None,
        };
    };
    // Only while the message is still the one selected: the user may have moved on.
    let removing = removing.filter(|m| id == RowId::Message(*m));
    let position = |id: RowId| rows.iter().position(|r| r.id() == id);
    match position(id) {
        Some(ix) => Reselect::Keep {
            ix: Some(ix),
            removing,
        },
        // The next row that is still there, or else the one before: rows around it may
        // have gone with it (several moved at once), or come (new mail).
        None if removing.is_some() => {
            let after = before.get(at + 1..).unwrap_or_default().iter();
            let before = before.get(..at).unwrap_or_default().iter().rev();
            after
                .chain(before)
                .find_map(|r| position(r.id()))
                .map_or(Reselect::Gone, Reselect::Next)
        }
        None => Reselect::Gone,
    }
}

/// The wait for a message the user moved or deleted to leave the list, so the next one
/// opens (see `docs/architecture/gui.md`, "读与改").
///
/// Whether it leaves is the store's to say, not the frontend's to predict (on Gmail, one
/// moved to a label from All Mail stays): once the change is made, the rows are read
/// again, and if that read (or a later one) still has the message, it stays selected and
/// is no longer waited for. Reads in flight before may predate the change. Each removal
/// has its number, and only the latest's settling counts: archiving and then deleting
/// one message, the archive's read still finding it doesn't end the wait for the delete.
/// The list numbers its reads of the rows shown (not of more pages) from 1.
#[derive(Debug, Default)]
pub(super) struct Removal {
    removing: Option<MessageId>,
    /// Removals asked for so far: the latest is `removing`'s.
    count: u64,
    /// The change behind removal number `.0` is made: the rows of read number `.1` or
    /// later show it.
    settled: Option<(u64, u64)>,
}

impl Removal {
    /// The rows go on to the next once `id`, the selected message, goes. Returns the
    /// removal's number, for [`settled`](Self::settled).
    pub fn expect(&mut self, id: MessageId) -> u64 {
        self.removing = Some(id);
        self.count += 1;
        self.settled = None;
        self.count
    }

    /// The change behind `removal` was made locally (`made`), or failed: a failure stays
    /// at once. Returns whether to read the rows again, the read numbered `next_read`
    /// being the first that shows the change. Nothing changes if a later removal has been
    /// asked for since, or the wait is over.
    pub fn settled(&mut self, removal: u64, made: bool, next_read: u64) -> bool {
        if self.count != removal || self.removing.is_none() {
            return false;
        }
        if !made {
            self.removing = None;
            return false;
        }
        self.settled = Some((removal, next_read));
        true
    }

    /// The rows of read number `read` came: if they show the change and still have the
    /// message, it stays.
    pub fn read(&mut self, read: u64, rows: &[Row]) {
        let Some((removal, from)) = self.settled else {
            return;
        };
        if read < from {
            return;
        }
        self.settled = None;
        // `expect` drops a settlement of an earlier removal.
        debug_assert_eq!(removal, self.count);
        if let Some(id) = self.removing
            && rows.iter().any(|r| r.id() == RowId::Message(id))
        {
            self.removing = None;
        }
    }

    /// Where the selection goes as `before` are replaced by `rows`, `selected` selected
    /// ([`reselect`]), still waiting while the message is listed.
    pub fn reselect(&mut self, selected: Option<RowId>, before: &[Row], rows: &[Row]) -> Select {
        let at = selected.and_then(|id| before.iter().position(|r| r.id() == id));
        match reselect(at.zip(selected), self.removing.take(), before, rows) {
            Reselect::Keep { removing, .. } => {
                self.removing = removing;
                Select::Keep
            }
            Reselect::Next(ix) => Select::Open(rows[ix].id()),
            Reselect::Gone => Select::Clear,
        }
    }

    /// Nothing is waited for: another source is shown.
    pub fn clear(&mut self) {
        self.removing = None;
        self.settled = None;
    }

    /// Whether a message is waited for to leave.
    #[cfg(test)]
    fn is_waiting(&self) -> bool {
        self.removing.is_some()
    }
}

/// A message to select once the rows have it: what a notification clicked was about,
/// which may have just arrived and not be listed yet. It is waited for until the first
/// read started after asking has come and still doesn't have it; another source shown,
/// or a row the user selects meanwhile, ends the wait ([`Reveal::forget`]). Reads are
/// numbered as for [`Removal`].
#[derive(Debug, Default)]
pub(super) struct Reveal(Option<(MessageId, u64)>);

impl Reveal {
    /// Waits for `id`, which the read numbered `next_read` (and any later) should have.
    pub fn ask(&mut self, id: MessageId, next_read: u64) {
        self.0 = Some((id, next_read));
    }

    pub fn forget(&mut self) {
        self.0 = None;
    }

    /// The rows of read number `read` came (or are those shown, of the latest read so
    /// far): where the message waited for is among them, if it is, to select. The wait
    /// ends then, or when it should have been there.
    pub fn read(&mut self, read: u64, rows: &[Row]) -> Option<usize> {
        let (id, from) = self.0?;
        let at = rows.iter().position(|r| r.id() == RowId::Message(id));
        if at.is_some() || read >= from {
            self.0 = None;
        }
        at
    }

    /// Whether a message is waited for.
    pub fn is_waiting(&self) -> bool {
        self.0.is_some()
    }
}

#[cfg(test)]
mod tests {
    use catmail_core::{DraftId, MessageSummary};

    use super::*;
    use crate::fixtures;

    fn message(id: i64) -> Row {
        Row::Message(MessageSummary {
            date: None,
            ..fixtures::message(id, 1, 1)
        })
    }

    #[test]
    fn the_selection_follows_the_row_or_moves_on() {
        use Reselect::*;
        let m = |id| RowId::Message(MessageId(id));
        let rows = |ids: &[i64]| -> Vec<Row> { ids.iter().map(|&id| message(id)).collect() };
        let removing = Some(MessageId(2));
        #[rustfmt::skip]
        let cases = [
            // Nothing selected: nothing is, and a removal isn't waited for.
            (None, removing, &[1, 2][..], &[1, 2][..], Keep { ix: None, removing: None }),
            // Still there, wherever it is now.
            (Some((1, m(2))), None, &[1, 2], &[0, 1, 2], Keep { ix: Some(2), removing: None }),
            // Moved but still listed: keep waiting.
            (Some((1, m(2))), removing, &[1, 2, 3], &[1, 2, 3], Keep { ix: Some(1), removing }),
            // Waiting for another message than the selected one: the user moved on.
            (Some((0, m(1))), removing, &[1, 2, 3], &[1, 3], Keep { ix: Some(0), removing: None }),
            // Moved away: the next row still there, or the one before.
            (Some((1, m(2))), removing, &[1, 2, 3, 4], &[1, 3, 4], Next(1)),
            (Some((1, m(2))), removing, &[1, 2], &[1], Next(0)),
            // With those above it: not the row now at its index.
            (Some((1, m(2))), removing, &[1, 2, 3, 4, 5], &[3, 4, 5], Next(0)),
            // With those below it, and new mail above.
            (Some((0, m(2))), removing, &[2, 3, 4], &[9, 4], Next(1)),
            // Moved away and nothing left of the rows around it, or gone by itself.
            (Some((0, m(2))), removing, &[2], &[], Gone),
            (Some((0, m(2))), removing, &[2, 3], &[9], Gone),
            (Some((1, m(2))), None, &[1, 2, 3], &[1, 3], Gone),
            (Some((0, RowId::Draft(DraftId(2)))), removing, &[1], &[1], Gone),
        ];
        for (selected, removing, before, ids, expected) in cases {
            assert_eq!(
                reselect(selected, removing, &rows(before), &rows(ids)),
                expected,
                "{selected:?} {removing:?} {before:?} {ids:?}"
            );
        }
    }

    #[test]
    fn a_message_to_reveal_is_waited_for_until_a_read_that_should_have_it() {
        let rows = |ids: &[i64]| -> Vec<Row> { ids.iter().map(|&id| message(id)).collect() };
        let mut reveal = Reveal::default();
        // Listed already.
        reveal.ask(MessageId(2), 4);
        assert_eq!(reveal.read(3, &rows(&[1, 2])), Some(1));
        assert!(!reveal.is_waiting());

        // Not yet: a read from before asking doesn't end the wait, the next does.
        reveal.ask(MessageId(5), 4);
        assert_eq!(reveal.read(3, &rows(&[1, 2])), None);
        assert!(reveal.is_waiting());
        assert_eq!(reveal.read(4, &rows(&[1, 2])), None);
        assert!(!reveal.is_waiting());

        reveal.ask(MessageId(5), 4);
        assert_eq!(reveal.read(4, &rows(&[5, 1, 2])), Some(0));
        reveal.ask(MessageId(5), 4);
        reveal.forget();
        assert_eq!(reveal.read(4, &rows(&[5])), None);
    }

    #[test]
    fn a_removal_waits_until_a_read_after_the_change() {
        let rows = |ids: &[i64]| -> Vec<Row> { ids.iter().map(|&id| message(id)).collect() };
        let selected = Some(RowId::Message(MessageId(2)));
        let mut removal = Removal::default();
        let first = removal.expect(MessageId(2));
        // A read in flight, started before the change was made: it says nothing.
        assert!(removal.settled(first, true, 2));
        removal.read(1, &rows(&[1, 2, 3]));
        assert!(removal.is_waiting());
        // Made, and the message still listed (Gmail, to a label from All Mail): it stays.
        removal.read(2, &rows(&[1, 2, 3]));
        assert!(!removal.is_waiting());
        let rows_now = rows(&[1, 3]);
        assert_eq!(
            removal.reselect(selected, &rows(&[1, 2, 3]), &rows_now),
            Select::Clear
        );

        // Archived, then deleted: the archive settling doesn't end the wait for the delete.
        let archive = removal.expect(MessageId(2));
        let delete = removal.expect(MessageId(2));
        assert!(!removal.settled(archive, true, 3));
        removal.read(3, &rows(&[1, 2, 3]));
        assert!(removal.is_waiting());
        assert_eq!(
            removal.reselect(selected, &rows(&[1, 2, 3]), &rows(&[1, 2, 3])),
            Select::Keep
        );
        assert!(removal.is_waiting(), "still listed: still waited for");
        assert_eq!(
            removal.reselect(selected, &rows(&[1, 2, 3]), &rows(&[1, 3])),
            Select::Open(RowId::Message(MessageId(3)))
        );
        // Settling once it has gone does nothing.
        assert!(!removal.settled(delete, true, 4));

        // A change that failed stays at once.
        let failed = removal.expect(MessageId(2));
        assert!(!removal.settled(failed, false, 5));
        assert!(!removal.is_waiting());
    }
}
