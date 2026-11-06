//! The list's rows and their reads: what is listed and how it is laid out ([`Listing`]),
//! and the reads that fill it ([`ListModel`]): one at a time, those of a source shown
//! before dropped, a page more or the rows shown again, and where the selection goes once
//! they come, with the messages the user moved or a notification asked for.

use std::collections::HashSet;
use std::rc::Rc;

use catmail_core::{MailClient, MessageCursor, MessageId, MessageSummary};

use super::{Days, PAGE, Removal, Reveal, Row, RowId, fetch};
use crate::mailbox::Source;
use crate::refresh::Coalesce;

/// What the list shows: a source's rows, grouped by day, and the lines they are laid out
/// as. The lines refer to the rows and the days, so a page more only adds lines.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    source: Option<Source>,
    rows: Vec<Row>,
    days: Days,
    lines: Vec<LineAt>,
    /// More rows may follow those read.
    has_more: bool,
}

/// A line of the list, by where its day or row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineAt {
    Day(usize),
    Row(usize),
}

/// A line of the list: a day's heading or one of its rows.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Day(String),
    Row(Row),
}

/// A line's identity, across reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineKey {
    /// A day can head two runs, when the rows aren't by date: by its position.
    Day(usize),
    Row(RowId),
}

impl Listing {
    /// `source`'s, with nothing read yet.
    pub fn new(source: Option<Source>) -> Self {
        Self {
            source,
            ..Self::default()
        }
    }

    pub fn source(&self) -> Option<&Source> {
        self.source.as_ref()
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn messages(&self) -> impl Iterator<Item = &MessageSummary> {
        self.rows.iter().filter_map(Row::message)
    }

    pub fn row(&self, id: RowId) -> Option<&Row> {
        self.rows.iter().find(|row| row.id() == id)
    }

    pub fn message(&self, id: MessageId) -> Option<&MessageSummary> {
        self.messages().find(|m| m.id == id)
    }

    /// The messages checked, top to bottom.
    pub fn checked(&self, checked: &HashSet<MessageId>) -> Vec<MessageSummary> {
        if checked.is_empty() {
            return Vec::new();
        }
        let checked = self.messages().filter(|m| checked.contains(&m.id));
        checked.cloned().collect()
    }

    /// What a command from a key acts on: the messages checked, or else the one selected.
    pub fn keyed(
        &self,
        checked: &HashSet<MessageId>,
        selected: Option<RowId>,
    ) -> Vec<MessageSummary> {
        let messages = self.checked(checked);
        if !messages.is_empty() {
            return messages;
        }
        let selected = match selected {
            Some(RowId::Message(id)) => self.message(id),
            _ => None,
        };
        selected.into_iter().cloned().collect()
    }

    /// How many lines the list has.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The line at `ix`.
    pub fn line(&self, ix: usize) -> Option<Line> {
        Some(match *self.lines.get(ix)? {
            LineAt::Day(day) => Line::Day(self.days.get(day)?.label.clone()),
            LineAt::Row(row) => Line::Row(self.rows[row].clone()),
        })
    }

    /// Each line's identity, top to bottom.
    pub fn keys(&self) -> Vec<LineKey> {
        let key = |(ix, line): (usize, &LineAt)| match *line {
            LineAt::Day(_) => LineKey::Day(ix),
            LineAt::Row(row) => LineKey::Row(self.rows[row].id()),
        };
        self.lines.iter().enumerate().map(key).collect()
    }

    /// The line of row `id`, if it is listed.
    pub fn line_of(&self, id: RowId) -> Option<usize> {
        let row = self.rows.iter().position(|row| row.id() == id)?;
        self.lines.iter().position(|&line| line == LineAt::Row(row))
    }

    fn replace(&mut self, rows: Vec<Row>, more: bool) {
        self.days = Days::new(&rows);
        self.rows = rows;
        self.lines.clear();
        self.lay_out(0);
        self.has_more = more;
    }

    /// The page starts after the last row shown, whatever came or went above it since;
    /// the refresh such a change brings updates those.
    fn append(&mut self, rows: Vec<Row>, more: bool) {
        let from = self.rows.len();
        self.rows.extend(rows);
        self.days.extend(&self.rows, from);
        self.lay_out(from);
        self.has_more = more;
    }

    /// Adds the lines of `rows[from..]`: the rest of the last day, then the days after.
    fn lay_out(&mut self, from: usize) {
        for (ix, day) in self.days.iter().enumerate() {
            let end = day.start + day.len;
            if end <= from {
                continue;
            }
            if day.start >= from {
                self.lines.push(LineAt::Day(ix));
            }
            self.lines
                .extend((day.start.max(from)..end).map(LineAt::Row));
        }
    }
}

/// A read of the list for [`ListModel::loaded`]: which rows of which source.
#[derive(Debug, Clone)]
pub struct Fetch {
    source: Source,
    found_on_server: Rc<[MessageId]>,
    limit: u32,
    after: Option<MessageCursor>,
    /// The source's, counted when it is shown.
    generation: u64,
    /// The number of a read of the rows shown, not of a page more.
    read: Option<u64>,
}

impl Fetch {
    /// Reads the rows, and whether there may be more.
    pub async fn run(&self, client: &MailClient) -> catmail_core::Result<(Vec<Row>, bool)> {
        let (source, found) = (&self.source, &self.found_on_server);
        fetch(client, source, found, self.limit, self.after).await
    }
}

/// What a read of the list changes.
#[derive(Debug, PartialEq)]
pub struct Loaded {
    /// The rows to show; nothing if they are as shown, or the read failed.
    pub rows: Option<Rows>,
    pub select: Select,
    /// The messages still checked, if any of those checked are gone (moved, deleted, or
    /// expunged on the server).
    pub checked: Option<HashSet<MessageId>>,
    /// Read the rows shown again: something changed while they were read.
    pub again: bool,
}

/// Rows a read brought, for the listing.
#[derive(Debug, Clone, PartialEq)]
pub enum Rows {
    /// The rows shown, read again: in place of them.
    Replace(Vec<Row>, bool),
    /// A page more, after them.
    Append(Vec<Row>, bool),
}

impl Rows {
    pub fn apply(self, listing: &mut Listing) {
        match self {
            Rows::Replace(rows, more) => listing.replace(rows, more),
            Rows::Append(rows, more) => listing.append(rows, more),
        }
    }
}

/// Where the selection goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    Keep,
    /// As if the user opened it: the next row after one the user moved, or the message a
    /// notification was about.
    Open(RowId),
    /// The row selected is gone by itself: expunged, sent, a new UIDVALIDITY.
    Clear,
}

