//! The middle pane: a folder's messages, or every account's INBOX together (loaded a
//! page at a time as the list scrolls), search results, the drafts kept here, or what is waiting in the outbox, newest first
//! under a heading for each day. The list (`ListState`) owns selection, keyboard
//! navigation and scrolling; the delegate here owns the rows, and the messages checked
//! for a command on them all.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use catmail_core::{
    AccountId, FolderId, MailClient, MessageCursor, MessageId, MessageSummary, SearchQuery,
};
use catmail_frontend::list::{self, Days, PAGE, Reselect, SERVER_SEARCH_DELAY, fetch, reselect};
pub use catmail_frontend::list::{Check, Row, RowId};
pub use catmail_frontend::mailbox::Source;
use gpui_kit::component::list::{ListDelegate, ListEvent, ListItem, ListState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _};
use gpui_kit::{
    App, Context, EventEmitter, FontWeight, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, prelude::FluentBuilder as _,
};

use super::folders::AccountFolders;
use super::message_commands::{Applies, MessageCommand};
use crate::theme::colors::ThemeColors as _;

mod rows;

use rows::{Shape, draft_row, message_row, outbox_row};

/// A command on a row's message from the row itself, not the selection: the mailbox
/// runs it.
#[derive(Debug, Clone)]
pub struct RowCommand {
    pub message: MessageSummary,
    pub command: MessageCommand,
}

impl EventEmitter<RowCommand> for ListState<MessageList> {}

/// The list's index of row `flat`, counted top to bottom.
fn path(days: &Days, flat: usize) -> Option<IndexPath> {
    let (section, row) = days.path(flat)?;
    Some(IndexPath::new(row).section(section))
}

/// Row `ix` of the list, counted top to bottom.
fn flat(days: &Days, ix: IndexPath) -> Option<usize> {
    days.flat(ix.section, ix.row)
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
    /// Removals asked for so far: the latest is `removing`'s. An earlier one settling
    /// says nothing about it (the user may archive, then delete, the same message).
    removals: u64,
    /// Loads of the rows shown started so far (not of more pages).
    reads: u64,
    /// The message to select once the rows have it (see [`reveal`]), and the first read
    /// that must have it: one in flight may have started before it arrived.
    revealing: Option<(MessageId, u64)>,
    /// The change behind removal number `.0` is made: the rows of the read numbered `.1`
    /// or later show it, and if the message is among them it stays (see
    /// [`removal_settled`]).
    settled: Option<(u64, u64)>,
    /// The name of each user label shown, of every account, from `folders`: a row shows
    /// those of its message's labels (see `AccountFolders::user_labels`).
    labels: HashMap<FolderId, SharedString>,
    /// Each account's folders, which say what the commands on a row's message do.
    folders: HashMap<AccountId, AccountFolders>,
    /// The message row the pointer is on.
    hovered: Option<MessageId>,
    /// The messages checked, for a command on them all: apart from the selected row, the
    /// one the reader shows. Always among the rows.
    checked: HashSet<MessageId>,
    /// Search results the servers found that local search can't, by their text: read with
    /// every page of the results (see `MailClient::search_server`).
    found_on_server: Rc<[MessageId]>,
    _load: Task<()>,
    /// Asks the servers about the search shown.
    _server: Task<()>,
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
            removals: 0,
            reads: 0,
            revealing: None,
            settled: None,
            labels: HashMap::new(),
            folders: HashMap::new(),
            hovered: None,
            checked: HashSet::new(),
            found_on_server: Rc::new([]),
            _load: Task::ready(()),
            _server: Task::ready(()),
        }
    }

    pub fn row_at(&self, ix: IndexPath) -> Option<&Row> {
        self.rows.get(flat(&self.days, ix)?)
    }

    pub fn source(&self) -> Option<&Source> {
        self.source.as_ref()
    }

    /// The folder whose messages are listed; not search results.
    pub fn shown_folder(&self) -> Option<FolderId> {
        match self.source {
            Some(Source::Folder(id)) => Some(id),
            _ => None,
        }
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected?)
    }

    pub fn selected_message(&self) -> Option<&MessageSummary> {
        self.selected_row()?.message()
    }

    /// The messages checked, top to bottom.
    pub fn checked_messages(&self) -> impl Iterator<Item = &MessageSummary> + Clone {
        self.messages().filter(|m| self.checked.contains(&m.id))
    }

    pub fn has_checked(&self) -> bool {
        !self.checked.is_empty()
    }

    /// Whether every message listed is checked, and there is one.
    pub fn all_checked(&self) -> bool {
        !self.checked.is_empty() && self.messages().count() == self.checked.len()
    }

    pub fn has_messages(&self) -> bool {
        self.messages().next().is_some()
    }

    fn messages(&self) -> impl Iterator<Item = &MessageSummary> + Clone {
        self.rows.iter().filter_map(Row::message)
    }

    #[cfg(test)]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Whether it waits for a message the user moved to leave, to select the next one.
    #[cfg(test)]
    pub fn waits_for_removal(&self) -> bool {
        self.removing.is_some()
    }

    #[cfg(test)]
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// The accounts' folders, as the sidebar has them now. Returns whether they changed,
    /// and the rows need drawing again.
    pub fn set_folders(&mut self, folders: HashMap<AccountId, AccountFolders>) -> bool {
        if self.folders == folders {
            return false;
        }
        self.labels = folders
            .values()
            .flat_map(AccountFolders::user_labels)
            .map(|(id, name)| (id, name.into()))
            .collect();
        self.folders = folders;
        true
    }

    /// The names of the message's user labels but `except`, by name.
    pub fn label_names(
        &self,
        message: &MessageSummary,
        except: Option<FolderId>,
    ) -> Vec<SharedString> {
        list::label_names(&self.labels, message, except)
    }

    /// The reading pane, open while a row is selected, leaves the rows less room.
    fn shape(&self) -> Shape {
        if self.selected_row().is_some() {
            Shape::Stacked
        } else {
            Shape::Line
        }
    }

    /// Drops the message [`reveal`] waits for: the user chose another meanwhile.
    pub fn forget_reveal(&mut self) {
        self.revealing = None;
    }

    /// Selects the next row once `id`, the selected message, goes away. Returns the
    /// removal's number, for [`removal_settled`].
    pub fn expect_removal(&mut self, id: MessageId) -> u64 {
        self.removing = Some(id);
        self.removals += 1;
        self.settled = None;
        self.removals
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
    delegate.revealing = None;
    delegate.settled = None;
    delegate.generation += 1;
    delegate.found_on_server = Rc::new([]);
    delegate._server = match &delegate.source {
        Some(Source::Search(text)) => search_server(delegate, text.clone(), window, cx),
        _ => Task::ready(()),
    };
    uncheck_all(list, cx);
    list.set_selected_index(None, window, cx);
    list.scroll_to_item(IndexPath::new(0), gpui_kit::ScrollStrategy::Top, window, cx);
    load(list, Load::Replace(PAGE), window, cx);
}

