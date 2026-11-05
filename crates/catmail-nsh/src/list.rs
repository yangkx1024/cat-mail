//! The middle pane, as the GUI's (`catmail-gui`'s `mailbox/message_list`): what the
//! selected place lists, or search results, newest first under a heading for each day;
//! a row on one line while nothing is open, the subject under the sender while the
//! reading pane is; the messages checked for a command on them all. What a source lists,
//! the days, where the selection goes and what a row says are `catmail_frontend::list`'s.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use catmail_core::{Flags, FolderId, MessageCursor, MessageId, MessageSummary, SearchQuery};
use catmail_frontend::commands::MessageCommand;
use catmail_frontend::format;
use catmail_frontend::list::{
    self, Check, Days, PAGE, ROW_LABELS, Removal, Reselect, Row, RowId, SEARCH_DELAY,
    SERVER_SEARCH_DELAY, Severity,
};
use catmail_frontend::mailbox::{self, Source};
use nsh_base::theme;
use nsh_core::Hsla;
use nsh_core::element::desktop::{MenuItem, popup_menu};
use nsh_core::element::{ElementRef, input};
use nsh_core::geometry::Percentage;
use nsh_core::prelude::*;

use crate::app::{client, in_mailbox, report};
use crate::commands::{self, Size};
use crate::icons::{Icon, icon};
use crate::sidebar::{Accounts, SelectedPlace};

/// What the list shows: the rows, grouped by day, and what a row of them shows besides.
#[derive(Clone, Default)]
pub struct Rows {
    pub source: Option<Source>,
    pub rows: Vec<Row>,
    days: Days,
    /// The headings and the rows, as the list lays them out.
    items: Rc<[Item]>,
    has_more: bool,
    /// A page is being read.
    loading: bool,
}

/// A line of the list: a day's heading or one of its rows.
#[derive(Debug, Clone, PartialEq)]
enum Item {
    Day(String),
    Row(Row),
}

impl Item {
    /// Its identity, across reads.
    fn key(&self, ix: usize) -> ItemKey {
        match self {
            // A day can head two runs, when the rows aren't by date.
            Item::Day(_) => ItemKey::Day(ix),
            Item::Row(row) => ItemKey::Row(row.id()),
        }
    }
}

#[derive(Hash)]
enum ItemKey {
    Day(usize),
    Row(RowId),
}

fn items(rows: &[Row], days: &Days) -> Rc<[Item]> {
    let mut items = Vec::with_capacity(rows.len() + days.len());
    for day in days.iter() {
        items.push(Item::Day(day.label.clone()));
        items.extend(
            rows[day.start..day.start + day.len]
                .iter()
                .cloned()
                .map(Item::Row),
        );
    }
    items.into()
}

/// The row the reading pane shows. Its index changes as rows come and go; it doesn't.
#[derive(Clone, Default)]
pub struct Selected(pub Option<RowId>);

/// The messages checked, for a command on them all: apart from the selected row, the one
/// the reader shows. Always among the rows.
#[derive(Clone, Default)]
pub struct Checked(pub HashSet<MessageId>);

/// The message row the pointer is on.
#[derive(Clone, Default)]
struct Hovered(Option<MessageId>);

/// What the search field holds.
#[derive(Clone, Default)]
struct SearchText(String);

/// The name of each user label shown, of every account: a row shows those of its
/// message's labels.
#[derive(Clone, Default)]
struct LabelNames(HashMap<FolderId, String>);

/// The reads of the list: which source they are for, and what waits on them. Not
/// reactive.
#[derive(Default)]
struct Loads {
    /// Bumped when the source changes, so rows read for the one before are dropped.
    generation: u64,
    /// The engine reported a change while a page was being read: refresh after it.
    stale: bool,
    /// Search results the servers found that local search can't, by their text: read
    /// with every page of the results.
    found_on_server: Rc<[MessageId]>,
    /// Bumped by every key typed in the search field: a pause searches.
    typed: u64,
    /// `CATMAIL_NSH_OPEN` opened its row already.
    opened_from_env: bool,
    /// Reads of the rows shown started so far (not of more pages).
    reads: u64,
    /// The message the user moved or deleted: once it is gone while still selected, the
    /// next row still there opens.
    removal: Removal,
}

#[derive(Clone, Default)]
struct LoadState(Rc<RefCell<Loads>>);

fn loads() -> Rc<RefCell<Loads>> {
    global::<LoadState>().with_untracked(|l| l.0.clone())
}

/// The list's handle, for scrolling it.
#[derive(Clone, Default)]
struct ListRef(Option<ElementRef>);

/// Whether the reading pane is open: a row is selected.
pub fn reading() -> bool {
    global::<Selected>().with(|s| s.0.is_some())
}