/// The reads of the list. Not what it shows: that is the [`Listing`], which the frontend
/// keeps where its views read it, and hands in.
#[derive(Debug, Default)]
pub struct ListModel {
    /// Counted when the source changes, so rows read for the one before are dropped.
    generation: u64,
    /// One read at a time; the engine reporting a change meanwhile reads again after it.
    reading: Coalesce,
    /// Search results the servers found that local search can't, by their text: read
    /// with every page of the results.
    found_on_server: Rc<[MessageId]>,
    /// Reads of the rows shown started so far (not of more pages).
    reads: u64,
    /// The message the user moved or deleted: once it is gone while still selected, the
    /// next row still there opens.
    removal: Removal,
    /// The message a notification was about, to open once the rows have it.
    reveal: Reveal,
}

impl ListModel {
    /// Shows `source` from the top, or nothing: the listing to show, and its first read.
    /// Whatever was waited for of the source before is no longer.
    pub fn show(&mut self, source: Option<Source>) -> (Listing, Option<Fetch>) {
        self.generation += 1;
        self.reading = Coalesce::default();
        self.found_on_server = Rc::new([]);
        self.removal.clear();
        self.reveal.forget();
        let listing = Listing::new(source);
        let fetch = self.start(&listing, PAGE, None);
        (listing, fetch)
    }

    /// The source shown, counted: whether one is still shown, for work started for it.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the list is being read.
    pub fn is_reading(&self) -> bool {
        self.reading.is_running()
    }

    /// The rows `listing` shows (at least a page), read again after the engine reported
    /// a change, keeping the selected row selected. During a sync changes come faster
    /// than a page is read: at most one read runs, and one more follows if asked for
    /// meanwhile ([`Loaded::again`]).
    pub fn refresh(&mut self, listing: &Listing) -> Option<Fetch> {
        let shown = u32::try_from(listing.rows.len()).unwrap_or(u32::MAX);
        self.start(listing, shown.max(PAGE), None)
    }