/// Asks the servers about `text` once the local results showed for
/// [`SERVER_SEARCH_DELAY`], and reads the results again with what they found, if
/// anything. Failures leave the local results alone; they are only logged.
fn search_server(
    delegate: &MessageList,
    text: String,
    window: &mut Window,
    cx: &mut Context<ListState<MessageList>>,
) -> Task<()> {
    let (client, generation) = (delegate.client.clone(), delegate.generation);
    cx.spawn_in(window, async move |list, cx| {
        cx.background_executor().timer(SERVER_SEARCH_DELAY).await;
        // The engine logs what each server said; this is what kept it from asking.
        let found = match client.search_server(SearchQuery::new(text)).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(error = %e, "couldn't search the servers");
                return;
            }
        };
        if found.is_empty() {
            return;
        }
        list.update_in(cx, |list, window, cx| {
            let delegate = list.delegate_mut();
            if delegate.generation != generation {
                return;
            }
            delegate.found_on_server = found.into();
            refresh(list, window, cx);
        })
        .ok();
    })
}

/// Checks message `id`, or unchecks it if it is checked.
pub fn toggle_checked(
    list: &mut ListState<MessageList>,
    id: MessageId,
    cx: &mut Context<ListState<MessageList>>,
) {
    let checked = &mut list.delegate_mut().checked;
    if !checked.remove(&id) {
        checked.insert(id);
    }
    cx.notify();
}

/// Checks the messages listed that `which` takes, and only those: of a folder, those of
/// the pages loaded.
pub fn check(
    list: &mut ListState<MessageList>,
    which: Check,
    cx: &mut Context<ListState<MessageList>>,
) {
    let checked = list
        .delegate()
        .messages()
        .filter(|m| which.takes(m.flags))
        .map(|m| m.id)
        .collect();
    set_checked(list, checked, cx);
}

pub fn uncheck_all(list: &mut ListState<MessageList>, cx: &mut Context<ListState<MessageList>>) {
    set_checked(list, HashSet::new(), cx);
}

