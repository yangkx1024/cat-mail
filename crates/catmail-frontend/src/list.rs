//! The middle pane's rows apart from how they look: what a source lists and how it is
//! read a page at a time, the rows grouped by day, where the selection goes when they
//! are read again, which messages a check menu takes, and what a row says. The list's
//! reads and the lines it shows are [`ListModel`]'s and [`Listing`]'s.

mod model;

use std::collections::HashMap;
use std::hash::Hash;
use std::rc::Rc;
use std::time::Duration;

use catmail_core::{
    Draft, DraftId, Flags, FolderId, MailClient, MessageCursor, MessageId, MessageSummary,
    OutboxEntry, OutboxId, OutboxStatus, SearchQuery,
};
use jiff::civil::Date;

use crate::format::{self, Calendar};
use crate::mailbox::Source;

pub use model::{
    Fetch, Line, LineKey, ListModel, Listing, Loaded, Rows, Select, ServerSearch, Typed,
};

/// Rows read at a time.
pub const PAGE: u32 = 200;

/// How long typing in the search field pauses before it searches.
pub const SEARCH_DELAY: Duration = Duration::from_millis(200);

/// How long search results show before the servers are asked about the text too. A
/// search the user moves on from still has the server finish the command at hand (a scan
/// of a whole folder's text, on servers without an index), which the next one waits for:
/// this keeps the pauses in typing from asking, at the cost of server results showing a
/// little later than they could.
pub const SERVER_SEARCH_DELAY: Duration = Duration::from_secs(1);

/// The most labels a row shows by name; the rest are counted.
pub const ROW_LABELS: usize = 2;

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Message(MessageSummary),
    /// Shared: the right pane shows it, and a draft can hold a whole forwarded message.
    Draft(Rc<Draft>),
    Outbox(OutboxEntry),
}

/// A row's identity, which survives reloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowId {
    Message(MessageId),
    Draft(DraftId),
    Outbox(OutboxId),
}

impl Row {
    pub fn id(&self) -> RowId {
        match self {
            Row::Message(m) => RowId::Message(m.id),
            Row::Draft(d) => RowId::Draft(d.id),
            Row::Outbox(e) => RowId::Outbox(e.id),
        }
    }

    pub fn message(&self) -> Option<&MessageSummary> {
        match self {
            Row::Message(m) => Some(m),
            _ => None,
        }
    }

    /// When it happened, as far as the list goes: a message's date, when a draft was last
    /// changed, when an entry was put in the outbox.
    pub fn date(&self) -> Option<i64> {
        match self {
            Row::Message(m) => m.date,
            Row::Draft(d) => Some(d.updated_at),
            Row::Outbox(e) => Some(e.created_at),
        }
    }
}

/// Reads `limit` rows of `source`, after `after`; and whether there may be more.
/// `found_on_server` are the search results the servers found that local search can't
/// (see `MailClient::search_server`).
pub(crate) async fn fetch(
    client: &MailClient,
    source: &Source,
    found_on_server: &[MessageId],
    limit: u32,
    after: Option<MessageCursor>,
) -> catmail_core::Result<(Vec<Row>, bool)> {
    // A full page may have more after it.
    let page = |messages: Vec<MessageSummary>| {
        let more = messages.len() == limit as usize;
        (messages.into_iter().map(Row::Message).collect(), more)
    };
    Ok(match source {
        Source::Inboxes => page(client.list_inboxes(limit, after).await?),
        Source::Folder(folder) => page(client.list_messages(*folder, limit, after).await?),
        Source::Search(text) => {
            let mut query = SearchQuery::new(text.clone());
            query.limit = limit;
            query.after = after;
            query.found_on_server = found_on_server.to_vec();
            page(client.search(query).await?)
        }
        Source::Drafts(account) => {
            let drafts = client.list_drafts(*account).await?;
            let rows = drafts.into_iter().map(|d| Row::Draft(Rc::new(d))).collect();
            (rows, false)
        }
        Source::Outbox(account) => {
            let entries = client.list_outbox(Some(*account)).await?;
            // Newest first, like everything else in the list.
            (entries.into_iter().rev().map(Row::Outbox).collect(), false)
        }
    })
}

/// A run of rows from the same day, shown under its heading.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Day {
    day: Option<Date>,
    label: String,
    /// Its first row, counted top to bottom.
    start: usize,
    len: usize,
}

/// The rows grouped by day: the list's sections.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Days(Vec<Day>);

