//! The buttons the panes and windows share: icon buttons (a toolbar's or a row's), the
//! primary one, a text one, the separator between groups of them, their tooltip, and the
//! items of a sidebar's menu (the mailbox's, the settings dialog's).

use std::rc::Rc;

use nsh_base::theme;
use nsh_core::prelude::*;

use crate::icons::{Icon, icon};

/// How big the buttons are: a toolbar's, or a row's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    Bar,
    Row,
}

impl Size {
    pub fn box_size(self) -> f32 {
        match self {
            Size::Bar => 30.0,
            Size::Row => 24.0,
        }
    }

    fn icon_size(self) -> f32 {
        match self {
            Size::Bar => 18.0,
            Size::Row => 15.0,
        }
    }
}

pub fn separator(size: Size) -> View {
    view()
        .w(px(1.0))
        .h(px(size.box_size() - 12.0))
        .mx(px(4.0))
        .bg(move || theme().border)
}

/// An icon that does `press`; its name shows as a tooltip and is what screen readers
/// say. Disabled, it is dimmed and does nothing.
pub fn icon_button(
    which: impl Fn() -> Icon + 'static,
    name: impl Fn() -> String + Copy + 'static,
    enabled: impl Fn() -> bool + Copy + 'static,
    size: Size,
    press: impl Fn() + 'static,
) -> View {
    button_box(name, enabled, enabled, size, press)
        .child(icon(which, size.icon_size()).text_color(move || theme().foreground))
}

/// An [`icon_button`] whose chevron folds and unfolds something: it turns down while
/// `open`.
pub fn fold_button(
    open: impl Fn() -> bool + 'static,
    name: impl Fn() -> String + Copy + 'static,
    size: Size,
    press: impl Fn() + 'static,
) -> View {
    button_box(name, || true, || true, size, press).child(
        crate::motion::chevron(open, size.icon_size()).text_color(move || theme().foreground),
    )
}

/// An [`icon_button`] that shows macOS's spinner in place of its icon while `busy`: it
/// does nothing then, but isn't dimmed, the spinner saying why.
pub fn busy_icon_button(
    which: impl Fn() -> Icon + Copy + 'static,
    name: impl Fn() -> String + Copy + 'static,
    enabled: impl Fn() -> bool + Copy + 'static,
    busy: impl Fn() -> bool + Copy + 'static,
    size: Size,
    press: impl Fn() + 'static,
) -> View {
    let active = move || enabled() && !busy();
    button_box(name, active, enabled, size, press).child(dynamic(move || {
        if busy() {
            spinner(size.icon_size() - 2.0)
                .text_color(move || theme().muted_foreground)
                .into_element()
        } else {
            icon(which, size.icon_size())
                .text_color(move || theme().foreground)
                .into_element()
        }
    }))
}

/// An icon button's box, without the icon: it does `press` while `active`, and is dimmed
/// unless `lit`.
fn button_box(
    name: impl Fn() -> String + Copy + 'static,
    active: impl Fn() -> bool + Copy + 'static,
    lit: impl Fn() -> bool + 'static,
    size: Size,
    press: impl Fn() + 'static,
) -> View {
    view()
        .w(px(size.box_size()))
        .h(px(size.box_size()))
        .flex_shrink_0()
        .rounded(px(6.0))
        .items_center()
        .justify_center()
        .opacity(move || if lit() { 1.0_f32 } else { 0.4 })
        .hover(move |s| {
            s.bg(move || {
                if active() {
                    theme().accent
                } else {
                    AdaptiveColor::TRANSPARENT
                }
            })
        })
        .label(move || SharedString::from(name()))
        .on_press(move || {
            if untracked(active) {
                press();
            }
        })
        .tooltip(tooltip(name))
}

/// macOS's spinner, `size` points square, turning while shown: a spoke at a time, as
/// AppKit's does.
pub fn spinner(size: f32) -> Svg {
    const SPOKES: u32 = 12;
    const TURN_MS: f32 = 1000.0;
    let angle = animated_value(0.0);
    let steps = Easing::Steps(SPOKES, StepPosition::JumpEnd);
    let turning = Animation::timing(&angle, 360.0, Timing::new(TURN_MS, steps)).looped(None);
    // Nothing waits on it ending: it ends with the spinner.
    let _ = turning.start();
    icon(|| Icon::Spinner, size).bind_rotate(&angle)
}

/// The narrow chevron beside a button that opens a menu of other ways to do what the
/// button does (`open` shows it): one control with it, as far as hover goes.
pub fn menu_button(
    name: impl Fn() -> String + Copy + 'static,
    size: Size,
    open: impl Fn() + 'static,
) -> View {
    view()
        .w(px(size.box_size() / 2.0))
        .h(px(size.box_size()))
        .flex_shrink_0()
        .rounded(px(6.0))
        .items_center()
        .justify_center()
        .hover(|s| s.bg(move || theme().accent))
        .label(move || SharedString::from(name()))
        .on_press(open)
        .tooltip(tooltip(name))
        .child(
            icon(|| Icon::ChevronDown, size.icon_size() * 2.0 / 3.0)
                .text_color(move || theme().muted_foreground),
        )
}