pub fn uncheck(
    list: &mut ListState<MessageList>,
    ids: &[MessageId],
    cx: &mut Context<ListState<MessageList>>,
) {
    let checked = &mut list.delegate_mut().checked;
    let before = checked.len();
    checked.retain(|id| !ids.contains(id));
    if checked.len() != before {
        cx.notify();
    }
}

fn set_checked(
    list: &mut ListState<MessageList>,
    checked: HashSet<MessageId>,
    cx: &mut Context<ListState<MessageList>>,
) {
    let delegate = list.delegate_mut();
    if delegate.checked != checked {
        delegate.checked = checked;
        cx.notify();
    }
}

/// Selects and opens message `id`, of the source shown: now if it is listed, or else once
/// the rows read from now on have it (it may have just arrived). Nothing happens if the
/// next of them doesn't, if another source is shown first, or if the user selects a row
/// meanwhile ([`MessageList::forget_reveal`]).
pub fn reveal(
    list: &mut ListState<MessageList>,
    id: MessageId,
    window: &mut Window,
    cx: &mut Context<ListState<MessageList>>,
) {
    let delegate = list.delegate_mut();
    // The next read starts after now, once one in flight is done.
    let reads = delegate.reads;
    delegate.revealing = Some((id, reads + 1));
    select_revealed(list, reads, window, cx);
    if list.delegate().revealing.is_some() {
        refresh(list, window, cx);
    }
}

/// Selects the message [`reveal`] asked for if it is among the rows, which read number
/// `read` brought; gives up if they don't have it and that read should have.
fn select_revealed(
    list: &mut ListState<MessageList>,
    read: u64,
    window: &mut Window,
    cx: &mut Context<ListState<MessageList>>,
) {
    let delegate = list.delegate_mut();
    let Some((id, from)) = delegate.revealing else {
        return;
    };
    let ix = delegate
        .rows
        .iter()
        .position(|r| r.id() == RowId::Message(id))
        .and_then(|ix| path(&delegate.days, ix));
    if ix.is_none() && read < from {
        return;
    }
    delegate.revealing = None;
    let Some(ix) = ix else {
        return;
    };
    list.set_selected_index(Some(ix), window, cx);
    list.scroll_to_item(ix, gpui_kit::ScrollStrategy::Center, window, cx);
    cx.emit(ListEvent::Select(ix));
}

