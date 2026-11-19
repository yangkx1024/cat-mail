//! The bar of a page wider than its web view, at the bottom of the pane it is in
//! (`catmail_frontend::html::Sideways`): the web view is as tall as its page, so the
//! body's own sideways scroller, which the page hides, would be at the end of the
//! message. It looks and comes and goes as the pane's own indicator at the side: shown
//! while the page or the pane scrolls, and as the mouse moves over the pane (on Linux,
//! over its thumb), then faded; dragged, or pressed beside its thumb, it scrolls the page.

use std::time::Duration;

use catmail_frontend::html;
use nsh_core::element::ElementRef;
use nsh_core::prelude::*;
use nsh_core::time::{Instant, now};

/// How long it stays after the last scroll, then how long it takes to fade (ms): the
/// scroll views' indicators'.
const HOLD: u64 = 700;
const FADE: f32 = 300.0;

/// The thumb's thickness: the scroll views' indicators', which macOS draws thicker.
#[cfg(target_os = "macos")]
const THICK: f32 = 6.0;
#[cfg(not(target_os = "macos"))]
const THICK: f32 = nsh_core::scroll::INDICATOR_THICK.0;

/// The space between the thumb and the pane's edges.
const EDGE: f32 = 2.0;

/// How tall the bar is, the thumb at its bottom: a pointer is less precise than the
/// thumb is thick.
const GRAB: f32 = 12.0;

/// How far beside the thumb the mouse reveals it on Linux: the scroll views'.
#[cfg(not(target_os = "macos"))]
const SLOP: f32 = 4.0;

/// A web view's page scrolled sideways, and its bar ([`Self::around`]). One per web view.
#[derive(Clone, Copy)]
pub struct Sideways {
    /// The web view, which the bar scrolls.
    pub(super) web: ElementRef,
    /// What the page said last.
    at: Signal<html::Sideways>,
    /// The thumb's room along the bar.
    rail: Signal<f32>,
    /// The page or the pane scrolled a moment ago.
    active: Signal<bool>,
    /// The pointer is over the bar.
    over: Signal<bool>,
    /// The pane's height, the bar at its bottom: where the mouse reveals it on Linux.
    #[cfg(not(target_os = "macos"))]
    height: Stored<f32>,
    /// The offset a drag of the thumb started at.
    drag: Signal<Option<f32>>,
    /// When the bar hides, unless something shows it first: one timer at a time, waiting
    /// again while it moved on (scrolls come every frame).
    hides_at: Stored<Option<Instant>>,
    opacity: AnimatedValue,
}

impl Sideways {
    pub fn new() -> Self {
        Self {
            web: element_ref(),
            at: signal(html::Sideways::default()),
            rail: signal(0.0),
            active: signal(false),
            over: signal(false),
            #[cfg(not(target_os = "macos"))]
            height: stored(0.0),
            drag: signal(None),
            hides_at: stored(None),
            opacity: animated_value(0.0),
        }
    }

    /// What the page says: a page scrolled (not the first word of one, nor a resize)
    /// shows the bar. While the thumb is dragged, the page's word on where it is trails
    /// the drag, which the thumb follows.
    pub(super) fn said(self, now: html::Sideways) {
        let before = self.at.get_untracked();
        let mut now = now;
        if self.drag.get_untracked().is_some() {
            now.offset = before.offset;
        }
        self.at.set(now);
        if before.content > 0.0 && before.offset != now.offset {
            self.show();
        }
    }

    /// The pane scrolled: the bar shows too, as both of a view's indicators do.
    fn scrolled(self) {
        if self.at.with_untracked(|s| s.scrolls()) {
            self.show();
        }
    }

    /// The mouse moved over the pane, at `e.position` in it: the bar shows where the
    /// pane's own indicator does. Not a drag, as AppKit's: that is a press's.
    fn hovered(self, e: &MouseEvent) {
        if e.pressed_button.is_none()
            && self.at.with_untracked(|s| s.scrolls())
            && self.reveals(e.position)
        {
            self.show();
        }
    }

    /// Anywhere on the pane, as AppKit's overlay scroll bars and so nsh's indicators:
    /// unless the user wants them only while scrolling.
    #[cfg(target_os = "macos")]
    fn reveals(self, _at: Point<Points>) -> bool {
        use objc2_foundation::{NSUserDefaults, ns_string};
        !NSUserDefaults::standardUserDefaults()
            .stringForKey(ns_string!("AppleShowScrollBars"))
            .is_some_and(|setting| setting.isEqualToString(ns_string!("WhenScrolling")))
    }

    /// Only on its thumb, as nsh's indicators on Linux: moving over the page doesn't.
    #[cfg(not(target_os = "macos"))]
    fn reveals(self, at: Point<Points>) -> bool {
        let Some((start, len)) = self
            .at
            .with_untracked(|s| s.thumb(self.rail.get_untracked()))
        else {
            return false;
        };
        let x = at.x.0 - EDGE;
        at.y.0 >= self.height.get() - GRAB && (start - SLOP..=start + len + SLOP).contains(&x)
    }

