//! What a window has at its top, as the GUI's (`layout::Frame`). On macOS the main window
//! has no title bar: its content goes up to the top, the window's buttons over the
//! sidebar's corner, in the row of the bars at the top of the panes, which is where the
//! window is moved from. A composer keeps a title bar, but the app's, in the theme's
//! colors. Elsewhere the system's title bar is over every window.

use nsh_base::theme;
use nsh_core::desktop::TitleBarStyle;
use nsh_core::element::desktop::{set_title_bar, title_bar};
use nsh_core::prelude::*;

/// How tall the bars at the top of the panes are where nothing else decides it.
const BAR_HEIGHT: f32 = 48.0;

/// How tall the row of the window's buttons is on macOS: AppKit's unified toolbar's,
/// which [`unified`] asks for and the buttons are centered in. nsh's [`title_bar`] says
/// more than that on macOS 27 (the view around the buttons is taller than their row,
/// longbridge/nsh#1068), so only whether there is one is read from it.
const WINDOW_BUTTONS_ROW: f32 = 52.0;

/// How tall the composer's title bar is asked to be: GPUI Kit's.
const TITLE_BAR_HEIGHT: f32 = 34.0;

/// The main window has no title bar on macOS: its top bars are the row of the window's
/// buttons, which AppKit centers in it. In the view's root, once.
pub fn unified() {
    if cfg!(target_os = "macos") {
        set_title_bar(TitleBarStyle::Hidden, Some(Points(BAR_HEIGHT)));
    }
}

/// How tall the bars at the top of the panes are, the toolbars' and the list's search
/// row, so they line up: in the main window on macOS, the row the window's buttons sit
/// in. Follows it (full screen hides the buttons).
pub fn bar_height() -> f32 {
    if has_window_buttons() {
        WINDOW_BUTTONS_ROW
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

/// The bar of commands at the top of a pane, as the GUI's `layout::toolbar`: a
/// [`move_row`] with a line under it.
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
    if cfg!(target_os = "macos") {
        set_title_bar(TitleBarStyle::Hidden, Some(Points(TITLE_BAR_HEIGHT)));
    }
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