/// The change behind `removal` ([`MessageList::expect_removal`]), which may take the
/// message out of the list, was made locally (`made`), or failed. Whether it leaves the
/// list is the store's to say (on Gmail, a message moved to a label stays in All Mail):
/// the rows are read again, and if they still have it, it stays selected and is no
/// longer waited for. If it failed it stays at once. Nothing happens if a later removal
/// has been asked for since.
pub fn removal_settled(
    list: &mut ListState<MessageList>,
    removal: u64,
    made: bool,
    window: &mut Window,
    cx: &mut Context<ListState<MessageList>>,
) {
    let delegate = list.delegate_mut();
    if delegate.removals != removal || delegate.removing.is_none() {
        return; // A later one, gone already, or the user moved on.
    }
    if !made {
        delegate.removing = None;
        return;
    }
    // A read in flight may have started before the change: the next one shows it.
    delegate.settled = Some((removal, delegate.reads + 1));
    refresh(list, window, cx);
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
    /// The next page, after the last row shown.
    After(MessageCursor),
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
    let (limit, after) = match what {
        Load::Replace(n) => (n, None),
        Load::After(cursor) => (PAGE, Some(cursor)),
    };
    let append = after.is_some();
    if !append {
        delegate.reads += 1;
    }
    let read = delegate.reads;
    let client = delegate.client.clone();
    let found_on_server = delegate.found_on_server.clone();
    let generation = delegate.generation;
    delegate.loading = true;
    // Replacing the task drops (cancels) a load still in flight, which only `show` does:
    // the other callers wait for `loading`.
    delegate._load = cx.spawn_in(window, async move |list, cx| {
        let result = fetch(&client, &source, &found_on_server, limit, after).await;
        list.update_in(cx, |list, window, cx| {
            if list.delegate().generation != generation {
                return;
            }
            let delegate = list.delegate_mut();
            delegate.loading = false;
            // Whether anything shown changed: the skeleton shows while nothing is listed.
            let mut changed = true;
            match result {
                Ok((rows, more)) => {
                    if !append
                        && let Some((removal, from)) = delegate.settled
                        && read >= from
                    {
                        delegate.settled = None;
                        // `expect_removal` drops a settlement of an earlier removal.
                        debug_assert_eq!(removal, delegate.removals);
                        if let Some(id) = delegate.removing
                            && rows.iter().any(|r| r.id() == RowId::Message(id))
                        {
                            delegate.removing = None;
                        }
                    }
                    let had_more = std::mem::replace(&mut delegate.has_more, more);
                    if append {
                        // The page starts after the last row shown, whatever came or went
                        // above it since; the refresh such a change brings updates those.
                        let from = delegate.rows.len();
                        delegate.rows.extend(rows);
                        delegate.days.extend(&delegate.rows, from);
                    } else if !rows.is_empty() && rows == delegate.rows && more == had_more {
                        // A sync's events often bring nothing this list shows. The
                        // selection stays on the same row, and a removal waited for
                        // still is.
                        changed = false;
                    } else {
                        replace_rows(list, rows, window, cx);
                    }
                    if !append {
                        select_revealed(list, read, window, cx);
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
            if changed {
                cx.notify();
            }
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
    let next = reselect(selected, delegate.removing.take(), &delegate.rows, &rows);
    delegate.days = Days::new(&rows);
    delegate.rows = rows;
    // Those gone are no longer checked: moved, deleted, or expunged on the server.
    let delegate = list.delegate();
    if !delegate.checked.is_empty() {
        let listed: HashSet<_> = delegate.messages().map(|m| m.id).collect();
        let checked = delegate.checked.intersection(&listed).copied().collect();
        set_checked(list, checked, cx);
    }
    if let Reselect::Keep { ix, removing } = next {
        let ix = ix.and_then(|ix| path(&list.delegate().days, ix));
        list.set_selected_index(ix, window, cx);
        list.delegate_mut().removing = removing;
        return;
    }
    let next = match next {
        // Every row has a path; if one hadn't, it would be as good as gone.
        Reselect::Next(ix) => path(&list.delegate().days, ix),
        _ => None,
    };
    list.set_selected_index(next, window, cx);
    match next {
        Some(next) => cx.emit(ListEvent::Select(next)),
        // Setting the index emits nothing; deselecting with Escape emits this.
        None => cx.emit(ListEvent::Cancel),
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
        let label = SharedString::from(self.days.get(section)?.label.clone());
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
        let ix = flat(&self.days, ix)?;
        let row = self.rows.get(ix)?;
        let selected = self.selected == Some(ix);
        let shape = self.shape();
        let item = match row {
            // Not the label listed: every row has it.
            Row::Message(message) => {
                let id = message.id;
                message_row(
                    message,
                    &self.label_names(message, self.shown_folder()),
                    shape,
                    self.checked.contains(&id),
                    // The commands show on one line, while hovered.
                    (self.hovered == Some(id) && shape == Shape::Line)
                        .then(|| Applies::of([message], |account| self.folders.get(&account))),
                    cx,
                )
                .on_hover(cx.listener(move |list, &hovered: &bool, _, cx| {
                    let delegate = list.delegate_mut();
                    let before = delegate.hovered;
                    if hovered {
                        delegate.hovered = Some(id);
                    } else if before == Some(id) {
                        delegate.hovered = None;
                    }
                    if delegate.hovered != before {
                        cx.notify();
                    }
                }))
            }
            Row::Draft(draft) => draft_row(draft, shape, cx),
            Row::Outbox(entry) => outbox_row(entry, cx),
        };
        // The virtual list has no gap between rows; the margin keeps a selected row's
        // background apart from a hovered neighbour's. The padding is the row's own.
        Some(
            item.selected(selected)
                .rounded(cx.theme().radius_lg)
                .my_0p5(),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        crate::layout::empty(list::empty_text(self.source.as_ref()), cx)
    }

    fn loading(&self, _: &App) -> bool {
        // The skeleton only while nothing is shown yet; a refresh keeps the rows.
        self.loading && self.rows.is_empty()
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        let (shape, before) = (self.shape(), self.selected);
        self.selected = ix.and_then(|ix| flat(&self.days, ix));
        if self.shape() == shape {
            return;
        }
        // Every row changes height: the one opened, or the one just closed, would move
        // away from under the pointer. (Not when the rows went with it: another source.)
        let row = self.selected.or(before.filter(|&ix| ix < self.rows.len()));
        if let Some(path) = row.and_then(|ix| path(&self.days, ix)) {
            // The list is being updated right now.
            cx.defer_in(window, move |list, window, cx| {
                list.scroll_to_item(path, gpui_kit::ScrollStrategy::Center, window, cx)
            });
        }
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
            let delegate = list.delegate();
            // Only messages have more pages: of folders, the inboxes and search results.
            let last = delegate.rows.last().and_then(Row::message);
            if let Some(last) = last.filter(|_| !delegate.loading) {
                load(list, Load::After(last.cursor()), window, cx);
            }
        });
    }
}