// --- Reading the rows ---------------------------------------------------------------

/// Shows `source` from the top, or nothing.
pub fn show(source: Option<Source>) {
    let generation = {
        let loads = loads();
        let mut loads = loads.borrow_mut();
        loads.generation += 1;
        loads.stale = false;
        loads.found_on_server = Rc::new([]);
        loads.removal.clear();
        loads.generation
    };
    if let Some(Source::Search(text)) = &source {
        search_server(text.clone(), generation);
    }
    global::<Rows>().set(Rows {
        source,
        ..Rows::default()
    });
    global::<Selected>().set(Selected(None));
    global::<Checked>().set(Checked::default());
    if let Some(list) = global::<ListRef>().with_untracked(|r| r.0) {
        list.scroll_to_index(0, Percentage(0.0), px(0.0), false);
    }
    load(Load::Replace(PAGE));
}

/// Asks the servers about `text` once the local results showed for
/// [`SERVER_SEARCH_DELAY`], and reads the results again with what they found, if
/// anything. Failures leave the local results alone; they are only logged.
fn search_server(text: String, generation: u64) {
    in_mailbox(async move {
        timer(SERVER_SEARCH_DELAY).await;
        if loads().borrow().generation != generation {
            return;
        }
        // The engine logs what each server said; this is what kept it from asking.
        let found = match client().search_server(SearchQuery::new(text)).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(error = %e, "couldn't search the servers");
                return;
            }
        };
        let loads = loads();
        if found.is_empty() || loads.borrow().generation != generation {
            return;
        }
        loads.borrow_mut().found_on_server = found.into();
        refresh();
    });
}

/// Reads the rows shown again (after the engine reported a change), keeping the selected
/// row selected. During a sync changes arrive faster than a page loads, so they are
/// coalesced: at most one read runs, and one more follows if needed.
pub fn refresh() {
    if global::<Rows>().with_untracked(|r| r.loading) {
        loads().borrow_mut().stale = true;
        return;
    }
    let shown = global::<Rows>().with_untracked(|r| r.rows.len()) as u32;
    load(Load::Replace(shown.max(PAGE)));
}

enum Load {
    /// The first `n` rows, replacing what is shown.
    Replace(u32),
    /// The next page, after the last row shown.
    After(MessageCursor),
}

fn load(what: Load) {
    let Some(source) = global::<Rows>().with_untracked(|r| r.source.clone()) else {
        return;
    };
    let (limit, after) = match what {
        Load::Replace(n) => (n, None),
        Load::After(cursor) => (PAGE, Some(cursor)),
    };
    let (generation, found_on_server, read) = {
        let loads = loads();
        let mut loads = loads.borrow_mut();
        if after.is_none() {
            loads.reads += 1;
        }
        (loads.generation, loads.found_on_server.clone(), loads.reads)
    };
    global::<Rows>().update(|r| r.loading = true);
    in_mailbox(async move {
        let result = list::fetch(&client(), &source, &found_on_server, limit, after).await;
        if loads().borrow().generation != generation {
            return; // Another source is shown.
        }
        match result {
            Ok((rows, more)) if after.is_some() => global::<Rows>().update(|shown| {
                // The page starts after the last row shown, whatever came or went above
                // it since; the refresh such a change brings updates those.
                let from = shown.rows.len();
                shown.rows.extend(rows);
                shown.days.extend(&shown.rows, from);
                shown.items = items(&shown.rows, &shown.days);
                shown.has_more = more;
                shown.loading = false;
            }),
            Ok((rows, more)) => {
                loads().borrow_mut().removal.read(read, &rows);
                replace(rows, more);
            }
            Err(e) => {
                global::<Rows>().update(|r| r.loading = false);
                report("Couldn't load the list", e);
            }
        }
        if std::mem::take(&mut loads().borrow_mut().stale) {
            refresh();
        }
    });
}