    /// The next page, when the list is scrolled to its end: only of messages (folders,
    /// the inboxes and search results), only if there may be more, and not while the list
    /// is being read, which leaves it to a later scroll.
    pub fn more(&mut self, listing: &Listing) -> Option<Fetch> {
        if !listing.has_more || self.reading.is_running() {
            return None;
        }
        let after = listing.rows.last()?.message()?.cursor();
        self.start(listing, PAGE, Some(after))
    }

    /// The servers found `found` for the search shown in `generation`: whether to read
    /// the results again with them.
    pub fn found_on_server(&mut self, generation: u64, found: Vec<MessageId>) -> bool {
        if found.is_empty() || generation != self.generation {
            return false;
        }
        self.found_on_server = found.into();
        true
    }

    fn start(
        &mut self,
        listing: &Listing,
        limit: u32,
        after: Option<MessageCursor>,
    ) -> Option<Fetch> {
        let source = listing.source.clone()?;
        if !self.reading.start() {
            return None;
        }
        let read = after.is_none().then(|| {
            self.reads += 1;
            self.reads
        });
        Some(Fetch {
            source,
            found_on_server: self.found_on_server.clone(),
            limit,
            after,
            generation: self.generation,
            read,
        })
    }

    /// `fetch` came back with rows and whether there may be more, or `None` if it failed,
    /// while `listing` is shown with `selected` selected and `checked` checked. Nothing if
    /// another source is shown since.
    pub fn loaded(
        &mut self,
        fetch: &Fetch,
        result: Option<(Vec<Row>, bool)>,
        listing: &Listing,
        selected: Option<RowId>,
        checked: &HashSet<MessageId>,
    ) -> Option<Loaded> {
        if fetch.generation != self.generation {
            return None;
        }
        let again = self.reading.finish();
        let mut loaded = Loaded {
            rows: None,
            select: Select::Keep,
            checked: None,
            again,
        };
        let Some((rows, more)) = result else {
            return Some(loaded);
        };
        let Some(read) = fetch.read else {
            loaded.rows = Some(Rows::Append(rows, more));
            return Some(loaded);
        };
        self.removal.read(read, &rows);
        // A sync's events often bring nothing this list shows.
        let unchanged = !rows.is_empty() && listing.rows == rows && listing.has_more == more;
        if !unchanged {
            loaded.select = self.removal.reselect(selected, &listing.rows, &rows);
            let listed: HashSet<_> = rows.iter().filter_map(Row::message).map(|m| m.id).collect();
            if !checked.is_subset(&listed) {
                loaded.checked = Some(checked.intersection(&listed).copied().collect());
            }
        }
        let shown = if unchanged { &listing.rows } else { &rows };
        if let Select::Open(_) = loaded.select {
            // The user's move wins over a notification clicked before.
            self.reveal.forget();
        } else if let Some(at) = self.reveal.read(read, shown) {
            loaded.select = Select::Open(shown[at].id());
        }
        if !unchanged {
            loaded.rows = Some(Rows::Replace(rows, more));
        }
        Some(loaded)
    }

    /// The user opened a row (or the list moved on to it): a message a notification asked
    /// for before is no longer waited for.
    pub fn opened(&mut self) {
        self.reveal.forget();
    }

    /// Selects message `id` of the source shown: now if `listing` has it, or else once
    /// the rows read from now on have it (it may have just arrived). Returns the row to
    /// open now, and whether to read the rows again for it. Nothing happens if the next
    /// read doesn't have it, if another source is shown first, or if the user opens a row
    /// meanwhile.
    pub fn reveal(&mut self, id: MessageId, listing: &Listing) -> (Option<RowId>, bool) {
        // The next read starts after now, once one running is done.
        self.reveal.ask(id, self.reads + 1);
        let open = self
            .reveal
            .read(self.reads, &listing.rows)
            .map(|at| listing.rows[at].id());
        (open, self.reveal.is_waiting())
    }

