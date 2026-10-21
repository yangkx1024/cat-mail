//! Layout pieces the panes and windows share, so they line up and look alike.

use std::rc::Rc;

use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::{
    App, Bounds, Div, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, Size, Stateful, StatefulInteractiveElement as _,
    Styled as _, TitlebarOptions, Window, WindowBounds, WindowOptions, div, rems,
};

use crate::assets::Hero;
use crate::format;

/// A window centered on the screen. Its geometry is in device-independent pixels, not
/// the rem scale.
pub fn window_options(
    title: impl Into<SharedString>,
    size: Size<Pixels>,
    min_size: Size<Pixels>,
    cx: &App,
) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
        window_min_size: Some(min_size),
        ..Default::default()
    }
}

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

/// What a pane or list shows when it has nothing to show: a quiet line in its middle.
pub fn empty(text: impl Into<SharedString>, cx: &App) -> Div {
    h_flex()
        .size_full()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
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

/// A short status beside a name, on the background and in the text color given.
pub fn badge(text: impl Into<SharedString>, (bg, fg): (Hsla, Hsla), cx: &App) -> Div {
    div()
        .px_1p5()
        .rounded(cx.theme().radius)
        .text_xs()
        .bg(bg)
        .text_color(fg)
        .child(text.into())
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

/// The element id of the button that confirms [`confirm_danger`]: GPUI Kit's, for its
/// alert dialog's OK.
#[cfg(test)]
pub const CONFIRM: &str = "ok";

/// Asks before something that can't be undone: `title` asks the question, `description`
/// says what follows, and the red `ok` button does it with `on_ok`.
pub fn confirm_danger(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    ok: &'static str,
    window: &mut Window,
    cx: &mut App,
    on_ok: impl Fn(&mut Window, &mut App) + 'static,
) {
    let (title, description) = (title.into(), description.into());
    let on_ok = Rc::new(on_ok);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let on_ok = on_ok.clone();
        alert
            .title(title.clone())
            .description(description.clone())
            .confirm()
            .ok_text(ok)
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                on_ok(window, cx);
                true
            })
    });
}