/// Puts the new rows in, keeping the selected row selected by its identity.
fn replace(rows: Vec<Row>, more: bool) {
    let shown = global::<Rows>();
    let unchanged =
        shown.with_untracked(|r| !rows.is_empty() && r.rows == rows && r.has_more == more);
    if unchanged {
        // A sync's events often bring nothing this list shows.
        shown.update(|r| r.loading = false);
        return;
    }
    let selected = global::<Selected>().with_untracked(|s| s.0);
    let next = shown.with_untracked(|r| {
        let at = selected.and_then(|id| r.rows.iter().position(|row| row.id() == id));
        loads()
            .borrow_mut()
            .removal
            .reselect(at.zip(selected), &r.rows, &rows)
    });
    let next_row = match next {
        Reselect::Next(ix) => rows.get(ix).map(Row::id),
        _ => None,
    };
    // Those gone are no longer checked: moved, deleted, or expunged on the server.
    let listed: HashSet<_> = rows.iter().filter_map(Row::message).map(|m| m.id).collect();
    let checked = global::<Checked>();
    if checked.with_untracked(|c| !c.0.is_subset(&listed)) {
        checked.update(|c| c.0.retain(|id| listed.contains(id)));
    }
    let days = Days::new(&rows);
    shown.update(|r| {
        r.items = items(&rows, &days);
        r.rows = rows;
        r.days = days;
        r.has_more = more;
        r.loading = false;
    });
    match (next, next_row) {
        (Reselect::Keep { .. }, _) => {}
        // The user moved or deleted it: on to the next, as if the user opened that.
        (Reselect::Next(_), Some(row)) => open(row),
        // Gone on its own: expunged, sent, a new UIDVALIDITY.
        _ => global::<Selected>().set(Selected(None)),
    }
    open_from_env();
}

/// For looking at the reading pane without a mouse (screenshots of the spike): the row
/// at index `CATMAIL_NSH_OPEN` of the first rows read opens.
fn open_from_env() {
    if std::mem::replace(&mut loads().borrow_mut().opened_from_env, true) {
        return;
    }
    let row = std::env::var("CATMAIL_NSH_OPEN")
        .ok()
        .and_then(|ix| ix.parse::<usize>().ok())
        .and_then(|ix| global::<Rows>().with_untracked(|r| r.rows.get(ix).map(Row::id)));
    if row.is_some() {
        global::<Selected>().set(Selected(row));
    }
}

fn load_more() {
    let after = global::<Rows>().with_untracked(|r| {
        // Only messages have more pages: of folders, the inboxes and search results.
        (r.has_more && !r.loading)
            .then(|| {
                r.rows
                    .last()
                    .and_then(Row::message)
                    .map(MessageSummary::cursor)
            })
            .flatten()
    });
    if let Some(after) = after {
        load(Load::After(after));
    }
}

/// The user opened `row` (or the list moved on to it from one the user moved): opening a
/// message marks it read, unless the settings say not to. Read again after a refresh, it
/// stays as it is.
pub fn open(row: RowId) {
    global::<Selected>().set(Selected(Some(row)));
    let RowId::Message(id) = row else {
        return;
    };
    if crate::shell::settings().mark_read_on_open
        && message(id).is_some_and(|m| !m.flags.contains(Flags::SEEN))
    {
        commands::set_flags(vec![id], Flags::SEEN, Flags::empty());
    }
}

/// The message listed with `id`. Tracked.
pub fn message(id: MessageId) -> Option<MessageSummary> {
    global::<Rows>().with(|r| {
        r.rows
            .iter()
            .filter_map(Row::message)
            .find(|m| m.id == id)
            .cloned()
    })
}

/// The selected row's identity. Tracked.
pub fn selected_id() -> Option<MessageId> {
    match global::<Selected>().with(|s| s.0) {
        Some(RowId::Message(id)) => Some(id),
        _ => None,
    }
}

/// The selected message, as the list has it now. Tracked.
pub fn selected_message() -> Option<MessageSummary> {
    message(selected_id()?)
}

/// The messages checked, top to bottom. Tracked.
pub fn checked_messages() -> Vec<MessageSummary> {
    let checked = global::<Checked>().with(|c| c.0.clone());
    if checked.is_empty() {
        return Vec::new();
    }
    global::<Rows>().with(|r| {
        r.rows
            .iter()
            .filter_map(Row::message)
            .filter(|m| checked.contains(&m.id))
            .cloned()
            .collect()
    })
}

/// What a key acts on: the messages checked, or else the selected one.
fn keyed() -> Vec<MessageSummary> {
    let checked = untracked(checked_messages);
    if checked.is_empty() {
        untracked(selected_message).into_iter().collect()
    } else {
        checked
    }
}

/// Unchecks the messages moved or deleted: those still listed (on Gmail, one moved to a
/// label from All Mail) have been dealt with.
pub fn uncheck(ids: &[MessageId]) {
    let checked = global::<Checked>();
    if checked.with_untracked(|c| ids.iter().any(|id| c.0.contains(id))) {
        checked.update(|c| c.0.retain(|id| !ids.contains(id)));
    }
}

/// Whether the list shows search results.
pub fn is_searching() -> bool {
    global::<Rows>().with_untracked(|r| matches!(r.source, Some(Source::Search(_))))
}

/// Selects the next row once `id`, the selected message, goes away. Returns the
/// removal's number, for [`removal_settled`].
pub fn expect_removal(id: MessageId) -> u64 {
    loads().borrow_mut().removal.expect(id)
}