    /// `ids` are about to be moved or deleted: if `selected` is among them, the next row
    /// is selected once it goes away. Only the selected message's removal is waited for:
    /// another's (a row's own command) must not take the place of one pending for it.
    /// Returns the removal's number, for [`removal_settled`](Self::removal_settled).
    pub fn expect_removal(&mut self, selected: Option<RowId>, ids: &[MessageId]) -> Option<u64> {
        let Some(RowId::Message(id)) = selected else {
            return None;
        };
        ids.contains(&id).then(|| self.removal.expect(id))
    }

    /// The change behind `removal`, which may take the message out of the list, was made
    /// locally (`made`), or failed: whether to read the rows again, which say whether it
    /// left ([`Removal::settled`]).
    pub fn removal_settled(&mut self, removal: u64, made: bool) -> bool {
        // A read running may have started before the change: the next one shows it.
        self.removal.settled(removal, made, self.reads + 1)
    }
}

#[cfg(test)]
mod tests {
    use catmail_core::FolderId;

    use super::*;
    use crate::fixtures::{message, midday};

    fn rows(ids: &[i64]) -> Vec<Row> {
        ids.iter()
            .map(|&id| Row::Message(message(id, 1, 1)))
            .collect()
    }

    fn m(id: i64) -> RowId {
        RowId::Message(MessageId(id))
    }

    const FOLDER: Source = Source::Folder(FolderId(1));

    /// The model and the listing as a frontend keeps them, and what is selected and
    /// checked.
    struct List {
        model: ListModel,
        listing: Listing,
        selected: Option<RowId>,
        checked: HashSet<MessageId>,
    }

    impl List {
        /// `FOLDER` shown, with its first read on its way.
        fn new() -> (Self, Fetch) {
            let mut model = ListModel::default();
            let (listing, fetch) = model.show(Some(FOLDER));
            let list = Self {
                model,
                listing,
                selected: None,
                checked: HashSet::new(),
            };
            (list, fetch.unwrap())
        }

        /// `fetch` came back with `ids`; applies what it changes, as a frontend does.
        fn load(&mut self, fetch: &Fetch, ids: &[i64]) -> Option<Loaded> {
            let loaded = self.model.loaded(
                fetch,
                Some((rows(ids), false)),
                &self.listing,
                self.selected,
                &self.checked,
            )?;
            if let Some(rows) = loaded.rows.clone() {
                rows.apply(&mut self.listing);
            }
            match loaded.select {
                Select::Keep => {}
                Select::Open(row) => self.open(row),
                Select::Clear => self.selected = None,
            }
            if let Some(checked) = &loaded.checked {
                self.checked = checked.clone();
            }
            Some(loaded)
        }

        fn open(&mut self, row: RowId) {
            self.model.opened();
            self.selected = Some(row);
        }

        fn ids(&self) -> Vec<RowId> {
            self.listing.rows.iter().map(Row::id).collect()
        }
    }

    #[test]
    fn a_read_asked_for_meanwhile_follows_and_another_sources_are_dropped() {
        let (mut list, first) = List::new();
        // Changes while the first page is on its way: one more read after it.
        assert!(list.model.refresh(&list.listing).is_none());
        assert!(list.model.refresh(&list.listing).is_none());
        assert!(list.model.is_reading());
        let loaded = list.load(&first, &[1, 2]).unwrap();
        assert!(loaded.again);
        assert!(!list.model.is_reading());
        let again = list.model.refresh(&list.listing).unwrap();
        assert!(!list.load(&again, &[1, 2]).unwrap().again);

        // Another source shown: what was read for this one is dropped.
        let stale = list.model.refresh(&list.listing).unwrap();
        let (listing, next) = list.model.show(Some(Source::Inboxes));
        list.listing = listing;
        assert_eq!(list.load(&stale, &[1, 2]), None);
        assert!(
            list.model.is_reading(),
            "the new source's read is still running"
        );
        list.load(&next.unwrap(), &[3]);
        assert_eq!(list.ids(), [m(3)]);
        // Nothing shown: nothing read.
        let (nothing, fetch) = list.model.show(None);
        assert!(fetch.is_none());
        assert!(list.model.refresh(&nothing).is_none());
    }

