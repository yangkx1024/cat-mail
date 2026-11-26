//! What the list shows, which the mailbox's parts share: the rows of the place selected in
//! the sidebar ([`SelectedPlace`]) or of a search, read a page at a time and again after
//! the engine's events; the row selected, which the right pane shows, and the messages
//! checked; and the changes to messages, after which the selection moves on when the
//! selected one leaves ([`run_change`]). What a source lists, the reads and where the
//! selection goes after them are `catmail_frontend::mailbox::list`'s (`Listing`, `ListModel`);
//! here they are run and kept in nsh's signals, which the list (`list/`) draws.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use catmail_core::{
    AccountId, DraftSummary, FlagFilter, Flags, FolderId, MessageId, MessageSummary, OutboxEntry,
};
use catmail_frontend::mailbox::commands::Change;
use catmail_frontend::mailbox::list::{
    Fetch, ListModel, Listing, Loaded, Row, RowId, Rows, SEARCH_DELAY, SERVER_SEARCH_DELAY, Select,
    ServerSearch, Typed,
};
use catmail_frontend::mailbox::{Place, Source};
use nsh_core::element::ElementRef;
use nsh_core::geometry::Percentage;
use nsh_core::prelude::*;

use crate::accounts::with_accounts;
use crate::clock;
use crate::engine::client;
use crate::motion;
use crate::shell::tasks::in_view;
use crate::toast::report;

/// The place selected in the sidebar: the list shows it, unless a search is typed.
#[derive(Clone, Default)]
pub struct SelectedPlace(pub Option<Place>);

/// The row the reading pane shows. Its index changes as rows come and go; it doesn't.
#[derive(Clone, Default)]
pub struct Selected(pub Option<RowId>);

/// The messages checked, for a command on them all: apart from the selected row, the one
/// the reader shows. Always among the rows.
#[derive(Clone, Default)]
pub struct Checked(pub HashSet<MessageId>);

/// A page is being read. Apart from the [`Listing`]: the rows don't change with it.
#[derive(Clone, Copy, Default, PartialEq)]
pub struct Loading(pub bool);

#[derive(Clone, Default)]
pub struct SearchText(pub String);

/// The name of each user label shown, of every account: a row shows those of its
/// message's labels.
#[derive(Clone, Default)]
pub struct LabelNames(pub HashMap<FolderId, String>);

/// The reads of the list, and the row `CATMAIL_GUI_OPEN` opens. Not reactive.
#[derive(Default)]
struct ListState {
    model: ListModel,
    /// `CATMAIL_GUI_OPEN` opened its row already.
    opened_from_env: bool,
}

#[derive(Clone, Default)]
struct State(Rc<RefCell<ListState>>);

fn state<T>(f: impl FnOnce(&mut ListState) -> T) -> T {
    let state = global::<State>().with_untracked(|s| s.0.clone());
    f(&mut state.borrow_mut())
}

/// `f` with the reads, and the listing they are of. Untracked.
fn reads<T>(f: impl FnOnce(&mut ListModel, &Listing) -> T) -> T {
    global::<Listing>().with_untracked(|listing| state(|s| f(&mut s.model, listing)))
}

/// The list's element, which a new listing scrolls to its top. The list's view sets it.
#[derive(Clone, Default)]
pub struct ListRef(pub Option<ElementRef>);

/// Shows `source` from the top, or nothing.
pub fn show(source: Option<Source>) {
    let (listing, fetch) = state(|s| s.model.show(source, FlagFilter::All));
    shown(listing, fetch);
}

/// Shows the source shown from the top again, only the messages `filter` takes.
pub fn filter(filter: FlagFilter) {
    let (listing, fetch) = reads(|model, listing| model.show(listing.source().cloned(), filter));
    shown(listing, fetch);
}