/// The change behind `removal`, which may take the message out of the list, was made
/// locally (`made`), or failed: the rows are read again, and say whether it left
/// ([`Removal::settled`]).
pub fn removal_settled(removal: u64, made: bool) {
    let again = {
        let loads = loads();
        let mut loads = loads.borrow_mut();
        // A read in flight may have started before the change: the next one shows it.
        let next_read = loads.reads + 1;
        loads.removal.settled(removal, made, next_read)
    };
    if again {
        refresh();
    }
}

/// The accounts' user labels, by the name a row shows them by.
pub fn set_labels() {
    let names: HashMap<_, _> = global::<Accounts>().with_untracked(|a| {
        a.0.iter()
            .flatten()
            .flat_map(|entry| entry.folders.user_labels())
            .collect()
    });
    if global::<LabelNames>().with_untracked(|l| l.0 != names) {
        global::<LabelNames>().set(LabelNames(names));
    }
}

// --- Searching ----------------------------------------------------------------------

/// The search field changed: a pause in typing searches, or with nothing typed shows
/// the place selected again.
fn typed(text: String) {
    global::<SearchText>().set(SearchText(text.clone()));
    let typed = {
        let loads = loads();
        let mut loads = loads.borrow_mut();
        loads.typed += 1;
        loads.typed
    };
    in_mailbox(async move {
        timer(SEARCH_DELAY).await;
        if loads().borrow().typed != typed {
            return;
        }
        let text = text.trim();
        let source = if text.is_empty() {
            global::<SelectedPlace>().with_untracked(|p| p.0.map(|p| p.source()))
        } else {
            Some(Source::Search(text.to_owned()))
        };
        if global::<Rows>().with_untracked(|r| r.source != source) {
            show(source);
        }
    });
}

/// Leaving a search for a place: the place's rows, not the results.
pub fn clear_search() {
    loads().borrow_mut().typed += 1;
    global::<SearchText>().set(SearchText::default());
}

// --- Checking -----------------------------------------------------------------------

fn toggle_checked(id: MessageId) {
    global::<Checked>().update(|c| {
        if !c.0.remove(&id) {
            c.0.insert(id);
        }
    });
}

/// Checks the messages listed that `which` takes, and only those: of a folder, those of
/// the pages read.
fn check(which: Check) {
    let checked = global::<Rows>().with_untracked(|r| {
        r.rows
            .iter()
            .filter_map(Row::message)
            .filter(|m| which.takes(m.flags))
            .map(|m| m.id)
            .collect()
    });
    global::<Checked>().set(Checked(checked));
}

/// The keys of the commands on messages, as the GUI binds them: on those checked, or else
/// the selected one.
fn bind_command_keys() {
    let keys = [
        ("e", MessageCommand::Archive),
        ("delete", MessageCommand::Delete),
        ("backspace", MessageCommand::Delete),
        ("mod+shift+m", MessageCommand::Move),
        ("mod+shift+u", MessageCommand::ToggleRead),
        ("mod+shift+l", MessageCommand::ToggleFlag),
    ];
    for (key, command) in keys {
        on_key(key, move |_| commands::run(command, keyed()));
    }
    // Gmail's key for them.
    on_key("l", |_| commands::edit_labels());
}

/// X, as Gmail's: checks the selected message, or unchecks it.
fn toggle_selected() {
    if let Some(RowId::Message(id)) = global::<Selected>().with_untracked(|s| s.0) {
        toggle_checked(id);
    }
}

// --- The pane -----------------------------------------------------------------------

/// The list column: the search field, the title, and the rows.
pub fn list_column() -> impl IntoElement {
    let list = element_ref();
    global::<ListRef>().set(ListRef(Some(list)));
    on_key("x", move |_| toggle_selected());
    bind_command_keys();
    // Opening a row or closing it changes every row's height: the one opened, or just
    // closed, would move away from under the pointer.
    let last_selected = stored(None::<RowId>);
    effect(move || {
        let selected = global::<Selected>().with(|s| s.0);
        let before = last_selected.replace(selected);
        if selected.is_some() == before.is_some() {
            return;
        }
        let row = selected.or(before);
        let ix = global::<Rows>().with_untracked(|r| {
            r.items
                .iter()
                .position(|item| matches!(item, Item::Row(row_) if Some(row_.id()) == row))
        });
        if let Some(ix) = ix {
            list.scroll_to_index(ix as u32, Percentage(0.5), px(0.0), false);
        }
    });
    view().flex_1().min_w(px(0.0)).child(header()).child(
        virtual_list(move || global::<Rows>().with(|r| r.items.len()), item)
            .keys(move || {
                global::<Rows>().with(|r| {
                    r.items
                        .iter()
                        .enumerate()
                        .map(|(ix, item)| item.key(ix))
                        .collect::<Vec<_>>()
                })
            })
            .on_load_more(load_more)
            .empty(
                view()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .p(px(24.0))
                    .child(
                        text(move || {
                            let (source, loading) =
                                global::<Rows>().with(|r| (r.source.clone(), r.loading));
                            // Nothing while the first page is on its way.
                            if loading {
                                String::new()
                            } else {
                                list::empty_text(source.as_ref()).to_owned()
                            }
                        })
                        .text_sm()
                        .text_color(move || theme().muted_foreground),
                    ),
            )
            .estimated_item_size(px(48.0))
            .element_ref(&list)
            .px(px(12.0))
            .flex_1(),
    )
}

