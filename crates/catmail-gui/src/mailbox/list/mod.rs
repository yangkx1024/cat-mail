//! The middle pane: what the selected place lists, or search results, newest first under a
//! heading for each day; a row on one line while nothing is open, the subject under the
//! sender while the reading pane is; the messages checked for a command on them all. What a
//! source lists, the days and lines, the reads and where the selection goes after them, and
//! what a row says are `catmail_frontend::list`'s (`Listing`, `ListModel`); here they are
//! read, kept in nsh's signals and drawn.

mod rows;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use catmail_core::{Draft, Flags, FolderId, MessageId, MessageSummary, OutboxEntry, SearchQuery};
use catmail_frontend::commands::MessageCommand;
use catmail_frontend::composer::ComposeFrom;
use catmail_frontend::list::{
    self, Check, Fetch, ListModel, Listing, Loaded, Row, RowId, Rows, SEARCH_DELAY,
    SERVER_SEARCH_DELAY, Select,
};
use catmail_frontend::mailbox::{self, Source};
use nsh_base::theme;
use nsh_core::element::desktop::{MenuItem, popup_menu};
use nsh_core::element::{ElementRef, input};
use nsh_core::geometry::Percentage;
use nsh_core::prelude::*;

use crate::buttons::Size;
use crate::chrome;
use crate::engine::client;
use crate::icons::{Icon, icon};
use crate::mailbox::commands;
use crate::mailbox::sidebar::{self, Accounts, SelectedPlace};
use crate::tasks::in_view;
use crate::toast::report;

use rows::item;
pub use rows::{avatar_color, user_label_names};

/// The row the reading pane shows. Its index changes as rows come and go; it doesn't.
#[derive(Clone, Default)]
pub struct Selected(pub Option<RowId>);

/// The messages checked, for a command on them all: apart from the selected row, the one
/// the reader shows. Always among the rows.
#[derive(Clone, Default)]
pub struct Checked(pub HashSet<MessageId>);

/// A page is being read. Apart from the [`Listing`]: the rows don't change with it.
#[derive(Clone, Copy, Default, PartialEq)]
struct Loading(bool);

/// What the search field holds.
#[derive(Clone, Default)]
struct SearchText(String);

/// The name of each user label shown, of every account: a row shows those of its
/// message's labels.
#[derive(Clone, Default)]
struct LabelNames(HashMap<FolderId, String>);

/// The reads of the list. Not reactive.
#[derive(Clone, Default)]
struct Reads(Rc<RefCell<ListModel>>);

fn reads() -> Rc<RefCell<ListModel>> {
    global::<Reads>().with_untracked(|r| r.0.clone())
}

/// What the view adds to the reads. Not reactive.
#[derive(Default)]
struct Typing {
    /// Counts the keys typed in the search field: a pause searches if no other key
    /// followed.
    typed: Cell<u64>,
    /// `CATMAIL_GUI_OPEN` opened its row already.
    opened_from_env: Cell<bool>,
}

#[derive(Clone, Default)]
struct TypingState(Rc<Typing>);

fn typing() -> Rc<Typing> {
    global::<TypingState>().with_untracked(|t| t.0.clone())
}

/// Counts a key typed in the search field, or the field cleared.
fn count_typed() -> u64 {
    let typing = typing();
    typing.typed.set(typing.typed.get() + 1);
    typing.typed.get()
}

/// The list's handle, for scrolling it.
#[derive(Clone, Default)]
struct ListRef(Option<ElementRef>);

/// The search field's, for Ctrl+F.
#[derive(Clone, Default)]
struct SearchRef(Option<ElementRef>);

/// Puts the keys in the search field.
pub fn focus_search() {
    if let Some(search) = global::<SearchRef>().with_untracked(|r| r.0) {
        search.focus();
    }
}

/// Whether the reading pane is open: a row is selected.
pub fn reading() -> bool {
    global::<Selected>().with(|s| s.0.is_some())
}

// --- Reading the rows ---------------------------------------------------------------

/// Shows `source` from the top, or nothing.
pub fn show(source: Option<Source>) {
    let (listing, fetch) = reads().borrow_mut().show(source.clone());
    if let Some(Source::Search(text)) = source {
        search_server(text, reads().borrow().generation());
    }
    global::<Listing>().set(listing);
    global::<Selected>().set(Selected(None));
    global::<Checked>().set(Checked::default());
    if let Some(list) = global::<ListRef>().with_untracked(|r| r.0) {
        list.scroll_to_index(0, Percentage(0.0), px(0.0), false);
    }
    read(fetch);
}