/// The button of the main thing to do where it is (Compose, Send, Edit): filled, round,
/// with an icon and a label.
pub fn primary_button(
    which: Icon,
    label: impl Fn() -> String + 'static,
    hint: impl Fn() -> String + 'static,
    enabled: impl Fn() -> bool + Copy + 'static,
    press: impl Fn() + 'static,
) -> View {
    let label = Rc::new(label);
    let name = label.clone();
    view()
        .flex_row()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .gap(px(8.0))
        .h(px(34.0))
        .px(px(18.0))
        .rounded(px(17.0))
        .bg(move || theme().primary)
        .opacity(move || if enabled() { 1.0_f32 } else { 0.5 })
        .label(move || SharedString::from(name()))
        .on_press(move || {
            if untracked(enabled) {
                press();
            }
        })
        .tooltip(tooltip(hint))
        .child(icon(move || which, 16.0).text_color(move || theme().primary_foreground))
        .child(
            text(move || label())
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || theme().primary_foreground),
        )
}

/// A button that says what it does: outlined when it is the likeliest choice, plain
/// otherwise.
pub fn text_button(label: &'static str, outlined: bool, press: impl Fn() + 'static) -> View {
    view()
        .flex_row()
        .items_center()
        .h(px(32.0))
        .px(px(14.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(move || {
            if outlined {
                theme().border
            } else {
                AdaptiveColor::TRANSPARENT
            }
        })
        .hover(|s| s.bg(move || theme().accent))
        .label(SharedString::from(label))
        .on_press(press)
        .child(
            text(label)
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || theme().foreground),
        )
}

pub fn tooltip(text_: impl Fn() -> String + 'static) -> AnyElement {
    view()
        .max_w(px(320.0))
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .bg(move || theme().popover)
        .border_1()
        .border_color(move || theme().border)
        .child(
            text(text_)
                .text_xs()
                .text_color(move || theme().popover_foreground),
        )
        .into_element()
}

pub fn labelled_button(
    glyph: Icon,
    label: &'static str,
    outlined: bool,
    enabled: impl Fn() -> bool + Copy + 'static,
    press: impl Fn() + 'static,
) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(30.0))
        .px(px(12.0))
        .rounded(px(15.0))
        .border_1()
        .border_color(move || {
            if outlined {
                theme().border
            } else {
                AdaptiveColor::TRANSPARENT
            }
        })
        .hover(move |s| {
            s.bg(move || {
                if enabled() {
                    theme().accent
                } else {
                    AdaptiveColor::TRANSPARENT
                }
            })
        })
        .opacity(move || if enabled() { 1.0_f32 } else { 0.5 })
        .label(label)
        .on_press(move || {
            if untracked(enabled) {
                press();
            }
        })
        .child(icon(move || glyph, 15.0).text_color(move || theme().foreground))
        .child(
            text(label)
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || theme().foreground),
        )
}

/// An item of a sidebar menu, the mailbox's or the settings'. Selected, the icon fills
/// and the icon and label take the primary color; the label keeps its weight.
pub fn menu_item(
    which: impl Fn() -> Icon + 'static,
    label: impl IntoReactive<SharedString>,
    selected: impl Fn() -> bool + Copy + 'static,
) -> View {
    view()
        .h(px(28.0))
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .bg(menu_item_background(selected, false))
        .hover(move |s| s.bg(menu_item_background(selected, true)))
        // As large as the label's text.
        .child(
            icon(
                move || if selected() { which().solid() } else { which() },
                14.0,
            )
            .text_color(row_color(selected)),
        )
        .child(
            text(label)
                .number_of_lines(1)
                .flex_1()
                .text_sm()
                .text_color(row_color(selected)),
        )
}

/// A menu item's background: `hovered` for the hover style's.
pub fn menu_item_background(
    selected: impl Fn() -> bool + 'static,
    hovered: bool,
) -> impl Fn() -> AdaptiveColor + 'static {
    move || {
        if selected() {
            theme().sidebar_accent
        } else if hovered {
            theme().sidebar_accent.opacity(0.8)
        } else {
            AdaptiveColor::TRANSPARENT
        }
    }
}

fn row_color(selected: impl Fn() -> bool + 'static) -> impl Fn() -> AdaptiveColor + 'static {
    move || {
        if selected() {
            theme().primary
        } else {
            theme().sidebar_foreground
        }
    }
}