fn header() -> impl IntoElement {
    let title = computed(move || {
        let source = global::<Rows>().with(|r| r.source.clone());
        let place = global::<SelectedPlace>().with(|p| p.0);
        global::<Accounts>().with(|a| {
            let accounts = a.0.as_deref().unwrap_or_default();
            (
                mailbox::list_title(source.as_ref(), place, accounts),
                mailbox::list_note(source.as_ref(), place, accounts),
            )
        })
    });
    let lists_messages =
        move || global::<Rows>().with(|r| r.source.as_ref().is_some_and(Source::lists_messages));
    view()
        .px(px(12.0))
        .pt(px(12.0))
        .pb(px(8.0))
        .gap(px(10.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .h(px(36.0))
                .px(px(12.0))
                .rounded(px(18.0))
                .border_1()
                .border_color(move || theme().input)
                .child(
                    icon(|| Icon::MagnifyingGlass, 16.0)
                        .text_color(move || theme().muted_foreground),
                )
                .child(
                    input()
                        .value(move || global::<SearchText>().with(|t| t.0.clone()))
                        .placeholder("Search".to_owned())
                        .on_change(move |text: &str| typed(text.to_owned()))
                        .flex_1()
                        .text_sm()
                        .text_color(move || theme().foreground),
                ),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(4.0))
                .child(when(lists_messages, check_all))
                .child(
                    text(move || title.with(|t| t.0.clone()))
                        .number_of_lines(1)
                        .flex_shrink(1.0)
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(move || theme().foreground),
                )
                .child(dynamic(move || {
                    // The commands on the messages checked take the note's room.
                    if has_checked() {
                        view()
                            .flex_1()
                            .flex_row()
                            .justify_end()
                            .child(commands::buttons(checked_messages, Size::Bar))
                            .into_element()
                    } else {
                        text(move || title.with(|t| t.1.clone().unwrap_or_default()))
                            .text_xs()
                            .text_color(move || theme().muted_foreground)
                            .into_element()
                    }
                })),
        )
}

fn has_checked() -> bool {
    global::<Checked>().with(|c| !c.0.is_empty())
}

/// Checks every message listed, or none; its menu checks those read, flagged and so on.
fn check_all() -> impl IntoElement {
    let counts = computed(move || {
        let listed = global::<Rows>().with(|r| r.rows.iter().filter_map(Row::message).count());
        let checked = global::<Checked>().with(|c| c.0.len());
        (listed, checked)
    });
    let all = move || counts.with(|&(listed, checked)| checked > 0 && checked == listed);
    view()
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .child(
            view()
                .w(px(16.0))
                .h(px(16.0))
                .rounded(px(4.0))
                .border_1()
                .items_center()
                .justify_center()
                .border_color(move || {
                    if all() {
                        theme().primary
                    } else {
                        theme().input
                    }
                })
                .bg(move || {
                    if all() {
                        theme().primary
                    } else {
                        AdaptiveColor::TRANSPARENT
                    }
                })
                .label("Select all")
                .on_press(move || {
                    if all() {
                        global::<Checked>().set(Checked::default());
                    } else {
                        check(Check::All);
                    }
                })
                .child(when(all, || {
                    icon(|| Icon::Check, 12.0).text_color(move || theme().primary_foreground)
                })),
        )
        .child(
            view()
                .p(px(2.0))
                .label("Select")
                .on_press(|| {
                    let items = Check::MENU
                        .into_iter()
                        .map(|which| MenuItem::new(which.label()).on_select(move || check(which)))
                        .collect();
                    // Nothing waits on it closing.
                    drop(popup_menu(items));
                })
                .child(
                    icon(|| Icon::ChevronDown, 12.0).text_color(move || theme().muted_foreground),
                ),
        )
}

