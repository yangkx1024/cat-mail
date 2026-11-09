//! The middle pane: what the selected place lists, or search results, newest first under a
//! heading for each day; a row on one line while nothing is open, the subject under the
//! sender while the reading pane is; the messages checked for a command on them all. What a
//! source lists, the days and lines, the reads and where the selection goes after them, and
//! what a row says are `catmail_frontend::list`'s (`Listing`, `ListModel`); here they are
//! read, kept in nsh's signals and drawn.

mod rows;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use catmail_core::{AccountId, Draft, Flags, FolderId, MessageId, MessageSummary, OutboxEntry};
use catmail_frontend::commands::MessageCommand;
use catmail_frontend::composer::ComposeFrom;
use catmail_frontend::list::{
    self, Check, Fetch, ListModel, Listing, Loaded, Row, RowId, Rows, SEARCH_DELAY,
    SERVER_SEARCH_DELAY, Select, ServerSearch,
};
use catmail_frontend::mailbox::{self, Source};
use nsh_base::theme;
use nsh_core::element::desktop::{MenuItem, popup_menu};
use nsh_core::element::{ElementRef, input};
use nsh_core::geometry::Percentage;
use nsh_core::prelude::*;

use crate::buttons::Size;
use crate::chrome;
use crate::clock;
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

/// The reads of the list, and the row `CATMAIL_GUI_OPEN` opens. Not reactive.
#[derive(Default)]
struct ListState {
    model: ListModel,
    /// `CATMAIL_GUI_OPEN` opened its row already.
    opened_from_env: bool,
}

#[derive(Clone, Default)]
struct State(Rc<RefCell<ListState>>);

/// `f` with the list's state.
fn state<T>(f: impl FnOnce(&mut ListState) -> T) -> T {
    let state = global::<State>().with_untracked(|s| s.0.clone());
    f(&mut state.borrow_mut())
}

/// `f` with the reads, and the listing they are of. Untracked.
fn reads<T>(f: impl FnOnce(&mut ListModel, &Listing) -> T) -> T {
    global::<Listing>().with_untracked(|listing| state(|s| f(&mut s.model, listing)))
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
    let (listing, fetch) = state(|s| s.model.show(source));
    if let Some(search) = state(|s| s.model.server_search(&listing)) {
        search_server(search);
    }
    global::<Listing>().set(listing);
    set_shown_in();
    global::<Selected>().set(Selected(None));
    global::<Checked>().set(Checked::default());
    if let Some(list) = global::<ListRef>().with_untracked(|r| r.0) {
        list.scroll_to_index(0, Percentage(0.0), px(0.0), false);
    }
    read(fetch);
}

/// Asks the servers once the local results showed for [`SERVER_SEARCH_DELAY`], and reads
/// the results again with what they found, if anything. Failures leave the local results
/// alone; they are only logged.
fn search_server(search: ServerSearch) {
    in_view(async move {
        timer(SERVER_SEARCH_DELAY).await;
        if !state(|s| s.model.is_shown(&search)) {
            return;
        }
        // The engine logs what each server said; this is what kept it from asking.
        let found = match search.run(&client()).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(error = %e, "couldn't search the servers");
                return;
            }
        };
        read(reads(|model, listing| {
            model.found_on_server(&search, found, listing)
        }));
    });
}

/// Reads the rows shown again (after the engine reported a change), keeping the selected
/// row selected; one more read follows one running (`ListModel::refresh`).
pub fn refresh() {
    let fetch = reads(|model, listing| model.refresh(listing));
    read(fetch);
}

fn load_more() {
    let fetch = reads(|model, listing| model.more(listing));
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
            reads(|model, listing| model.loaded(&fetch, rows, listing, selected, &checked.0))
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
    let reading = Loading(state(|s| s.model.is_reading()));
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
        let calendar = untracked(clock::calendar);
        global::<Listing>().update(|listing| rows.apply(listing, &calendar));
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
    if state(|s| std::mem::replace(&mut s.opened_from_env, true)) {
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
    let (row, again) = reads(|model, listing| model.reveal(id, listing));
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
    state(|s| s.model.opened());
    global::<Selected>().set(Selected(Some(row)));
    let RowId::Message(id) = row else {
        return;
    };
    if crate::shell::settings().mark_read_on_open
        && message(id).is_some_and(|m| !m.flags.contains(Flags::SEEN))
    {
        commands::run_change(catmail_frontend::commands::mark_read(id));
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
    state(|s| s.model.expect_removal(selected, ids))
}

/// The change behind `removal`, which may take the message out of the list, was made
/// locally (`made`), or failed: the rows are read again, and say whether it left.
pub fn removal_settled(removal: u64, made: bool) {
    if state(|s| s.model.removal_settled(removal, made)) {
        refresh();
    }
}

/// The accounts' user labels, by the name a row shows them by.
/// Also where each account's messages are looked at ([`ShownIn`]).
pub fn set_labels() {
    let names: HashMap<_, _> = sidebar::with_accounts(|accounts| {
        let labels = accounts
            .iter()
            .flat_map(|entry| entry.folders.user_labels());
        labels.collect()
    });
    if global::<LabelNames>().with_untracked(|l| l.0 != names) {
        global::<LabelNames>().set(LabelNames(names));
    }
    set_shown_in();
}

/// Where the rows' messages are looked at, by account (`Source::shown_in_each`): the
/// label a row needn't name. Worked out once for the source and the accounts, not by
/// each row.
#[derive(Clone, Default, PartialEq)]
struct ShownIn(HashMap<AccountId, FolderId>);

fn set_shown_in() {
    let source = source();
    let shown_in = match source {
        Some(source) => sidebar::with_accounts(|a| source.shown_in_each(a)),
        None => HashMap::new(),
    };
    if global::<ShownIn>().with_untracked(|s| s.0 != shown_in) {
        global::<ShownIn>().set(ShownIn(shown_in));
    }
}

// --- Searching ----------------------------------------------------------------------

/// The search field changed: a pause in typing searches, or with nothing typed shows
/// the place selected again (`ListModel::paused`).
fn typed(text: String) {
    global::<SearchText>().set(SearchText(text.clone()));
    let typed = state(|s| s.model.typed());
    in_view(async move {
        timer(SEARCH_DELAY).await;
        let place = global::<SelectedPlace>().with_untracked(|p| p.0);
        if let Some(source) = reads(|model, listing| model.paused(typed, &text, place, listing)) {
            show(source);
        }
    });
}

/// Leaving a search for a place: the place's rows, not the results.
pub fn clear_search() {
    state(|s| s.model.typed());
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
    follow_the_calendar();
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

/// Puts the rows under the days of the user's zone again when it changes, and when a day
/// is over there: what was Today is Yesterday.
fn follow_the_calendar() {
    effect(move || {
        let calendar = clock::calendar();
        if !global::<Listing>().with_untracked(|l| l.grouped_by(&calendar)) {
            global::<Listing>().update(|l| l.regroup(&calendar));
        }
    });
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
    crate::buttons::icon_button(
        || Icon::ArrowPath,
        || "Sync now".to_owned(),
        move || sidebar::has_accounts() && !syncing.get(),
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
            let accounts = a.list();
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
