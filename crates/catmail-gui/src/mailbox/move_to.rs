//! "Move to": a dialog listing the account's folders, filtered as you type.

use std::rc::Rc;

use catmail_core::FolderId;
use gpui_kit::component::list::{List, ListDelegate, ListEvent, ListItem, ListState};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IndexPath, Sizable as _, WindowExt as _, h_flex,
};
use gpui_kit::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Styled as _, Task, Window, div,
    rems,
};

use crate::assets::Hero;

pub struct FolderPicker {
    folders: Vec<(FolderId, String)>,
    shown: Vec<usize>,
    selected: Option<usize>,
}

impl FolderPicker {
    fn new(folders: Vec<(FolderId, String)>) -> Self {
        let shown = (0..folders.len()).collect();
        Self {
            folders,
            shown,
            selected: None,
        }
    }

    fn folder(&self, row: usize) -> Option<FolderId> {
        self.folders.get(*self.shown.get(row)?).map(|(id, _)| *id)
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
        Some(
            ListItem::new(("move-to", id.0 as u64))
                .selected(self.selected == Some(ix.row))
                .rounded(cx.theme().radius)
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(
                            Icon::new(Hero::Folder)
                                .small()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(label.clone()),
                ),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        crate::layout::empty("No folder by that name", cx)
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

/// Opens the dialog; `on_pick` gets the folder chosen (with Enter or a click).
pub fn open(
    folders: Vec<(FolderId, String)>,
    window: &mut Window,
    cx: &mut App,
    on_pick: impl Fn(FolderId, &mut Window, &mut App) + 'static,
) {
    let list = cx.new(|cx| ListState::new(FolderPicker::new(folders), window, cx).searchable(true));
    let on_pick = Rc::new(on_pick);
    let subscription = window.subscribe(&list, cx, move |list, event: &ListEvent, window, cx| {
        if let ListEvent::Confirm(ix) = event
            && let Some(folder) = list.read(cx).delegate().folder(ix.row)
        {
            window.close_dialog(cx);
            on_pick(folder, window, cx);
        }
    });
    list.update(cx, |list, cx| list.focus(window, cx));
    let width = rems(26.).to_pixels(window.rem_size());
    window.open_dialog(cx, move |dialog, _, _| {
        // The subscription lives as long as the dialog's builder.
        let _ = &subscription;
        dialog.title("Move to").w(width).child(
            div()
                .h(rems(20.))
                .child(List::new(&list).search_placeholder("Folder")),
        )
    });
}
