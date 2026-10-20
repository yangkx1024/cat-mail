//! Layout pieces the panes and windows share, so they line up and look alike.

use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
use gpui_kit::{
    App, Div, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, div, rems,
};

use crate::assets::Hero;
use crate::format;

/// The bar of commands at the top of a pane or window; the same height whatever it holds.
pub fn toolbar(cx: &App) -> Div {
    h_flex()
        .flex_none()
        .h_12()
        .px_3()
        .gap_1()
        .border_b_1()
        .border_color(cx.theme().border)
}

/// A page of the right pane, `content` starting with its [`page_title`]: it scrolls as a
/// whole, so a long header doesn't squeeze the text, and keeps a readable line length.
/// The caller sizes it.
pub fn page(id: impl Into<ElementId>, content: Div) -> Stateful<Div> {
    div()
        .id(id)
        .overflow_y_scroll()
        .child(content.max_w(rems(48.)).px_6().pt_5().pb_8())
}

pub fn page_title(title: impl Into<SharedString>) -> Div {
    div()
        .text_xl()
        .font_weight(FontWeight::SEMIBOLD)
        .child(title.into())
}

/// A name and its value, the names lined up in a column.
pub fn labelled(name: impl IntoElement, value: impl IntoElement, cx: &App) -> Div {
    h_flex()
        .items_start()
        .gap_2()
        .child(
            div()
                .w_16()
                .flex_none()
                .text_color(cx.theme().muted_foreground)
                .child(name),
        )
        .child(div().flex_1().min_w_0().child(value))
}

/// An attachment of a draft: its name and size.
pub fn attachment_chip(filename: impl Into<SharedString>, size: u64, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .gap_1p5()
        .h_7()
        .px_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().muted)
        .text_sm()
        .child(Icon::new(Hero::PaperClip).xsmall().text_color(muted))
        .child(filename.into())
        .child(div().text_xs().text_color(muted).child(format::size(size)))
}