/// What a cell shows: a day's heading or a row. A cell shows other items as the list
/// scrolls, so which it is follows its position.
fn item(at: Position) -> impl IntoElement {
    let item = computed(move || global::<Rows>().with(|r| r.items.get(at.index()).cloned()));
    let kind = computed(move || {
        item.with(|item| match item {
            Some(Item::Day(_)) | None => 0,
            Some(Item::Row(Row::Message(_))) => 1,
            Some(Item::Row(Row::Draft(_))) => 2,
            Some(Item::Row(Row::Outbox(_))) => 3,
        })
    });
    dynamic(move || match kind.get() {
        1 => message_row(item).into_element(),
        2 => draft_row(item).into_element(),
        3 => outbox_row(item).into_element(),
        _ => text(move || match item.get() {
            Some(Item::Day(label)) => label,
            _ => String::new(),
        })
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .px(px(8.0))
        .pt(px(12.0))
        .pb(px(4.0))
        .text_color(move || theme().muted_foreground)
        .into_element(),
    })
}

/// The row an item is, if it is one.
fn row_of(item: Memo<Option<Item>>) -> impl Fn() -> Option<Row> + Copy {
    move || match item.get() {
        Some(Item::Row(row)) => Some(row),
        _ => None,
    }
}

/// A row's box: selected, it has the accent behind it; pressed, it opens.
fn row_box(row: impl Fn() -> Option<Row> + Copy + 'static) -> View {
    let id = move || row().map(|r| r.id());
    let selected = move || {
        let id = id();
        id.is_some() && global::<Selected>().with(|s| s.0) == id
    };
    view()
        .rounded(px(8.0))
        .my(px(1.0))
        .style_when(selected, |s| s.bg(move || theme().accent))
        .on_press(move || {
            if let Some(id) = id() {
                open(id);
            }
        })
}

fn message_row(item: Memo<Option<Item>>) -> impl IntoElement {
    let message = computed(move || match item.get() {
        Some(Item::Row(Row::Message(m))) => Some(m),
        _ => None,
    });
    let id = move || message.with(|m| m.as_ref().map(|m| m.id));
    let field = move |f: fn(&MessageSummary) -> String| {
        move || message.with(|m| m.as_ref().map(f).unwrap_or_default())
    };
    let unread =
        move || message.with(|m| m.as_ref().is_some_and(|m| !m.flags.contains(Flags::SEEN)));
    let weight = move || {
        if unread() {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        }
    };
    let hovered = move || {
        let id = id();
        id.is_some() && global::<Hovered>().with(|h| h.0) == id
    };
    let who = move || {
        text(field(|m| format::sender(&m.from).to_owned()))
            .number_of_lines(1)
            .text_sm()
    };
    let subject = move || {
        text(field(|m| format::subject(m.subject.as_deref()).to_owned()))
            .number_of_lines(1)
            .text_sm()
    };
    let time = move || {
        text(field(|m| format::time(m.date)))
            .text_xs()
            .flex_shrink_0()
            .text_color(move || theme().muted_foreground)
    };
    let avatar = move || avatar(message);
    let marks = move || marks(message);
    row_box(row_of(item))
        .label(move || {
            SharedString::from(message.with(|m| {
                m.as_ref()
                    .map(|m| list::message_label(m, &label_names(m)))
                    .unwrap_or_default()
            }))
        })
        .on_mouse_enter(move |_: &MouseEvent| global::<Hovered>().set(Hovered(id())))
        .on_mouse_leave(move |_: &MouseEvent| {
            if global::<Hovered>().with_untracked(|h| h.0) == id() {
                global::<Hovered>().set(Hovered(None));
            }
        })
        .child(dynamic(move || {
            if reading() {
                // The subject under the sender, as the reading pane leaves the rows less
                // room; the avatar as on one line.
                view()
                    .flex_row()
                    .items_start()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(8.0))
                    .child(avatar().mt(px(2.0)))
                    .child(
                        view()
                            .flex_1()
                            .min_w(px(0.0))
                            .gap(px(2.0))
                            .child(
                                view()
                                    .flex_row()
                                    .gap(px(8.0))
                                    .child(
                                        who()
                                            .flex_1()
                                            .font_weight(weight())
                                            .text_color(move || theme().foreground),
                                    )
                                    .child(time()),
                            )
                            .child(
                                view()
                                    .flex_row()
                                    .items_start()
                                    .gap(px(6.0))
                                    .child(
                                        // Set apart from the sender by color: weight tells
                                        // unread.
                                        subject()
                                            .number_of_lines(2)
                                            .flex_1()
                                            .font_weight(weight())
                                            .text_color(move || theme().muted_foreground),
                                    )
                                    .child(marks()),
                            ),
                    )
                    .into_element()
            } else {
                // The senders as a column, so the subjects line up.
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(20.0))
                    .py(px(10.0))
                    .child(avatar())
                    .child(
                        who()
                            .w(px(192.0))
                            .flex_shrink_0()
                            .font_weight(weight())
                            .text_color(move || theme().foreground),
                    )
                    .child(
                        // Only as wide as it is: the commands shown while hovered
                        // follow it.
                        subject()
                            .flex_shrink(1.0)
                            .min_w(px(0.0))
                            .font_weight(weight())
                            .text_color(move || theme().foreground),
                    )
                    // The commands, within reach of the pointer: the message isn't open.
                    .child(when(hovered, move || {
                        commands::buttons(
                            move || message.with(|m| m.iter().cloned().collect()),
                            Size::Row,
                        )
                    }))
                    .child(view().flex_1())
                    // Whom it went to, while hovered.
                    .child(when(hovered, move || {
                        text(field(list::recipients))
                            .number_of_lines(1)
                            .max_w(px(256.0))
                            .text_xs()
                            .text_color(move || theme().muted_foreground)
                    }))
                    .child(marks())
                    .child(time())
                    .into_element()
            }
        }))
}