/// `listing` is shown in place of the list's rows, with `fetch` its first read.
fn shown(listing: Listing, fetch: Option<Fetch>) {
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

pub fn load_more() {
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
        if glides(&rows) {
            motion::glide_rows();
        }
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

/// Whether `rows` glide in: the rows shown read again, with messages come or gone (new
/// mail, one moved or deleted), rather than a source's first rows. Not while the list
/// switches its shape, either (`motion::still_rows`, from the list's `follow_selection`).
fn glides(rows: &Rows) -> bool {
    matches!(rows, Rows::Replace(..)) && !global::<Listing>().with_untracked(Listing::is_empty)
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
    tracing::info!(message = %id, listed = row.is_some(), read_again = again,
        "revealing a message");
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
        run_change(catmail_frontend::mailbox::commands::mark_read(id));
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
pub fn selected_draft() -> Option<Rc<DraftSummary>> {
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

/// The selected message, as the list has it now. Tracked.
pub fn selected_message() -> Option<MessageSummary> {
    message(selected_id()?)
}

/// The messages checked, top to bottom. Tracked.
pub fn checked_messages() -> Vec<MessageSummary> {
    global::<Checked>().with(|c| global::<Listing>().with(|l| l.checked(&c.0)))
}

/// What a key acts on: the messages checked, or else the selected one.
pub fn keyed() -> Vec<MessageSummary> {
    let selected = global::<Selected>().with_untracked(|s| s.0);
    global::<Checked>()
        .with_untracked(|c| global::<Listing>().with_untracked(|l| l.keyed(&c.0, selected)))
}

/// Unchecks the messages moved or deleted: those still listed (on Gmail, one moved to a
/// label from All Mail) have been dealt with.
fn uncheck(ids: &[MessageId]) {
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
fn expect_removal(ids: &[MessageId]) -> Option<u64> {
    let selected = global::<Selected>().with_untracked(|s| s.0);
    state(|s| s.model.expect_removal(selected, ids))
}

/// The change behind `removal`, which may take the message out of the list, was made
/// locally (`made`), or failed: the rows are read again, and say whether it left.
fn removal_settled(removal: u64, made: bool) {
    if state(|s| s.model.removal_settled(removal, made)) {
        refresh();
    }
}

/// Makes `change`. If it may take the messages out of the list and the selected one goes,
/// the next one is selected (`ListModel::expect_removal`); those dealt with are no longer
/// checked.
pub fn run_change(change: Change) {
    if change.is_empty() {
        return;
    }
    if change.unchecks() {
        uncheck(&change.ids);
    }
    let removal = change
        .may_remove()
        .then(|| expect_removal(&change.ids))
        .flatten();
    in_view(async move {
        let result = change.run(&client()).await;
        if let Err(e) = &result {
            report(&change.failure(), e);
        }
        if let Some(removal) = removal {
            removal_settled(removal, result.is_ok());
        }
    });
}

/// The accounts' user labels, by the name a row shows them by.
/// Also where each account's messages are looked at ([`ShownIn`]).
pub fn set_labels() {
    let names: HashMap<_, _> = with_accounts(|accounts| {
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
pub struct ShownIn(pub HashMap<AccountId, FolderId>);

fn set_shown_in() {
    let source = source();
    let shown_in = match source {
        Some(source) => with_accounts(|a| source.shown_in_each(a)),
        None => HashMap::new(),
    };
    if global::<ShownIn>().with_untracked(|s| s.0 != shown_in) {
        global::<ShownIn>().set(ShownIn(shown_in));
    }
}

/// The search field changed: a pause in typing searches, or with nothing typed shows
/// the place selected again (`ListModel::paused`).
pub fn typed(text: String) {
    global::<SearchText>().set(SearchText(text.clone()));
    let typed = state(|s| s.model.typed());
    in_view(async move {
        timer(SEARCH_DELAY).await;
        settle(typed, &text);
    });
}

/// The search field holds `text` since `typed`: its results, or with nothing typed the
/// place selected, unless shown already or another key came since.
pub fn settle(typed: Typed, text: &str) {
    let place = global::<SelectedPlace>().with_untracked(|p| p.0);
    if let Some(source) = reads(|model, listing| model.paused(typed, text, place, listing)) {
        show(source);
    }
}

/// Leaving a search for a place: the place's rows, not the results.
pub fn clear_search() -> Typed {
    let typed = state(|s| s.model.typed());
    global::<SearchText>().set(SearchText::default());
    typed
}

pub fn toggle_checked(id: MessageId) {
    global::<Checked>().update(|c| {
        if !c.0.remove(&id) {
            c.0.insert(id);
        }
    });
}