/// Asks the servers about `text` once the local results showed for
/// [`SERVER_SEARCH_DELAY`], and reads the results again with what they found, if
/// anything. Failures leave the local results alone; they are only logged.
fn search_server(text: String, generation: u64) {
    in_view(async move {
        timer(SERVER_SEARCH_DELAY).await;
        if reads().borrow().generation() != generation {
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
        if reads().borrow_mut().found_on_server(generation, found) {
            refresh();
        }
    });
}

/// Reads the rows shown again (after the engine reported a change), keeping the selected
/// row selected; one more read follows one running (`ListModel::refresh`).
pub fn refresh() {
    let fetch = global::<Listing>().with_untracked(|l| reads().borrow_mut().refresh(l));
    read(fetch);
}

fn load_more() {
    let fetch = global::<Listing>().with_untracked(|l| reads().borrow_mut().more(l));
    read(fetch);
}

/// Runs `fetch`, if a read is to start, and shows what it brings.
fn read(fetch: Option<Fetch>) {
    show_loading();
    let Some(fetch) = fetch else {
        return;
    };
    in_view(async move {
        let (rows, error) = match fetch.run(&client()).await {
            Ok(rows) => (Some(rows), None),
            Err(e) => (None, Some(e)),
        };
        let selected = global::<Selected>().with_untracked(|s| s.0);
        let loaded = global::<Checked>().with_untracked(|checked| {
            global::<Listing>().with_untracked(|listing| {
                let reads = reads();
                let mut reads = reads.borrow_mut();
                reads.loaded(&fetch, rows, listing, selected, &checked.0)
            })
        });
        // Another source is shown.
        let Some(loaded) = loaded else {
            return;
        };
        show_loading();
        if let Some(e) = error {
            report("Couldn't load the list", e);
        }
        show_loaded(loaded);
    });
}

fn show_loading() {
    let reading = Loading(reads().borrow().is_reading());
    if global::<Loading>().get_untracked() != reading {
        global::<Loading>().set(reading);
    }
}

fn show_loaded(loaded: Loaded) {
    if let Some(checked) = loaded.checked {
        global::<Checked>().set(Checked(checked));
    }
    let replaced = matches!(loaded.rows, Some(Rows::Replace(..)));
    if let Some(rows) = loaded.rows {
        global::<Listing>().update(|listing| rows.apply(listing));
    }
    match loaded.select {
        Select::Keep => {}
        Select::Open(row) => open(row),
        Select::Clear => global::<Selected>().set(Selected(None)),
    }
    if replaced {
        open_from_env();
    }
    if loaded.again {
        refresh();
    }
}

/// For looking at the reading pane without a mouse (screenshots): the row
/// at index `CATMAIL_GUI_OPEN` of the first rows read opens.
fn open_from_env() {
    if typing().opened_from_env.replace(true) {
        return;
    }
    let row = std::env::var("CATMAIL_GUI_OPEN")
        .ok()
        .and_then(|ix| ix.parse::<usize>().ok())
        .and_then(|ix| global::<Listing>().with_untracked(|l| l.rows().get(ix).map(Row::id)));
    if row.is_some() {
        global::<Selected>().set(Selected(row));
    }
}

/// Selects and opens message `id` of the source shown: now if it is listed, or else once
/// the rows read from now on have it (it may have just arrived). Nothing happens if the
/// next of them doesn't, if another source is shown first, or if the user opens a row
/// meanwhile.
pub fn reveal(id: MessageId) {
    let (row, again) = global::<Listing>().with_untracked(|l| reads().borrow_mut().reveal(id, l));
    if let Some(row) = row {
        open(row);
    }
    if again {
        refresh();
    }
}

/// The user opened `row` (or the list moved on to it from one the user moved): opening a
/// message marks it read, unless the settings say not to. Read again after a refresh, it
/// stays as it is.
pub fn open(row: RowId) {
    // The user's choice wins over a notification clicked before.
    reads().borrow_mut().opened();
    global::<Selected>().set(Selected(Some(row)));
    let RowId::Message(id) = row else {
        return;
    };
    if crate::shell::settings().mark_read_on_open
        && message(id).is_some_and(|m| !m.flags.contains(Flags::SEEN))
    {
        commands::set_flags(catmail_frontend::commands::mark_read(id));
    }
}

/// The message listed with `id`. Tracked.
pub fn message(id: MessageId) -> Option<MessageSummary> {
    global::<Listing>().with(|l| l.message(id).cloned())
}

/// The selected row's identity. Tracked.
pub fn selected_id() -> Option<MessageId> {
    match global::<Selected>().with(|s| s.0) {
        Some(RowId::Message(id)) => Some(id),
        _ => None,
    }
}

/// The selected draft, as the list has it now. Tracked.
pub fn selected_draft() -> Option<Rc<Draft>> {
    selected_row(|row| match row {
        Row::Draft(draft) => Some(draft.clone()),
        _ => None,
    })
}

/// The selected outbox entry, as the list has it now. Tracked.
pub fn selected_outbox() -> Option<OutboxEntry> {
    selected_row(|row| match row {
        Row::Outbox(entry) => Some(entry.clone()),
        _ => None,
    })
}

/// The row selected, as `pick` takes it. Tracked.
fn selected_row<T>(pick: impl FnOnce(&Row) -> Option<T>) -> Option<T> {
    let id = global::<Selected>().with(|s| s.0)?;
    global::<Listing>().with(|l| l.row(id).and_then(pick))
}

/// A draft opens in its window: on Enter, or a second click on its row.
fn edit_selected_draft() {
    if let Some(RowId::Draft(id)) = global::<Selected>().with_untracked(|s| s.0) {
        crate::composer::open(ComposeFrom::Draft(id));
    }
}

/// The selected message, as the list has it now. Tracked.
pub fn selected_message() -> Option<MessageSummary> {
    message(selected_id()?)
}

/// The messages checked, top to bottom. Tracked.
pub fn checked_messages() -> Vec<MessageSummary> {
    global::<Checked>().with(|c| global::<Listing>().with(|l| l.checked(&c.0)))
}

/// What a key acts on: the messages checked, or else the selected one.
fn keyed() -> Vec<MessageSummary> {
    let selected = global::<Selected>().with_untracked(|s| s.0);
    global::<Checked>()
        .with_untracked(|c| global::<Listing>().with_untracked(|l| l.keyed(&c.0, selected)))
}

/// Unchecks the messages moved or deleted: those still listed (on Gmail, one moved to a
/// label from All Mail) have been dealt with.
pub fn uncheck(ids: &[MessageId]) {
    let checked = global::<Checked>();
    if checked.with_untracked(|c| ids.iter().any(|id| c.0.contains(id))) {
        checked.update(|c| c.0.retain(|id| !ids.contains(id)));
    }
}

/// What the list shows. Untracked.
pub fn source() -> Option<Source> {
    global::<Listing>().with_untracked(|l| l.source().cloned())
}

/// `ids` are about to be moved or deleted: if the selected message is among them, the
/// next row is selected once it goes away. Returns the removal's number, for
/// [`removal_settled`].
pub fn expect_removal(ids: &[MessageId]) -> Option<u64> {
    let selected = global::<Selected>().with_untracked(|s| s.0);
    reads().borrow_mut().expect_removal(selected, ids)
}

/// The change behind `removal`, which may take the message out of the list, was made
/// locally (`made`), or failed: the rows are read again, and say whether it left.
pub fn removal_settled(removal: u64, made: bool) {
    if reads().borrow_mut().removal_settled(removal, made) {
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
    let typed = count_typed();
    in_view(async move {
        timer(SEARCH_DELAY).await;
        if typing().typed.get() != typed {
            return;
        }
        let text = text.trim();
        let source = if text.is_empty() {
            global::<SelectedPlace>().with_untracked(|p| p.0.map(|p| p.source()))
        } else {
            Some(Source::Search(text.to_owned()))
        };
        if global::<Listing>().with_untracked(|l| l.source() != source.as_ref()) {
            show(source);
        }
    });
}

/// Leaving a search for a place: the place's rows, not the results.
pub fn clear_search() {
    count_typed();
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
    let checked = global::<Listing>().with_untracked(|l| {
        l.messages()
            .filter(|m| which.takes(m.flags))
            .map(|m| m.id)
            .collect()
    });
    global::<Checked>().set(Checked(checked));
}

/// The keys of the commands on messages: on those checked, or else
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
    on_key("enter", move |_| edit_selected_draft());
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
        let ix = selected
            .or(before)
            .and_then(|row| global::<Listing>().with_untracked(|l| l.line_of(row)));
        if let Some(ix) = ix {
            list.scroll_to_index(ix as u32, Percentage(0.5), px(0.0), false);
        }
    });
    view().flex_1().min_w(px(0.0)).child(header()).child(
        virtual_list(move || global::<Listing>().with(Listing::len), item)
            .content_padding_x(px(12.0))
            .keys(move || global::<Listing>().with(Listing::keys))
            .on_load_more(load_more)
            .empty(
                view()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .p(px(24.0))
                    .child(
                        text(move || {
                            // Nothing while the first page is on its way.
                            if global::<Loading>().get().0 {
                                String::new()
                            } else {
                                let source = global::<Listing>().with(|l| l.source().cloned());
                                list::empty_text(source.as_ref()).to_owned()
                            }
                        })
                        .text_sm()
                        .text_color(move || theme().muted_foreground),
                    ),
            )
            .estimated_item_size(px(48.0))
            .element_ref(&list)
            .flex_1(),
    )
}