/// The names of the message's user labels a row shows: not the one listed, which every
/// row has.
fn label_names(message: &MessageSummary) -> Vec<String> {
    let listed = global::<Rows>().with(|r| match r.source {
        Some(Source::Folder(id)) => Some(id),
        _ => None,
    });
    global::<LabelNames>().with(|l| list::label_names(&l.0, message, listed))
}

/// The names of all the message's user labels, as the reading pane shows them.
pub fn user_label_names(message: &MessageSummary) -> Vec<String> {
    global::<LabelNames>().with(|l| list::label_names(&l.0, message, None))
}

/// What follows the subject: the labels, the paper clip, the flag.
fn marks(message: Memo<Option<MessageSummary>>) -> View {
    let labels =
        computed(move || message.with(|m| m.as_ref().map(label_names).unwrap_or_default()));
    let has = move |flag: fn(&MessageSummary) -> bool| {
        move || message.with(|m| m.as_ref().is_some_and(flag))
    };
    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .gap(px(6.0))
        .child(each(
            move || labels.with(|l| l.iter().take(ROW_LABELS).cloned().collect::<Vec<_>>()),
            |name| name.clone(),
            |name, _| {
                text(move || name.get())
                    .number_of_lines(1)
                    .max_w(px(96.0))
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .text_xs()
                    .bg(move || theme().muted)
                    .text_color(move || theme().muted_foreground)
            },
        ))
        .child(
            text(
                move || match labels.with(Vec::len).saturating_sub(ROW_LABELS) {
                    0 => String::new(),
                    more => format!("+{more}"),
                },
            )
            .text_xs()
            .text_color(move || theme().muted_foreground),
        )
        .child(when(has(|m| m.has_attachments), || {
            icon(|| Icon::PaperClip, 12.0).text_color(move || theme().muted_foreground)
        }))
        .child(when(has(|m| m.flags.contains(Flags::FLAGGED)), || {
            icon(|| Icon::FlagSolid, 12.0).text_color(FLAG)
        }))
}

/// The flag's amber, as the GUI's theme has it.
const FLAG: Hsla = hsla(38.0 / 360.0, 0.92, 0.50, 1.0);

/// The avatars' six hues ([`list::avatar_hue`]): red, green, yellow, blue, magenta, cyan.
const HUES: [f32; 6] = [0.0, 140.0, 45.0, 215.0, 300.0, 185.0];

/// An avatar's background, by the sender's address: a tint of their hue, as dark
/// backgrounds need more of it to show as much.
pub fn avatar_color(seed: &str) -> AdaptiveColor {
    let hue = HUES[list::avatar_hue(seed)] / 360.0;
    let share = if dark() { 0.30 } else { 0.22 };
    hsla(hue, 0.65, 0.5, share).into()
}