    #[test]
    fn pages_follow_the_rows_shown_and_the_same_rows_are_left_as_they_are() {
        let (mut list, first) = List::new();
        let loaded = list
            .model
            .loaded(
                &first,
                Some((rows(&[1, 2]), true)),
                &list.listing,
                None,
                &HashSet::new(),
            )
            .unwrap();
        loaded.rows.unwrap().apply(&mut list.listing);
        let page = list.model.more(&list.listing).unwrap();
        assert_eq!(page.after, Some(message(2, 1, 1).cursor()));
        assert!(
            list.model.more(&list.listing).is_none(),
            "one read at a time"
        );
        // A refresh while the page is read waits for it.
        assert!(list.model.refresh(&list.listing).is_none());
        let loaded = list.load(&page, &[3]).unwrap();
        assert!(matches!(loaded.rows, Some(Rows::Append(..))) && loaded.again);
        assert_eq!(list.ids(), [m(1), m(2), m(3)]);
        assert!(
            list.model.more(&list.listing).is_none(),
            "no more after that"
        );

        // Read again, the same: nothing to change. At least a page, or what is shown.
        let again = list.model.refresh(&list.listing).unwrap();
        assert_eq!(again.limit, PAGE);
        assert_eq!(list.load(&again, &[1, 2, 3]).unwrap().rows, None);
    }

    #[test]
    fn a_moved_message_opens_the_next_once_it_is_gone() {
        let (mut list, first) = List::new();
        list.load(&first, &[1, 2, 3]);
        list.open(m(2));
        list.checked = HashSet::from([MessageId(2), MessageId(3)]);
        let removal = list
            .model
            .expect_removal(list.selected, &[MessageId(2)])
            .unwrap();
        // A read started before the change still has it: it stays selected. The read
        // the change asks for follows it.
        let before = list.model.refresh(&list.listing).unwrap();
        assert!(list.model.removal_settled(removal, true));
        assert!(list.model.refresh(&list.listing).is_none());
        let loaded = list.load(&before, &[1, 2, 3]).unwrap();
        assert!(loaded.again);
        assert_eq!(list.selected, Some(m(2)));
        // The read after the change: on to the next, and it is no longer checked.
        let after = list.model.refresh(&list.listing).unwrap();
        let loaded = list.load(&after, &[1, 3]).unwrap();
        assert_eq!(loaded.select, Select::Open(m(3)));
        assert_eq!(list.checked, HashSet::from([MessageId(3)]));

        // Gone by itself: nothing is selected.
        let again = list.model.refresh(&list.listing).unwrap();
        assert_eq!(list.load(&again, &[1]).unwrap().select, Select::Clear);
        assert_eq!(list.selected, None);
    }

    #[test]
    fn a_message_a_notification_asked_for_opens_once_listed() {
        let (mut list, first) = List::new();
        list.load(&first, &[1, 2]);
        // Listed: at once.
        assert_eq!(
            list.model.reveal(MessageId(2), &list.listing),
            (Some(m(2)), false)
        );
        // Not yet: the next read has it.
        assert_eq!(list.model.reveal(MessageId(5), &list.listing), (None, true));
        let next = list.model.refresh(&list.listing).unwrap();
        assert_eq!(
            list.load(&next, &[5, 1, 2]).unwrap().select,
            Select::Open(m(5))
        );

        // The read has it, but the rows are as shown: still opens.
        list.model.reveal(MessageId(6), &list.listing);
        let next = list.model.refresh(&list.listing).unwrap();
        list.listing.replace(rows(&[6, 5, 1, 2]), false);
        let loaded = list.load(&next, &[6, 5, 1, 2]).unwrap();
        assert_eq!((loaded.rows, loaded.select), (None, Select::Open(m(6))));

        // The user opens another row first: it isn't waited for.
        list.model.reveal(MessageId(7), &list.listing);
        list.open(m(1));
        let next = list.model.refresh(&list.listing).unwrap();
        assert_eq!(
            list.load(&next, &[7, 6, 5, 1, 2]).unwrap().select,
            Select::Keep
        );
        // Nor once a read that should have had it didn't.
        list.model.reveal(MessageId(8), &list.listing);
        let next = list.model.refresh(&list.listing).unwrap();
        list.load(&next, &[7, 6, 5, 1, 2]);
        let next = list.model.refresh(&list.listing).unwrap();
        let loaded = list.load(&next, &[8, 7, 6, 5, 1, 2]).unwrap();
        assert_eq!(loaded.select, Select::Keep);
        assert_eq!(list.selected, Some(m(1)));
    }

