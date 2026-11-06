//! The buttons the panes and windows share: icon buttons (a toolbar's or a row's), the
//! primary one, a text one, the separator between groups of them, and their tooltip.

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
    fn box_size(self) -> f32 {
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

/// Between groups of a toolbar's buttons.
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
    view()
        .w(px(size.box_size()))
        .h(px(size.box_size()))
        .flex_shrink_0()
        .rounded(px(6.0))
        .items_center()
        .justify_center()
        .opacity(move || if enabled() { 1.0_f32 } else { 0.4 })
        .hover(move |s| {
            s.bg(move || {
                if enabled() {
                    theme().accent
                } else {
                    AdaptiveColor::TRANSPARENT
                }
            })
        })
        .label(move || SharedString::from(name()))
        .on_press(move || {
            if untracked(enabled) {
                press();
            }
        })
        .tooltip(tooltip(name))
        .child(icon(which, size.icon_size()).text_color(move || theme().foreground))
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

/// A tooltip that says `text`.
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

/// A small button with an icon and a label: outlined, or plain.
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