/// The sender's initial on their hue, or a check while the message is checked; pressing
/// it checks or unchecks the message, not opening it as well.
fn avatar(message: Memo<Option<MessageSummary>>) -> View {
    let id = move || message.with(|m| m.as_ref().map(|m| m.id));
    let checked = move || {
        let id = id();
        id.is_some_and(|id| global::<Checked>().with(|c| c.0.contains(&id)))
    };
    let seed = move || {
        message.with(|m| {
            m.as_ref()
                .and_then(|m| m.from.first())
                .map(|a| a.email.clone())
                .unwrap_or_default()
        })
    };
    view()
        .w(px(24.0))
        .h(px(24.0))
        .flex_shrink_0()
        .rounded(px(12.0))
        .items_center()
        .justify_center()
        .bg(move || {
            if checked() {
                theme().primary
            } else {
                avatar_color(&seed())
            }
        })
        .label("Select")
        .on_press(move || {
            if let Some(id) = id() {
                toggle_checked(id);
            }
        })
        .child(dynamic(move || {
            if checked() {
                icon(|| Icon::Check, 14.0)
                    .text_color(move || theme().primary_foreground)
                    .into_element()
            } else {
                text(move || {
                    message.with(|m| {
                        m.as_ref()
                            .map(|m| list::initial(format::sender(&m.from)))
                            .unwrap_or_default()
                    })
                })
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(move || theme().foreground)
                .into_element()
            }
        }))
}

fn draft_row(item: Memo<Option<Item>>) -> impl IntoElement {
    let draft = computed(move || match item.get() {
        Some(Item::Row(Row::Draft(d))) => Some(d),
        _ => None,
    });
    let field = move |f: fn(&catmail_core::Draft) -> String| {
        move || draft.with(|d| d.as_deref().map(f).unwrap_or_default())
    };
    let who = move || {
        text(field(list::draft_recipients))
            .number_of_lines(1)
            .text_sm()
    };
    let subject = move || {
        text(field(|d| {
            format::subject(Some(&d.content.subject)).to_owned()
        }))
        .number_of_lines(1)
        .text_sm()
    };
    let time = move || {
        text(field(|d| format::time(Some(d.updated_at))))
            .text_xs()
            .flex_shrink_0()
            .text_color(move || theme().muted_foreground)
    };
    let clip = move || {
        when(
            move || draft.with(|d| d.as_ref().is_some_and(|d| !d.attachments.is_empty())),
            || icon(|| Icon::PaperClip, 12.0).text_color(move || theme().muted_foreground),
        )
    };
    row_box(row_of(item)).child(dynamic(move || {
        if reading() {
            view()
                .px(px(8.0))
                .py(px(8.0))
                .gap(px(2.0))
                .child(
                    view()
                        .flex_row()
                        .gap(px(8.0))
                        .child(who().flex_1().text_color(move || theme().foreground))
                        .child(time()),
                )
                .child(
                    view()
                        .flex_row()
                        .gap(px(6.0))
                        .child(
                            subject()
                                .number_of_lines(2)
                                .flex_1()
                                .text_color(move || theme().muted_foreground),
                        )
                        .child(clip()),
                )
                .into_element()
        } else {
            view()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .px(px(20.0))
                .py(px(10.0))
                .child(
                    who()
                        .w(px(192.0))
                        .flex_shrink_0()
                        .text_color(move || theme().foreground),
                )
                .child(subject().flex_1().text_color(move || theme().foreground))
                .child(clip())
                .child(time())
                .into_element()
        }
    }))
}

fn outbox_row(item: Memo<Option<Item>>) -> impl IntoElement {
    let entry = computed(move || match item.get() {
        Some(Item::Row(Row::Outbox(e))) => Some(e),
        _ => None,
    });
    let field = move |f: fn(&catmail_core::OutboxEntry) -> String| {
        move || entry.with(|e| e.as_ref().map(f).unwrap_or_default())
    };
    let severity = move || entry.with(|e| e.as_ref().and_then(|e| list::outbox_status(e).1));
    row_box(row_of(item))
        .px(px(8.0))
        .py(px(8.0))
        .gap(px(2.0))
        .child(
            view()
                .flex_row()
                .gap(px(8.0))
                .child(
                    text(field(|e| format::subject(Some(&e.subject)).to_owned()))
                        .number_of_lines(1)
                        .flex_1()
                        .text_sm()
                        .style_when(
                            move || severity().is_some(),
                            |s| s.font_weight(FontWeight::BOLD),
                        )
                        .text_color(move || theme().foreground),
                )
                .child(
                    text(field(|e| format::time(Some(e.created_at))))
                        .text_xs()
                        .text_color(move || theme().muted_foreground),
                ),
        )
        .child(
            text(field(|e| format!("To {}", e.recipients.join(", "))))
                .number_of_lines(1)
                .text_xs()
                .text_color(move || theme().muted_foreground),
        )
        .child(
            view().flex_row().child(
                text(field(|e| list::outbox_status(e).0.to_owned()))
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .text_xs()
                    .bg(move || match severity() {
                        Some(Severity::Warning) => hsla(38.0 / 360.0, 0.92, 0.5, 0.2).into(),
                        Some(Severity::Danger) => hsla(0.0, 0.8, 0.5, 0.2).into(),
                        None => theme().muted,
                    })
                    .text_color(move || theme().foreground),
            ),
        )
}