    #[test]
    fn moving_on_from_a_moved_message_wins_over_a_notification() {
        let (mut list, first) = List::new();
        list.load(&first, &[1, 2, 3]);
        list.open(m(2));
        let removal = list
            .model
            .expect_removal(list.selected, &[MessageId(2)])
            .unwrap();
        assert!(list.model.removal_settled(removal, true));
        list.model.reveal(MessageId(9), &list.listing);
        let next = list.model.refresh(&list.listing).unwrap();
        assert_eq!(
            list.load(&next, &[9, 1, 3]).unwrap().select,
            Select::Open(m(3))
        );
        let next = list.model.refresh(&list.listing).unwrap();
        assert_eq!(list.load(&next, &[9, 1, 3]).unwrap().select, Select::Keep);
    }

    #[test]
    fn a_key_acts_on_the_messages_checked_or_else_the_one_selected() {
        let (mut list, first) = List::new();
        list.load(&first, &[1, 2, 3]);
        let keyed = |list: &List| -> Vec<i64> {
            let keyed = list.listing.keyed(&list.checked, list.selected);
            keyed.iter().map(|m| m.id.0).collect()
        };
        assert!(keyed(&list).is_empty());
        list.open(m(2));
        assert_eq!(keyed(&list), [2]);
        // Top to bottom, whatever order they were checked in.
        list.checked = HashSet::from([MessageId(3), MessageId(1)]);
        assert_eq!(keyed(&list), [1, 3]);
        // A draft selected isn't a message.
        list.checked.clear();
        list.selected = Some(RowId::Draft(catmail_core::DraftId(2)));
        assert!(keyed(&list).is_empty());
    }

    #[test]
    fn only_the_selected_messages_removal_is_waited_for() {
        let (mut list, first) = List::new();
        list.load(&first, &[1, 2, 3]);
        assert_eq!(list.model.expect_removal(None, &[MessageId(1)]), None);
        list.open(m(2));
        // A row's own command, on another message.
        assert_eq!(
            list.model.expect_removal(list.selected, &[MessageId(1)]),
            None
        );
        let ids = [MessageId(1), MessageId(2)];
        assert!(list.model.expect_removal(list.selected, &ids).is_some());
    }

    #[test]
    fn the_servers_results_are_read_with_the_search_they_were_for() {
        let mut model = ListModel::default();
        let (listing, fetch) = model.show(Some(Source::Search("lunch".into())));
        let fetch = fetch.unwrap();
        let generation = model.generation();
        assert!(!model.found_on_server(generation, Vec::new()));
        assert!(model.found_on_server(generation, vec![MessageId(4)]));
        model.loaded(&fetch, None, &listing, None, &HashSet::new());
        let again = model.refresh(&listing).unwrap();
        assert_eq!(&*again.found_on_server, [MessageId(4)]);
        model.show(Some(FOLDER));
        assert!(!model.found_on_server(generation, vec![MessageId(4)]));
    }

    #[test]
    fn a_page_more_adds_lines_to_the_last_day_and_after_it() {
        let dated = |id, day: &str| {
            let mut message = message(id, 1, 1);
            message.date = Some(midday(day));
            Row::Message(message)
        };
        let mut listing = Listing::new(Some(FOLDER));
        listing.replace(vec![dated(1, "2020-03-02")], true);
        listing.append(
            vec![
                dated(2, "2020-03-02"),
                dated(3, "2020-03-01"),
                dated(4, "2020-03-01"),
            ],
            false,
        );
        let lines: Vec<_> = (0..listing.len())
            .map(|ix| listing.line(ix).unwrap())
            .collect();
        let row = |id| Line::Row(dated(id, if id < 3 { "2020-03-02" } else { "2020-03-01" }));
        assert_eq!(
            lines,
            [
                Line::Day("March 2, 2020".into()),
                row(1),
                row(2),
                Line::Day("March 1, 2020".into()),
                row(3),
                row(4),
            ]
        );
        assert_eq!(listing.keys()[2..4], [LineKey::Row(m(2)), LineKey::Day(3)]);
        assert_eq!(listing.line_of(m(3)), Some(4));
        assert_eq!(listing.line_of(m(9)), None);
    }
}
