//! The middle pane: a folder's messages (loaded a page at a time as the list scrolls),
//! search results, the drafts kept here, or what is waiting in the outbox, newest first
//! under a heading for each day. The list (`ListState`) owns selection, keyboard
//! navigation and scrolling; the delegate here owns the rows.

use std::rc::Rc;

use catmail_core::{
    AccountId, Draft, DraftId, FolderId, MailClient, MessageId, MessageSummary, OutboxEntry,
    OutboxId, SearchQuery,
};
use gpui_kit::component::list::{ListDelegate, ListEvent, ListItem, ListState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _};
use gpui_kit::{
    App, Context, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, Task,
    Window, div, prelude::FluentBuilder as _,
};
use jiff::civil::Date;

use crate::format::Calendar;
use crate::theme::colors::ThemeColors as _;

mod rows;

pub use rows::outbox_status;
use rows::{draft_row, message_row, outbox_row};

const PAGE: u32 = 200;

/// What the list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Folder(FolderId),
    Search(String),
    Drafts(AccountId),
    Outbox(AccountId),
}

#[derive(Debug, Clone)]
pub enum Row {
    Message(MessageSummary),
    /// Shared: the right pane shows it, and a draft can hold a whole forwarded message.
    Draft(Rc<Draft>),
    Outbox(OutboxEntry),
}

/// A row's identity, which survives reloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    fn date(&self) -> Option<i64> {
        match self {
            Row::Message(m) => m.date,
            Row::Draft(d) => Some(d.updated_at),
            Row::Outbox(e) => Some(e.created_at),
        }
    }
}

/// A run of rows from the same day, shown under its heading.
#[derive(Debug, Clone)]
struct Day {
    day: Option<Date>,
    label: SharedString,
    start: usize,
    len: usize,
}

/// The rows grouped by day: the list's sections. Rows are counted top to bottom
/// (`flat`), the list counts within a section (`IndexPath`).
#[derive(Debug, Default)]
struct Days(Vec<Day>);

impl Days {
    fn new(rows: &[Row]) -> Self {
        let mut days = Self::default();
        days.extend(rows, 0);
        days
    }

    /// Groups `rows[from..]`, the rows after those already grouped. The rows are newest
    /// first, so a day is one run; if they weren't, a day would just get a heading more
    /// than once.
    fn extend(&mut self, rows: &[Row], from: usize) {
        let calendar = Calendar::now();
        for (ix, row) in rows.iter().enumerate().skip(from) {
            let day = calendar.day(row.date());
            match self.0.last_mut() {
                Some(last) if last.day == day => last.len += 1,
                _ => self.0.push(Day {
                    day,
                    label: calendar.heading(day).into(),
                    start: ix,
                    len: 1,
                }),
            }
        }
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn get(&self, section: usize) -> Option<&Day> {
        self.0.get(section)
    }

    fn flat(&self, ix: IndexPath) -> Option<usize> {
        let day = self.0.get(ix.section)?;
        (ix.row < day.len).then_some(day.start + ix.row)
    }

    fn path(&self, flat: usize) -> Option<IndexPath> {
        let section = self.0.partition_point(|d| d.start <= flat).checked_sub(1)?;
        let row = flat - self.0[section].start;
        (row < self.0[section].len).then(|| IndexPath::new(row).section(section))
    }
}

pub struct MessageList {
    client: MailClient,
    source: Option<Source>,
    /// Indexed as shown, top to bottom; `selected` too. The list itself counts by
    /// section (`IndexPath`).
    rows: Vec<Row>,
    /// Regrouped whenever `rows` changes.
    days: Days,
    selected: Option<usize>,
    /// A page is being fetched; `load_more` and `refresh` wait for it.
    loading: bool,
    /// The engine reported a change while a page was being fetched: refresh after it.
    stale: bool,
    has_more: bool,
    /// Bumped when the source changes, so rows fetched for the previous one are dropped.
    generation: u64,
    /// The message the user moved or deleted: once it is gone while still selected, the
    /// row now at its index is selected, so the next message opens.
    removing: Option<MessageId>,
    _load: Task<()>,
}

impl MessageList {
    pub fn new(client: MailClient) -> Self {
        Self {
            client,
            source: None,
            rows: Vec::new(),
            days: Days::default(),
            selected: None,
            loading: false,
            stale: false,
            has_more: false,
            generation: 0,
            removing: None,
            _load: Task::ready(()),
        }
    }

