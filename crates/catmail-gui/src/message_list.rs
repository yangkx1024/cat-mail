//! The selected folder's messages, newest first, loaded a page at a time as the list
//! scrolls. The list (`ListState`) owns selection, keyboard navigation and scrolling; the
//! delegate here owns the rows.

use catmail_core::{Flags, FolderId, MailClient, MessageSummary};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::list::{ListDelegate, ListEvent, ListItem, ListState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Icon, IndexPath, Sizable as _, WindowExt as _};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    App, Context, FontWeight, IntoElement, ParentElement as _, Styled as _, Task, Window, div,
    prelude::FluentBuilder as _,
};

use crate::format;

const PAGE: u32 = 200;

pub struct MessageList {
    client: MailClient,
    folder: Option<FolderId>,
    messages: Vec<MessageSummary>,
    selected: Option<usize>,
    /// A page is being fetched; `load_more` and `refresh` wait for it.
    loading: bool,
    /// The engine reported a change while a page was being fetched: refresh after it.
    stale: bool,
    has_more: bool,
    /// Bumped when the folder changes, so a page fetched for the previous one is dropped.
    generation: u64,
    _load: Task<()>,
}

impl MessageList {
    pub fn new(client: MailClient) -> Self {
        Self {
            client,
            folder: None,
            messages: Vec::new(),
            selected: None,
            loading: false,
            stale: false,
            has_more: false,
            generation: 0,
            _load: Task::ready(()),
        }
    }

    pub fn message(&self, row: usize) -> Option<&MessageSummary> {
        self.messages.get(row)
    }
}

/// Shows `folder` from its newest message, or nothing.
pub fn show_folder(
    list: &mut ListState<MessageList>,
    folder: Option<FolderId>,
    window: &mut Window,
    cx: &mut Context<ListState<MessageList>>,
) {
    let delegate = list.delegate_mut();
    delegate.folder = folder;
    delegate.messages.clear();
    delegate.has_more = false;
    delegate.stale = false;
    delegate.generation += 1;
    list.set_selected_index(None, window, cx);
    list.scroll_to_item(IndexPath::new(0), gpui_kit::ScrollStrategy::Top, window, cx);
    load(list, Load::Replace(PAGE), window, cx);
}

/// Re-reads the rows already shown (after the engine reported a change), keeping the
/// selected message selected. During a sync changes arrive faster than a page loads, so
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
    let shown = delegate.messages.len() as u32;
    load(list, Load::Replace(shown.max(PAGE)), window, cx);
}

enum Load {
    /// The newest `n` messages, replacing what is shown.
    Replace(u32),
    /// The next page after what is shown.
    Append,
}

fn load(
    list: &mut ListState<MessageList>,
    what: Load,
    window: &mut Window,
    cx: &mut Context<ListState<MessageList>>,
) {
    let delegate = list.delegate_mut();
    let Some(folder) = delegate.folder else {
        cx.notify();
        return;
    };
    let (limit, offset) = match what {
        Load::Replace(n) => (n, 0),
        Load::Append => (PAGE, delegate.messages.len() as u32),
    };
    let client = delegate.client.clone();
    let generation = delegate.generation;
    delegate.loading = true;
    // Replacing the task drops (cancels) a load still in flight, which only `show_folder`
    // does: the other callers wait for `loading`.
    delegate._load = cx.spawn_in(window, async move |list, cx| {
        let result = client.list_messages(folder, limit, offset).await;
        list.update_in(cx, |list, window, cx| {
            if list.delegate().generation != generation {
                return;
            }
            let delegate = list.delegate_mut();
            delegate.loading = false;
            match result {
                Ok(page) => {
                    delegate.has_more = page.len() == limit as usize;
                    if offset > 0 {
                        delegate.messages.extend(page);
                    } else {
                        let selected = delegate
                            .selected
                            .and_then(|ix| delegate.messages.get(ix))
                            .map(|m| m.id);
                        delegate.messages = page;
                        let ix = selected.and_then(|id| {
                            list.delegate().messages.iter().position(|m| m.id == id)
                        });
                        list.set_selected_index(ix.map(IndexPath::new), window, cx);
                        if selected.is_some() && ix.is_none() {
                            // The open message is gone (expunged, or a new UIDVALIDITY).
                            // Setting the index emits nothing; deselecting with Escape
                            // emits this.
                            cx.emit(ListEvent::Cancel);
                        }
                    }
                }
                Err(e) => window.push_notification(
                    Notification::error(format!("Couldn't load messages: {e}")),
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

impl ListDelegate for MessageList {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.messages.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let message = self.messages.get(ix.row)?;
        let unread = !message.flags.contains(Flags::SEEN);
        let flagged = message.flags.contains(Flags::FLAGGED);
        let sender = format::sender(&message.from);
        let subject = format::subject(message);
        let date = format::short_date(message.date);
        let muted = cx.theme().muted_foreground;
        // Every row has the same three lines: the list requires one row height.
        let snippet = message
            .snippet
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "\u{a0}".into());

        let label = format!(
            "{sender}, {subject}, {date}{}{}",
            if unread { ", unread" } else { "" },
            if message.has_attachments {
                ", has attachments"
            } else {
                ""
            },
        );
        Some(
            ListItem::new(("message", message.id.0 as u64))
                .selected(self.selected == Some(ix.row))
                .accessibility_label(label)
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .when(unread, |this| this.font_weight(FontWeight::SEMIBOLD))
                                        .child(sender),
                                )
                                .when(flagged, |this| {
                                    this.child(Icon::new(Lucide::Flag).xsmall().text_color(muted))
                                })
                                .when(message.has_attachments, |this| {
                                    this.child(
                                        Icon::new(Lucide::Paperclip).xsmall().text_color(muted),
                                    )
                                })
                                .child(div().text_xs().text_color(muted).child(date)),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .when(unread, |this| this.font_weight(FontWeight::SEMIBOLD))
                                .child(subject),
                        )
                        .child(div().truncate().text_xs().text_color(muted).child(snippet)),
                ),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let text = if self.folder.is_some() {
            "No messages"
        } else {
            "No folder selected"
        };
        h_flex()
            .size_full()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }

    fn loading(&self, _: &App) -> bool {
        // The skeleton only while nothing is shown yet; a refresh keeps the rows.
        self.loading && self.messages.is_empty()
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
