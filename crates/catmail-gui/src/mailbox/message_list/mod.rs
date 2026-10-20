//! The middle pane: a folder's messages (loaded a page at a time as the list scrolls),
//! search results, the drafts kept here, or what is waiting in the outbox. The list
//! (`ListState`) owns selection, keyboard navigation and scrolling; the delegate here
//! owns the rows.

use std::rc::Rc;

use catmail_core::{
    AccountId, Draft, DraftId, FolderId, MailClient, MessageId, MessageSummary, OutboxEntry,
    OutboxId, SearchQuery,
};
use gpui_kit::component::list::{ListDelegate, ListEvent, ListItem, ListState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _};
use gpui_kit::{
    App, Context, IntoElement, ParentElement as _, Styled as _, Task, Window,
    prelude::FluentBuilder as _,
};

use crate::theme::colors::ThemeColors as _;

mod rows;

pub use rows::outbox_status;
use rows::{draft_row, message_row, outbox_row};

const PAGE: u32 = 200;
/// Search results shown; a search narrow enough to be useful finds fewer.
const SEARCH_LIMIT: u32 = 500;

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
}

pub struct MessageList {
    client: MailClient,
    source: Option<Source>,
    rows: Vec<Row>,
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
            selected: None,
            loading: false,
            stale: false,
            has_more: false,
            generation: 0,
            removing: None,
            _load: Task::ready(()),
        }
    }

    pub fn row(&self, ix: usize) -> Option<&Row> {
        self.rows.get(ix)
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

    #[cfg(test)]
    pub fn message(&self, ix: usize) -> Option<&MessageSummary> {
        self.rows.get(ix)?.message()
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
            query.limit = SEARCH_LIMIT;
            let found = client.search(query).await?;
            (found.into_iter().map(Row::Message).collect(), false)
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
                        delegate.rows.extend(rows);
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
    let selected = delegate.selected_row().map(Row::id);
    // Only while the message is still the one selected: the user may have moved on.
    let removing = delegate
        .removing
        .take()
        .filter(|id| selected == Some(RowId::Message(*id)));
    let removed_at = removing.and(delegate.selected);
    delegate.rows = rows;
    let ix = selected.and_then(|id| delegate.rows.iter().position(|r| r.id() == id));
    match (selected, ix, removed_at) {
        (Some(_), None, Some(at)) if !delegate.rows.is_empty() => {
            // The user moved or deleted it: on to the next one, as the list stands now.
            let next = IndexPath::new(at.min(delegate.rows.len() - 1));
            list.set_selected_index(Some(next), window, cx);
            cx.emit(ListEvent::Select(next));
        }
        (Some(_), None, _) => {
            // The open row is gone (expunged, sent, or a new UIDVALIDITY). Setting the
            // index emits nothing; deselecting with Escape emits this.
            list.set_selected_index(None, window, cx);
            cx.emit(ListEvent::Cancel);
        }
        (_, ix, _) => {
            list.set_selected_index(ix.map(IndexPath::new), window, cx);
            // Still there (a page read before the change committed, or the change
            // failed): keep waiting.
            list.delegate_mut().removing = removing;
        }
    }
}

impl ListDelegate for MessageList {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.rows.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let row = self.rows.get(ix.row)?;
        let selected = self.selected == Some(ix.row);
        let item = match row {
            Row::Message(message) => message_row(message, cx),
            Row::Draft(draft) => draft_row(draft, cx),
            Row::Outbox(entry) => outbox_row(entry, cx),
        };
        Some(item.selected(selected).rounded(cx.theme().radius_lg).px_2())
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
        self.selected = ix.map(|ix| ix.row);
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