impl Days {
    fn new(rows: &[Row], calendar: &Calendar) -> Self {
        let mut days = Self::default();
        days.extend(rows, 0, calendar);
        days
    }

    /// Groups `rows[from..]`, the rows after those already grouped (by the same
    /// calendar). The rows are newest first, so a day is one run; if they weren't, a day
    /// would just get a heading more than once.
    fn extend(&mut self, rows: &[Row], from: usize, calendar: &Calendar) {
        for (ix, row) in rows.iter().enumerate().skip(from) {
            let day = calendar.day(row.date());
            match self.0.last_mut() {
                Some(last) if last.day == day => last.len += 1,
                _ => self.0.push(Day {
                    day,
                    label: calendar.heading(day),
                    start: ix,
                    len: 1,
                }),
            }
        }
    }

    fn get(&self, ix: usize) -> Option<&Day> {
        self.0.get(ix)
    }

    fn iter(&self) -> impl Iterator<Item = &Day> {
        self.0.iter()
    }
}

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
pub(crate) struct Removal {
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
pub(crate) struct Reveal(Option<(MessageId, u64)>);

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

/// Which of the messages listed to check, from the menu beside the list's checkbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    All,
    Read,
    Unread,
    Flagged,
    Unflagged,
}

impl Check {
    pub const MENU: [Check; 5] = [
        Check::All,
        Check::Read,
        Check::Unread,
        Check::Flagged,
        Check::Unflagged,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Check::All => "All",
            Check::Read => "Read",
            Check::Unread => "Unread",
            Check::Flagged => "Flagged",
            Check::Unflagged => "Unflagged",
        }
    }

    pub fn takes(self, flags: Flags) -> bool {
        match self {
            Check::All => true,
            Check::Read => flags.contains(Flags::SEEN),
            Check::Unread => !flags.contains(Flags::SEEN),
            Check::Flagged => flags.contains(Flags::FLAGGED),
            Check::Unflagged => !flags.contains(Flags::FLAGGED),
        }
    }
}

/// The names of the message's user labels but `except` (the label listed: every row has
/// it), sorted; `labels` names those shown (see `AccountFolders::user_labels`).
pub fn label_names<S: Clone + Ord>(
    labels: &HashMap<FolderId, S>,
    message: &MessageSummary,
    except: Option<FolderId>,
) -> Vec<S> {
    let mut names: Vec<_> = message
        .labels
        .iter()
        .filter(|&&id| Some(id) != except)
        .filter_map(|id| labels.get(id).cloned())
        .collect();
    names.sort();
    names
}

/// The addresses of whom `message` went to: To, or Cc when it has no To.
pub fn recipients(message: &MessageSummary) -> String {
    let to = if message.to.is_empty() {
        &message.cc
    } else {
        &message.to
    };
    to.iter()
        .map(|a| a.email.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whom a draft row says it goes to.
pub fn draft_recipients(draft: &Draft) -> String {
    let to = format::names(draft.content.recipients());
    if to.is_empty() {
        "No recipients".to_owned()
    } else {
        format!("To {to}")
    }
}

/// What a screen reader says of a message row.
pub fn message_label(
    message: &MessageSummary,
    labels: &[impl AsRef<str>],
    clock: &format::Clock,
) -> String {
    let unread = !message.flags.contains(Flags::SEEN);
    let flagged = message.flags.contains(Flags::FLAGGED);
    format!(
        "{}, {}, {}{}{}{}{}",
        format::sender(&message.from),
        format::subject(message.subject.as_deref()),
        format::time(message.date, clock),
        if unread { ", unread" } else { "" },
        if flagged { ", flagged" } else { "" },
        if message.has_attachments {
            ", has attachments"
        } else {
            ""
        },
        if labels.is_empty() {
            String::new()
        } else {
            let names: Vec<_> = labels.iter().map(AsRef::as_ref).collect();
            format!(", labels {}", names.join(", "))
        },
    )
}

/// How loud an outbox entry's state is, if it needs the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Danger,
}

/// How an outbox entry's state reads, and how loud it is if it needs the user.
pub fn outbox_status(entry: &OutboxEntry) -> (&'static str, Option<Severity>) {
    let text = match entry.status {
        OutboxStatus::Queued if entry.attempts > 0 => "Waiting to retry",
        OutboxStatus::Queued => "Waiting to send",
        OutboxStatus::Sending => "Sending…",
        OutboxStatus::Accepted => "Sent · saving to Sent",
        OutboxStatus::Unknown => "Not confirmed",
        OutboxStatus::Failed => "Rejected",
        OutboxStatus::Done => "Sent",
    };
    let severity = entry.status.needs_user().then_some(match entry.status {
        OutboxStatus::Failed => Severity::Danger,
        _ => Severity::Warning,
    });
    (text, severity)
}

/// What the list says when `source` lists nothing.
pub fn empty_text(source: Option<&Source>) -> &'static str {
    match source {
        None => "No folder selected",
        Some(Source::Inboxes | Source::Folder(_)) => "No messages",
        Some(Source::Search(_)) => "Nothing found",
        Some(Source::Drafts(_)) => "No drafts",
        Some(Source::Outbox(_)) => "Nothing waiting to be sent",
    }
}