/// Syncs every account now; it turns while one is syncing.
fn sync_button() -> impl IntoElement {
    // Only when it starts or stops: the accounts are read again many times meanwhile, and
    // starting the turn again each time would jerk it back.
    let syncing = computed(sidebar::syncing);
    let angle = animated_value(0.0);
    let turning =
        Animation::timing(&angle, 360.0, Timing::new(TURN_MS, Easing::Linear)).looped(None);
    effect(move || {
        if syncing.get() {
            let _ = turning.start();
        } else {
            spawn(finish_turn(angle));
        }
    });
    let has_accounts =
        move || global::<Accounts>().with(|a| a.0.as_ref().is_some_and(|a| !a.is_empty()));
    crate::buttons::icon_button(
        || Icon::ArrowPath,
        || "Sync now".to_owned(),
        move || has_accounts() && !syncing.get(),
        Size::Bar,
        sidebar::sync,
    )
    .bind_rotate(&angle)
}

/// How long a turn of the sync button takes, in milliseconds.
const TURN_MS: f32 = 1000.0;

/// Stops the sync button's turning once it is upright again, rather than snapping back.
async fn finish_turn(angle: AnimatedValue) {
    angle.stop();
    let at = angle.value().await;
    if at <= 0.0 {
        return;
    }
    let left = TURN_MS * (360.0 - at) / 360.0;
    // Not if a sync started meanwhile, which turns it again.
    if angle.timing(360.0, left, Easing::Linear).await {
        angle.set(0.0);
    }
}

