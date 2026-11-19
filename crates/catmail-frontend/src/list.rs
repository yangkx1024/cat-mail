//! The middle pane's rows apart from how they look: what a source lists and how it is
//! read a page at a time, the rows grouped by day, where the selection goes when they
//! are read again, which messages a check menu takes, and what a row says. The list's
//! reads and the lines it shows are [`ListModel`]'s and [`Listing`]'s, where the
//! selection goes `selection`'s.

mod model;
mod selection;

use std::collections::HashMap;
use std::hash::Hash;
use std::rc::Rc;
use std::time::Duration;

use catmail_core::{
    DraftId, DraftSummary, FlagFilter, Flags, FolderId, ListFilter, ListQuery, MailClient,
    MessageCursor, MessageId, MessageSummary, OutboxEntry, OutboxId, OutboxStatus, SearchQuery,
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
    /// Shared: the right pane shows it, and its text can be a whole forwarded message.
    Draft(Rc<DraftSummary>),
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

/// Reads `limit` rows of `source` that `filter` takes, after `after`; and whether there
/// may be more. `found_on_server` are the search results the servers found that local
/// search can't (see `MailClient::search_server`). The drafts and the outbox aren't
/// filtered.
pub(crate) async fn fetch(
    client: &MailClient,
    source: &Source,
    filter: ListFilter,
    found_on_server: &[MessageId],
    limit: u32,
    after: Option<MessageCursor>,
) -> catmail_core::Result<(Vec<Row>, bool)> {
    // A full page may have more after it.
    let page = |messages: Vec<MessageSummary>| {
        let more = messages.len() == limit as usize;
        (messages.into_iter().map(Row::Message).collect(), more)
    };
    let query = ListQuery {
        limit,
        after,
        filter,
    };
    Ok(match source {
        Source::Inboxes => page(client.list_inboxes(None, query).await?),
        Source::Folder(folder) => page(client.list_messages(*folder, query).await?),
        Source::Search(text) => {
            let mut search = SearchQuery::new(text.clone());
            search.limit = limit;
            search.after = after;
            search.found_on_server = found_on_server.to_vec();
            search.filter = query.filter;
            page(client.search(search).await?)
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

/// The views of a list of messages the menu beside its checkbox shows, in its order.
pub const FILTERS: [FlagFilter; 5] = [
    FlagFilter::All,
    FlagFilter::Read,
    FlagFilter::Unread,
    FlagFilter::Flagged,
    FlagFilter::Unflagged,
];

/// The name of a view of the list, as its menu shows it.
pub fn filter_label(filter: FlagFilter) -> &'static str {
    match filter {
        FlagFilter::All => "All",
        FlagFilter::Read => "Read",
        FlagFilter::Unread => "Unread",
        FlagFilter::Flagged => "Flagged",
        FlagFilter::Unflagged => "Unflagged",
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
pub fn draft_recipients(draft: &DraftSummary) -> String {
    let to = format::names(draft.recipients());
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
        OutboxStatus::Unsaved => "Sent · not saved to Sent",
        OutboxStatus::Unknown => "Not confirmed",
        OutboxStatus::Failed => "Not sent",
        OutboxStatus::Done => "Sent",
    };
    let severity = entry.status.needs_user().then_some(match entry.status {
        OutboxStatus::Failed => Severity::Danger,
        _ => Severity::Warning,
    });
    (text, severity)
}

/// What the list says when `source` lists nothing.
pub fn empty_text(source: Option<&Source>, filter: FlagFilter) -> &'static str {
    match source {
        None => "No folder selected",
        Some(Source::Inboxes | Source::Folder(_)) => match filter {
            FlagFilter::All => "No messages",
            FlagFilter::Read => "No read messages",
            FlagFilter::Unread => "No unread messages",
            FlagFilter::Flagged => "No flagged messages",
            FlagFilter::Unflagged => "No unflagged messages",
        },
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
