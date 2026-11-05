//! The main window on macOS, which has no title bar: it moves from the bars at its top
//! (`layout::movable`). The test platform doesn't move windows, and panics when asked
//! to: that panic is how a test sees the window would have moved.

use gpui_kit::{Pixels, Point};

use super::*;
/// Presses at `from` and moves the pointer away, as a person dragging does.
fn drag_from(cx: &mut TestAppContext, window: AnyWindowHandle, from: Point<Pixels>) {
    cx.update_window(window, |_, window, cx| {
        window.drag(from, point(from.x + px(40.), from.y + px(10.)), cx);
    })
    .unwrap();
}

fn center_of(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) -> Point<Pixels> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(id).bounds().center()
    })
    .unwrap()
}

/// A button in the toolbar takes the press: dragging from it doesn't move the window.
#[gpui_kit::test]
fn dragging_from_a_toolbar_button_leaves_the_window(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    click_message(cx, window, &mailbox, "你好，世界");

    let back = center_of(cx, window, "back");
    drag_from(cx, window, back);
    let search = center_of(cx, window, "search");
    drag_from(cx, window, search);
}

/// The toolbar's empty end moves the window.
#[gpui_kit::test]
#[should_panic(expected = "not implemented")]
fn dragging_from_the_toolbar_moves_the_window(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    click_message(cx, window, &mailbox, "你好，世界");

    // Past the last button, at the window's right edge.
    let back = center_of(cx, window, "back");
    drag_from(cx, window, point(px(1270.), back.y));
}

/// So does the row the window's buttons sit in, over the sidebar.
#[gpui_kit::test]
#[should_panic(expected = "not implemented")]
fn dragging_from_above_the_sidebar_moves_the_window(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);

    let compose = center_of(cx, window, "new-message");
    drag_from(cx, window, point(compose.x, px(10.)));
}

/// A press that leaves the row at once and is let go elsewhere is over: the pointer
/// coming back over the row, with no button down, doesn't move the window.
#[gpui_kit::test]
fn a_press_let_go_off_the_row_is_over(cx: &mut TestAppContext) {
    let mail = TestMail::start();
    let (window, mailbox) = open(cx, &mail);
    wait_for_inbox(cx, &mailbox);
    click_message(cx, window, &mailbox, "你好，世界");

    // From the toolbar's empty end, down the window: the first move is already below
    // the row, and the button is let go there.
    let back = center_of(cx, window, "back");
    cx.update_window(window, |_, window, cx| {
        window.drag(point(px(1270.), back.y), point(px(1270.), px(790.)), cx);
        window.hover("search", cx);
    })
    .unwrap();
}
