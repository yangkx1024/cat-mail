//! Layout pieces the panes and windows share, so they line up and look alike.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{Cancel, Confirm, DialogFooter};
use gpui_kit::component::sidebar::SidebarMenuItem;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::{
    App, Bounds, Div, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, Size, Stateful, StatefulInteractiveElement as _,
    StyleRefinement, Styled as _, TitlebarOptions, Window, WindowBounds, WindowOptions, div,
};

use crate::actions::DIALOG_BUTTON;
use crate::assets::Hero;
use crate::format;
use crate::theme::colors::ThemeColors as _;

/// A row of a sidebar menu, the mailbox's or the settings'. Selected, its label and its
/// icon, filled, take the primary color, and the label keeps its weight: GPUI Kit sets a
/// selected row in medium, which widens it.
pub fn sidebar_row(
    label: impl Into<SharedString>,
    icon: Hero,
    selected: bool,
    cx: &App,
) -> SidebarMenuItem {
    let item = SidebarMenuItem::new(label).active(selected);
    if !selected {
        return item.icon(Icon::new(icon));
    }
    let primary = cx.theme().primary;
    item.icon(Icon::new(icon.solid()).text_color(primary))
        .label_style(
            StyleRefinement::default()
                .font_weight(FontWeight::NORMAL)
                .text_color(primary),
        )
}

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
/// whole, so a long header doesn't squeeze the text, and spans the pane, whose width the
/// user sets. The caller sizes it.
pub fn page(id: impl Into<ElementId>, content: Div) -> Stateful<Div> {
    div()
        .id(id)
        .overflow_y_scroll()
        .child(content.px_6().pt_5().pb_8())
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

/// A sender's avatar: the first letter of `name`, on the color `email` picks, so a
/// sender keeps theirs wherever it shows. The caller sizes it.
pub fn avatar(name: &str, email: &str, cx: &App) -> Div {
    let (bg, fg) = cx.theme().avatar(email);
    let initial = name.chars().take(1).collect::<String>().to_uppercase();
    div()
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .text_color(fg)
        .font_weight(FontWeight::SEMIBOLD)
        .child(initial)
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

/// The element ids of the buttons that confirm and cancel [`confirm_danger`].
pub const CONFIRM: &str = "confirm-ok";
pub const CANCEL: &str = "confirm-cancel";

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
        // Buttons of our own, not GPUI Kit's: in the `DIALOG_BUTTON` context, Enter on a
        // focused Cancel cancels instead of being the dialog's Confirm.
        let cancel = Button::new(CANCEL)
            .label("Cancel")
            .key_context(DIALOG_BUTTON)
            .on_click(|_, window, cx| window.dispatch_action(Box::new(Cancel), cx));
        let confirm = Button::new(CONFIRM)
            .label(ok)
            .danger()
            .key_context(DIALOG_BUTTON)
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(Confirm { secondary: false }), cx)
            });
        alert
            .title(title.clone())
            .centered()
            .description(description.clone())
            .footer(DialogFooter::new().child(cancel).child(confirm))
            .on_ok(move |_, window, cx| {
                on_ok(window, cx);
                true
            })
    });
}
