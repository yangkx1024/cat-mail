//! A dialog listing an account's folders, filtered as you type: "Move to", which picks
//! one, and on Gmail "Labels", which checks the message's labels and toggles them. The
//! checks are its owner's: picking a label only says which one was picked.

use std::collections::HashSet;

use catmail_core::FolderId;
use gpui_kit::component::list::{List, ListDelegate, ListEvent, ListItem, ListState};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IndexPath, Sizable as _, WindowExt as _, h_flex,
};
use gpui_kit::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Styled as _, Task, WeakEntity,
    Window, div, prelude::FluentBuilder as _, rems,
};

use crate::assets::Hero;
use crate::layout;

pub struct FolderPicker {
    folders: Vec<(FolderId, String)>,
    /// For "Labels", the labels the message has, as its owner last set them
    /// ([`set_checked`](Self::set_checked)). `None` for "Move to".
    checked: Option<HashSet<FolderId>>,
    shown: Vec<usize>,
    selected: Option<usize>,
}

impl FolderPicker {
    fn new(folders: Vec<(FolderId, String)>, checked: Option<HashSet<FolderId>>) -> Self {
        let shown = (0..folders.len()).collect();
        Self {
            folders,
            checked,
            shown,
            selected: None,
        }
    }

    fn folder(&self, row: usize) -> Option<FolderId> {
        self.folders.get(*self.shown.get(row)?).map(|(id, _)| *id)
    }

    /// The labels to check, as the message has them now. Returns whether that changed
    /// them.
    pub fn set_checked(&mut self, labels: HashSet<FolderId>) -> bool {
        let changed = self.checked.as_ref() != Some(&labels);
        self.checked = Some(labels);
        changed
    }

    #[cfg(test)]
    pub fn checked(&self) -> Option<&HashSet<FolderId>> {
        self.checked.as_ref()
    }
}

impl ListDelegate for FolderPicker {
    type Item = ListItem;

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        let query = query.trim().to_lowercase();
        self.shown = (0..self.folders.len())
            .filter(|&ix| self.folders[ix].1.to_lowercase().contains(&query))
            .collect();
        Task::ready(())
    }

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.shown.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let (id, label) = self.folders.get(*self.shown.get(ix.row)?)?;
        let (icon, checked) = match &self.checked {
            Some(checked) => (Hero::Tag, Some(checked.contains(id))),
            None => (Hero::Folder, None),
        };
        let mut item = ListItem::new(("folder-picker", id.0 as u64))
            .selected(self.selected == Some(ix.row))
            .rounded(cx.theme().radius)
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(
                        Icon::new(icon)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                    .when(checked == Some(true), |this| {
                        this.child(
                            Icon::new(Hero::Check)
                                .small()
                                .text_color(cx.theme().primary),
                        )
                    }),
            );
        if let Some(checked) = checked {
            let state = if checked { "checked" } else { "not checked" };
            item = item.accessibility_label(format!("{label}, {state}"));
        }
        Some(item)
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let text = match (&self.checked, self.folders.is_empty()) {
            (Some(_), true) => "No labels",
            (Some(_), false) => "No label by that name",
            (None, _) => "No folder by that name",
        };
        crate::layout::empty(text, cx)
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix.map(|ix| ix.row);
    }
}

/// Opens "Move to"; `on_pick` gets the folder chosen (with Enter or a click).
pub fn open(
    folders: Vec<(FolderId, String)>,
    window: &mut Window,
    cx: &mut App,
    on_pick: impl Fn(FolderId, &mut Window, &mut App) + 'static,
) {
    let picker = FolderPicker::new(folders, None);
    let list = cx.new(|cx| ListState::new(picker, window, cx).searchable(true));
    show(
        "Move to",
        "Folder",
        list,
        window,
        cx,
        move |list, ix, window, cx| {
            if let Some(folder) = list.read(cx).delegate().folder(ix.row) {
                window.close_dialog(cx);
                on_pick(folder, window, cx);
            }
        },
    );
}

/// Opens "Labels", the message's `labels` among them checked. Picking one (with Enter or
/// a click) gives it to `on_pick`, and the dialog stays open for more; the owner changes
/// the message, and the checks with [`FolderPicker::set_checked`] as the message changes.
/// Returns the list, which lives as long as the dialog.
pub fn open_labels(
    labels: Vec<(FolderId, String)>,
    checked: HashSet<FolderId>,
    window: &mut Window,
    cx: &mut App,
    on_pick: impl Fn(FolderId, &mut Window, &mut App) + 'static,
) -> WeakEntity<ListState<FolderPicker>> {
    let picker = FolderPicker::new(labels, Some(checked));
    let list = cx.new(|cx| ListState::new(picker, window, cx).searchable(true));
    let weak = list.downgrade();
    show(
        "Labels",
        "Label",
        list,
        window,
        cx,
        move |list, ix, window, cx| {
            if let Some(label) = list.read(cx).delegate().folder(ix.row) {
                on_pick(label, window, cx);
            }
        },
    );
    weak
}

fn show(
    title: &'static str,
    placeholder: &'static str,
    list: gpui_kit::Entity<ListState<FolderPicker>>,
    window: &mut Window,
    cx: &mut App,
    on_confirm: impl Fn(&gpui_kit::Entity<ListState<FolderPicker>>, IndexPath, &mut Window, &mut App)
    + 'static,
) {
    list.update(cx, |list, cx| list.focus(window, cx));
    let subscription = window.subscribe(&list, cx, move |list, event: &ListEvent, window, cx| {
        if let ListEvent::Confirm(ix) = event {
            on_confirm(&list, *ix, window, cx);
        }
    });
    let width = rems(26.).to_pixels(window.rem_size());
    layout::open_dialog(window, cx, move |dialog, _, _| {
        // The subscription lives as long as the dialog's builder.
        let _ = &subscription;
        dialog.title(title).w(width).centered().child(
            div()
                .h(rems(20.))
                .child(List::new(&list).search_placeholder(placeholder)),
        )
    });
}