    /// Shows the bar until [`HOLD`] after the last call.
    fn show(self) {
        self.active.set(true);
        let hold = Duration::from_millis(HOLD);
        if self.hides_at.replace(Some(now() + hold)).is_none() {
            self.hide_after(hold);
        }
    }

    fn hide_after(self, delay: Duration) {
        set_timeout(delay, move || {
            let Some(at) = self.hides_at.get() else {
                return;
            };
            let left = at.saturating_duration_since(now());
            if left.is_zero() {
                self.hides_at.replace(None);
                self.active.set(false);
            } else {
                self.hide_after(left);
            }
        });
    }

    /// Scrolls the page to `offset`; the thumb goes there at once, not when the page
    /// says so.
    fn scroll_to(self, offset: f32) {
        let at = self.at.get_untracked();
        if at.offset == offset {
            return;
        }
        self.web
            .inject_java_script(format!("document.body.scrollLeft = {offset};"));
        self.at.set(html::Sideways { offset, ..at });
    }

    /// A press at `x` on the bar: on the thumb, a drag moves it from there; beside it,
    /// its middle jumps there first.
    fn press(self, x: f32) {
        let (at, rail) = (self.at.get_untracked(), self.rail.get_untracked());
        let Some((start, len)) = at.thumb(rail) else {
            return;
        };
        let x = x - EDGE;
        let from = if (start..start + len).contains(&x) {
            at.here()
        } else {
            let to = at.pressed(rail, x);
            self.scroll_to(to);
            to
        };
        self.drag.set(Some(from));
    }

    /// The drag moved `by` points since the press.
    fn drag_by(self, by: f32) {
        let Some(from) = self.drag.get_untracked() else {
            return;
        };
        let to = self
            .at
            .get_untracked()
            .dragged(self.rail.get_untracked(), from, by);
        self.scroll_to(to);
    }

    fn release(self) {
        if self.drag.get_untracked().is_some() {
            self.drag.set(None);
            self.show();
        }
    }

    /// The pane, a scroll view with the web view in it, with the bar at its bottom.
    pub fn around(self, pane: Scroll) -> impl IntoElement {
        let around = view()
            .flex_1()
            .min_h(px(0.0))
            .on_mouse_move(move |e: &MouseEvent| self.hovered(e));
        #[cfg(not(target_os = "macos"))]
        let around = around.on_layout(move |frame: &Bounds<Points>| {
            self.height.replace(frame.size.height.0);
        });
        around
            .child(pane.on_scroll(move |_: &ScrollEvent| self.scrolled()))
            .child(self.bar())
    }

    /// The bar, at the bottom of the view it is put in (over the pane's scroll view,
    /// which it is a sibling of).
    fn bar(self) -> impl IntoElement {
        let shown = computed(move || {
            self.at.with(|s| s.scrolls())
                && (self.active.get() || self.over.get() || self.drag.get().is_some())
        });
        effect(move || {
            if shown.get() {
                self.opacity.set(1.0);
            } else {
                drop(self.opacity.timing(0.0, FADE, Easing::Linear));
            }
        });
        let thumb = move || {
            self.at
                .with(|s| s.thumb(self.rail.get()))
                .unwrap_or_default()
        };
        // The scroll views' indicators' colors.
        let color = || AdaptiveColor {
            light: hsla(0.0, 0.0, 0.0, 0.45),
            dark: hsla(0.0, 0.0, 1.0, 0.55),
        };
        let rail = view()
            .flex_1()
            .flex_row()
            .items_end()
            .pointer_events(PointerEvents::None)
            .on_layout(move |frame: &Bounds<Points>| self.rail.set(frame.size.width.0))
            .child(
                view()
                    .flex_shrink_0()
                    .h(px(THICK))
                    .rounded(px(THICK / 2.0))
                    .bg(color)
                    .w(move || px(thumb().1))
                    .translate_x(move || px(thumb().0)),
            );
        view()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .bottom(px(0.0))
            .h(px(GRAB))
            .z_index(1)
            .flex_row()
            .pl(px(EDGE))
            // Clear of the pane's own indicator at the side, as a view's two are.
            .pr(px(EDGE + THICK + EDGE))
            .pb(px(EDGE))
            .overflow_hidden()
            .bind_opacity(&self.opacity)
            // Hidden, it lets the pointer through to the page under it.
            .pointer_events(move || {
                if shown.get() {
                    PointerEvents::Auto
                } else {
                    PointerEvents::None
                }
            })
            .on_mouse_enter(move |_: &MouseEvent| self.over.set(true))
            .on_mouse_leave(move |_: &MouseEvent| {
                self.over.set(false);
                self.show();
            })
            .claim_touches(TouchClaim::on_start().ask(|request: &ClaimRequest| request.claim()))
            // The pane's scroll view doesn't take the drag over.
            .keep_touches(true)
            .on_responder_grant(move |e: &ResponderEvent| self.press(e.location.x.0))
            .on_responder_move(move |e: &ResponderEvent| self.drag_by(e.gesture.delta.x.0))
            .on_responder_release(move |_: &ResponderEvent| self.release())
            .on_responder_terminate(move |_: &ResponderEvent| self.release())
            .child(rail)
    }
}