    pub fn row_at(&self, ix: IndexPath) -> Option<&Row> {
        self.rows.get(self.days.flat(ix)?)
    }

    pub fn source(&self) -> Option<&Source> {
        self.source.as_ref()
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected?)
    }

    pub fn selected_message(&self) -> Option<&MessageSummary> {
        self.selected_row()?.message()
    }

    #[cfg(test)]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Selects the next row once `id`, the selected message, goes away.
    pub fn expect_removal(&mut self, id: MessageId) {
        self.removing = Some(id);
    }
}

/// Shows `source` from the top, or nothing.
pub fn show(
    list: &mut ListState<MessageList>,
    source: Option<Source>,
    window: &mut Window,
    cx: &mut Context<ListState<MessageList>>,
) {
    let delegate = list.delegate_mut();
    delegate.source = source;
    delegate.rows.clear();
    delegate.days = Days::default();
    delegate.has_more = false;
    delegate.stale = false;
    delegate.removing = None;
    delegate.generation += 1;
    list.set_selected_index(None, window, cx);
    list.scroll_to_item(IndexPath::new(0), gpui_kit::ScrollStrategy::Top, window, cx);
    load(list, Load::Replace(PAGE), window, cx);
}

/// Re-reads the rows already shown (after the engine reported a change), keeping the
/// selected row selected. During a sync changes arrive faster than a page loads, so
/// they are coalesced: at most one load runs, and one more follows if needed.
pub fn refresh(
    list: &mut ListState<MessageList>,
    window: &mut Window,
    cx: &mut Context<ListState<MessageList>>,
) {
    let delegate = list.delegate_mut();
    if delegate.loading {
        delegate.stale = true;
        return;
    }
    let shown = delegate.rows.len() as u32;
    load(list, Load::Replace(shown.max(PAGE)), window, cx);
}

enum Load {
    /// The first `n` rows, replacing what is shown.
    Replace(u32),
    /// The next page after what is shown.
    Append,
}