/// Which of six hues a sender's avatar takes, picked by the address so a sender keeps
/// theirs.
pub fn avatar_hue(seed: &str) -> usize {
    (crate::fnv1a(seed.to_lowercase().bytes()) % 6) as usize
}

/// The letter an avatar shows: the first of the name, upper case.
pub fn initial(name: &str) -> String {
    name.chars().take(1).collect::<String>().to_uppercase()
}

#[cfg(test)]
mod tests {
    use catmail_core::Address;

    use super::*;
    use crate::fixtures;

    fn summary(id: i64, date: Option<i64>) -> MessageSummary {
        MessageSummary {
            date,
            ..fixtures::message(id, 1, 1)
        }
    }

    fn message(id: i64, day: Option<&str>) -> Row {
        Row::Message(summary(id, day.map(fixtures::midday)))
    }

    #[test]
    fn the_selection_follows_the_row_or_moves_on() {
        use Reselect::*;
        let m = |id| RowId::Message(MessageId(id));
        let rows = |ids: &[i64]| -> Vec<Row> { ids.iter().map(|&id| message(id, None)).collect() };
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
        let rows = |ids: &[i64]| -> Vec<Row> { ids.iter().map(|&id| message(id, None)).collect() };
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
        let rows = |ids: &[i64]| -> Vec<Row> { ids.iter().map(|&id| message(id, None)).collect() };
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

    #[test]
    fn rows_are_grouped_by_day() {
        let rows = vec![
            message(1, Some("2020-03-02")),
            message(2, Some("2020-03-02")),
            message(3, Some("2020-03-01")),
            message(4, None),
            message(5, None),
        ];
        let days = Days::new(&rows, &Calendar::now(&jiff::tz::TimeZone::UTC));
        let shape: Vec<_> = days
            .iter()
            .map(|s| (s.label.clone(), s.start, s.len))
            .collect();
        assert_eq!(
            shape,
            [
                ("March 2, 2020".to_owned(), 0, 2),
                ("March 1, 2020".to_owned(), 2, 1),
                ("No date".to_owned(), 3, 2),
            ]
        );
        let calendar = Calendar::now(&jiff::tz::TimeZone::UTC);
        assert!(Days::new(&[], &calendar).get(0).is_none());
    }

    #[test]
    fn recipients_are_to_or_else_cc() {
        let address = |email: &str| Address {
            name: Some("Name".into()),
            email: email.into(),
        };
        let mut message = summary(1, None);
        message.to = vec![address("a@example.com"), address("b@example.com")];
        message.cc = vec![address("c@example.com")];
        assert_eq!(recipients(&message), "a@example.com, b@example.com");
        message.to.clear();
        assert_eq!(recipients(&message), "c@example.com");
    }

    #[test]
    fn label_names_leave_out_the_label_listed() {
        let labels = HashMap::from([
            (FolderId(1), "Work"),
            (FolderId(2), "Clients"),
            (FolderId(3), "Hidden"),
        ]);
        let mut message = summary(1, None);
        message.labels = vec![FolderId(1), FolderId(2), FolderId(9)];
        assert_eq!(label_names(&labels, &message, None), ["Clients", "Work"]);
        assert_eq!(label_names(&labels, &message, Some(FolderId(2))), ["Work"]);
    }

    #[test]
    fn avatars_keep_their_hue_whatever_the_case() {
        assert_eq!(avatar_hue("A@Example.com"), avatar_hue("a@example.com"));
        assert!(avatar_hue("someone@example.com") < 6);
        assert_eq!(initial("émile"), "É");
        assert_eq!(initial(""), "");
    }
}