fn header() -> impl IntoElement {
    let title = computed(move || {
        let source = global::<Listing>().with(|l| l.source().cloned());
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
        move || global::<Listing>().with(|l| l.source().is_some_and(Source::lists_messages));
    let search = element_ref();
    global::<SearchRef>().set(SearchRef(Some(search)));
    view()
        .px(px(12.0))
        .pb(px(8.0))
        .gap(px(4.0))
        .child(
            // As tall as the toolbar across from it, and lined up with it: the row that
            // moves the window, on macOS.
            chrome::move_row()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .child(sync_button())
                .child(search_field(search)),
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

/// The search field, round as "Compose" across from it.
fn search_field(search: ElementRef) -> impl IntoElement {
    view()
        .flex_1()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .h(px(36.0))
        .px(px(12.0))
        .rounded(px(18.0))
        .border_1()
        .border_color(move || theme().input)
        .child(icon(|| Icon::MagnifyingGlass, 16.0).text_color(move || theme().muted_foreground))
        .child(
            input()
                .value(move || global::<SearchText>().with(|t| t.0.clone()))
                .placeholder("Search".to_owned())
                .on_change(move |text: &str| typed(text.to_owned()))
                .element_ref(&search)
                .flex_1()
                .text_sm()
                .text_color(move || theme().foreground),
        )
}

fn has_checked() -> bool {
    global::<Checked>().with(|c| !c.0.is_empty())
}

/// Checks every message listed, or none; its menu checks those read, flagged and so on.
fn check_all() -> impl IntoElement {
    let counts = computed(move || {
        let listed = global::<Listing>().with(|l| l.messages().count());
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