async fn fetch(
    client: &MailClient,
    source: &Source,
    limit: u32,
    offset: u32,
) -> catmail_core::Result<(Vec<Row>, bool)> {
    Ok(match source {
        Source::Folder(folder) => {
            let page = client.list_messages(*folder, limit, offset).await?;
            let more = page.len() == limit as usize;
            (page.into_iter().map(Row::Message).collect(), more)
        }
        Source::Search(text) => {
            let mut query = SearchQuery::new(text.clone());
            query.limit = limit;
            query.offset = offset;
            let found = client.search(query).await?;
            let more = found.len() == limit as usize;
            (found.into_iter().map(Row::Message).collect(), more)
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

fn load(
    list: &mut ListState<MessageList>,
    what: Load,
    window: &mut Window,
    cx: &mut Context<ListState<MessageList>>,
) {
    let delegate = list.delegate_mut();
    let Some(source) = delegate.source.clone() else {
        cx.notify();
        return;
    };
    let (limit, offset) = match what {
        Load::Replace(n) => (n, 0),
        Load::Append => (PAGE, delegate.rows.len() as u32),
    };
    let client = delegate.client.clone();
    let generation = delegate.generation;
    delegate.loading = true;
    // Replacing the task drops (cancels) a load still in flight, which only `show` does:
    // the other callers wait for `loading`.
    delegate._load = cx.spawn_in(window, async move |list, cx| {
        let result = fetch(&client, &source, limit, offset).await;
        list.update_in(cx, |list, window, cx| {
            if list.delegate().generation != generation {
                return;
            }
            let delegate = list.delegate_mut();
            delegate.loading = false;
            match result {
                Ok((rows, more)) => {
                    delegate.has_more = more;
                    if offset > 0 {
                        let from = delegate.rows.len();
                        delegate.rows.extend(rows);
                        delegate.days.extend(&delegate.rows, from);
                    } else {
                        replace_rows(list, rows, window, cx);
                    }
                }
                Err(e) => window.push_notification(
                    Notification::error(format!("Couldn't load the list: {e}")),
                    cx,
                ),
            }
            if std::mem::take(&mut list.delegate_mut().stale) {
                refresh(list, window, cx);
            }
            cx.notify();
        })
        .ok();
    });
}

/// Puts the new rows in, keeping the selected row selected by its identity.
fn replace_rows(
    list: &mut ListState<MessageList>,
    rows: Vec<Row>,
    window: &mut Window,
    cx: &mut Context<ListState<MessageList>>,
) {
    let delegate = list.delegate_mut();
    let selected = delegate.selected.zip(delegate.selected_row().map(Row::id));
    let next = reselect(selected, delegate.removing.take(), &rows);
    delegate.days = Days::new(&rows);
    delegate.rows = rows;
    match next {
        Reselect::Keep { ix, removing } => {
            let ix = ix.and_then(|ix| list.delegate().days.path(ix));
            list.set_selected_index(ix, window, cx);
            list.delegate_mut().removing = removing;
        }
        Reselect::Next(ix) => {
            let next = list.delegate().days.path(ix);
            list.set_selected_index(next, window, cx);
            if let Some(next) = next {
                cx.emit(ListEvent::Select(next));
            }
        }
        Reselect::Gone => {
            // Setting the index emits nothing; deselecting with Escape emits this.
            list.set_selected_index(None, window, cx);
            cx.emit(ListEvent::Cancel);
        }
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
    /// The user moved or deleted the selected message: on to the row now at its index.
    Next(usize),
    /// The selected row is gone (expunged, sent, or a new UIDVALIDITY).
    Gone,
}

/// `selected` is the selected row's index and identity in the rows replaced, `removing`
/// the message the user moved or deleted (see [`MessageList::expect_removal`]).
fn reselect(
    selected: Option<(usize, RowId)>,
    removing: Option<MessageId>,
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
    match rows.iter().position(|r| r.id() == id) {
        Some(ix) => Reselect::Keep {
            ix: Some(ix),
            removing,
        },
        None if removing.is_some() && !rows.is_empty() => Reselect::Next(at.min(rows.len() - 1)),
        None => Reselect::Gone,
    }
}

impl ListDelegate for MessageList {
    type Item = ListItem;

    fn sections_count(&self, _: &App) -> usize {
        self.days.len()
    }

    fn items_count(&self, section: usize, _: &App) -> usize {
        self.days.get(section).map_or(0, |d| d.len)
    }

    fn render_section_header(
        &mut self,
        section: usize,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        let label = self.days.get(section)?.label.clone();
        // Lined up with the rows' text (their padding).
        Some(
            div()
                .px_2()
                .pt_3()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let ix = self.days.flat(ix)?;
        let row = self.rows.get(ix)?;
        let selected = self.selected == Some(ix);
        let item = match row {
            Row::Message(message) => message_row(message, cx),
            Row::Draft(draft) => draft_row(draft, cx),
            Row::Outbox(entry) => outbox_row(entry, cx),
        };
        // The virtual list has no gap between rows; the margin keeps a selected row's
        // background apart from a hovered neighbour's.
        Some(
            item.selected(selected)
                .rounded(cx.theme().radius_lg)
                .px_2()
                .my_0p5(),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let text = match &self.source {
            None => "No folder selected",
            Some(Source::Folder(_)) => "No messages",
            Some(Source::Search(_)) => "Nothing found",
            Some(Source::Drafts(_)) => "No drafts",
            Some(Source::Outbox(_)) => "Nothing waiting to be sent",
        };
        crate::layout::empty(text, cx)
    }

    fn loading(&self, _: &App) -> bool {
        // The skeleton only while nothing is shown yet; a refresh keeps the rows.
        self.loading && self.rows.is_empty()
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix.and_then(|ix| self.days.flat(ix));
    }

    fn has_more(&self, _: &App) -> bool {
        self.has_more
    }

    fn load_more(&mut self, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        if self.loading || !self.has_more {
            return;
        }
        // `load` needs the list, which is being updated right now.
        cx.defer_in(window, |list, window, cx| {
            if !list.delegate().loading {
                load(list, Load::Append, window, cx);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use catmail_core::{AccountId, Flags};

    use super::*;

    /// Midday UTC: the same calendar day in nearly every time zone the test runs in.
    fn message(id: i64, date: Option<&str>) -> Row {
        let date = date.map(|d| {
            format!("{d}T12:00:00Z")
                .parse::<jiff::Timestamp>()
                .unwrap()
                .as_second()
        });
        Row::Message(MessageSummary {
            id: MessageId(id),
            account_id: AccountId(1),
            folder_id: FolderId(1),
            thread_id: None,
            subject: None,
            from: Vec::new(),
            date,
            snippet: None,
            flags: Flags::empty(),
            has_attachments: false,
        })
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
            (None, removing, &[1, 2][..], Keep { ix: None, removing: None }),
            // Still there, wherever it is now.
            (Some((1, m(2))), None, &[0, 1, 2], Keep { ix: Some(2), removing: None }),
            // Moved but still listed: keep waiting.
            (Some((1, m(2))), removing, &[1, 2, 3], Keep { ix: Some(1), removing }),
            // Waiting for another message than the selected one: the user moved on.
            (Some((0, m(1))), removing, &[1, 3], Keep { ix: Some(0), removing: None }),
            // Moved away: the row now at its place, or the last one.
            (Some((1, m(2))), removing, &[1, 3, 4], Next(1)),
            (Some((2, m(2))), removing, &[1, 3], Next(1)),
            // Moved away and nothing left, or gone by itself.
            (Some((0, m(2))), removing, &[], Gone),
            (Some((1, m(2))), None, &[1, 3], Gone),
            (Some((0, RowId::Draft(DraftId(2)))), removing, &[1], Gone),
        ];
        for (selected, removing, ids, expected) in cases {
            assert_eq!(
                reselect(selected, removing, &rows(ids)),
                expected,
                "{selected:?} {removing:?} {ids:?}"
            );
        }
    }

    #[test]
    fn rows_are_grouped_by_day_and_indexed_both_ways() {
        let rows = vec![
            message(1, Some("2020-03-02")),
            message(2, Some("2020-03-02")),
            message(3, Some("2020-03-01")),
            message(4, None),
            message(5, None),
        ];
        let days = Days::new(&rows);
        let shape: Vec<_> = days
            .0
            .iter()
            .map(|s| (s.label.to_string(), s.start, s.len))
            .collect();
        assert_eq!(
            shape,
            [
                ("March 2, 2020".to_owned(), 0, 2),
                ("March 1, 2020".to_owned(), 2, 1),
                ("No date".to_owned(), 3, 2),
            ]
        );
        for flat in 0..5 {
            let path = days.path(flat).unwrap();
            assert_eq!(days.flat(path), Some(flat), "{path:?}");
        }
        assert_eq!(days.path(3), Some(IndexPath::new(0).section(2)));
        assert_eq!(days.path(5), None);
        assert_eq!(days.flat(IndexPath::new(1).section(1)), None);
        assert_eq!(days.flat(IndexPath::new(0).section(3)), None);
        assert!(Days::new(&[]).path(0).is_none());
    }
}
