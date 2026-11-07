//! What a window has at its top. On macOS the main window has no title bar: its content
//! goes up to the top, the window's buttons over the sidebar's corner, in the row of the
//! bars at the top of the panes, which is where the window is moved from. A composer keeps
//! a title bar, but the app's, in the theme's colors. Elsewhere the system's title bar is
//! over every window.

use nsh_base::theme;
use nsh_core::desktop::{TitleBarOptions, TitleBarStyle, WindowAttributes};
use nsh_core::element::desktop::{current_window, title_bar};
use nsh_core::prelude::*;

/// How tall the bars at the top of the panes are where nothing else decides it.
const BAR_HEIGHT: f32 = 48.0;

/// How tall the main window's row of the window's buttons is asked to be on macOS: the
/// row AppKit's unified toolbar gives them, as in Mail.app.
const WINDOW_BUTTONS_ROW: f32 = 52.0;

/// How far the main window's close button is from the window's left edge on macOS: as
/// far as the sidebar's content (its 12-pt inset), so the buttons line up with Compose.
/// AppKit's own place, for a window with no toolbar, is closer to the edge.
const WINDOW_BUTTONS_LEFT: f32 = 12.0;

/// How tall the window's buttons are on macOS, to center them in their row.
const WINDOW_BUTTON_SIZE: f32 = 14.0;

/// How tall the composer's title bar is asked to be: GPUI Kit's.
const TITLE_BAR_HEIGHT: f32 = 34.0;

/// The main window has no title bar on macOS: its top bars are the row of the window's
/// buttons, which sit in its middle, lined up with the sidebar. In the view's root, once.
pub fn unified() {
    hide_title_bar(
        WINDOW_BUTTONS_ROW,
        Some(Point {
            x: Points(WINDOW_BUTTONS_LEFT),
            y: Points((WINDOW_BUTTONS_ROW - WINDOW_BUTTON_SIZE) / 2.0),
        }),
    );
}

/// On macOS, takes the system's title bar off the window being rendered for one of the
/// app's, `height` tall, with the window's buttons centered in it, or with the close
/// button's top left corner at `buttons`. Elsewhere the system draws the title bar.
fn hide_title_bar(height: f32, buttons: Option<Point<Points>>) {
    if cfg!(target_os = "macos") {
        let mut bar = TitleBarOptions::default();
        bar.style = TitleBarStyle::Hidden;
        bar.height = Some(Points(height));
        bar.controls_offset = buttons;
        let mut attributes = WindowAttributes::default();
        attributes.title_bar = Some(bar);
        current_window().set(attributes);
    }
}

/// Follows `title` with the title of the window being rendered (the window menu, Mission
/// Control and accessibility read it, also with the title bar hidden). In the view's root,
/// once.
pub fn follow_title(title: Memo<String>) {
    let window = current_window();
    effect(move || {
        let mut attributes = WindowAttributes::default();
        attributes.title = Some(title.get());
        window.set(attributes);
    });
}

/// How tall the bars at the top of the panes are, the toolbars' and the list's search
/// row, so they line up: in the main window on macOS, the row the window's buttons sit
/// in. Follows it (full screen hides the buttons).
pub fn bar_height() -> f32 {
    if has_window_buttons() {
        title_bar().top.0
    } else {
        BAR_HEIGHT
    }
}

/// Whether the window's buttons are over the top of the window, which then leaves them
/// their row: on macOS, unless the window is full screen, which hides them. Follows it.
pub fn has_window_buttons() -> bool {
    // Elsewhere the system's title bar has them, and there is nothing to follow.
    cfg!(target_os = "macos") && title_bar().top.0 > 0.0
}

/// A row as tall as the top bars, which moves the window: what leaves the window's
/// buttons their room where nothing else is in their row, and what each top bar starts
/// from.
pub fn move_row() -> View {
    view()
        .flex_shrink_0()
        .h(move || px(bar_height()))
        .window_drag(true)
}

/// The bar of commands at the top of a pane, a [`move_row`] with a line under it.
pub fn toolbar() -> View {
    move_row()
        .flex_row()
        .items_center()
        .px(px(12.0))
        .border_b_1()
        .border_color(move || theme().border)
}

/// `content` under a [`move_row`] where the window's buttons are over its top: a page
/// with no bar of its own at the top (signing in, why there is no mailbox).
pub fn page(content: impl IntoElement) -> View {
    view()
        .flex_1()
        .child(when(has_window_buttons, move_row))
        .child(content)
}

/// A composer's title bar on macOS, in the theme's colors: `title` in the middle of the
/// window, as the system's would be. Nothing elsewhere: the system draws it. In the
/// view's root, once.
pub fn title_bar_of(title: Memo<String>) -> impl IntoElement {
    hide_title_bar(TITLE_BAR_HEIGHT, None);
    when(has_window_buttons, move || {
        // As much room on the right as the buttons take on the left: the title is in the
        // middle.
        let buttons = move || px(title_bar().left.0);
        view()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(move || px(title_bar().top.0))
            .pl(buttons)
            .pr(buttons)
            .bg(move || theme().sidebar)
            .border_b_1()
            .border_color(move || theme().border)
            .window_drag(true)
            .child(
                text(move || title.get())
                    .number_of_lines(1)
                    .flex_1()
                    .text_center()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(move || theme().foreground),
            )
    })
}
