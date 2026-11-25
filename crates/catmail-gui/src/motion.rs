//! The app's motion: its tokens, and the moves more than one view makes. Every one of
//! them is instant with reduced motion.

use std::time::Duration;

use nsh_base::{CollapsibleElement, collapsible};
use nsh_core::element::{LayoutAnimation, LayoutCurve, LayoutTransition, Spring};
use nsh_core::prelude::*;

use crate::icons::{Icon, icon};

/// Moving into place: rows sliding, a chevron turning.
const BASE_MS: f32 = 240.0;
/// Leaving, which is faster than coming.
const FAST_MS: f32 = 180.0;
/// How long nsh's animated scrolls run (`ANIMATION` in nsh-platform-macos'
/// `scroll.rs`), with a frame to spare. Checked on each nsh upgrade (gui.md, "升级 nsh
/// 时核对").
const SCROLL_MS: f32 = 300.0 + 20.0;

/// How the layout's changes glide: frames move between where they were and where they
/// go (ease-in-out), what comes in fades in.
const GLIDE: LayoutAnimation = LayoutAnimation {
    create: Some(LayoutTransition::new(LayoutCurve::EaseOut, BASE_MS)),
    update: Some(LayoutTransition::new(LayoutCurve::EaseInEaseOut, BASE_MS)),
    delete: None,
};

/// The spring folds open and close on: nsh-base's default for `Collapsible` (a 180 ms
/// response, critically damped), given explicitly so that its length is known here.
fn reveal_spring() -> Spring {
    Spring::new(180.0)
}

/// Runs `act` each time `on` changes, not for its first value.
fn on_toggle(on: impl Fn() -> bool + 'static, act: impl Fn() + 'static) {
    let was = stored(untracked(&on));
    effect(move || {
        let now = on();
        if was.replace(now) != now {
            untracked(&act);
        }
    });
}

/// Whether the platform's scroll indicators are hidden, and which hiding the timer that
/// shows them again belongs to.
#[derive(Clone, Copy, Default, PartialEq)]
struct HiddenIndicators {
    hidden: bool,
    generation: u64,
}

/// Hides the platform's scroll indicators for `ms`, from the commit being made: a layout
/// animation is about to move scroll views or change their content. ⚠️ nsh's macOS
/// indicator isn't placed again as its view's frame animates, only as the view scrolls
/// or its content's size changes, so it would jump to where it ends, or move on a curve
/// of its own. Every scroll shows it, so it stays hidden while a scroll that comes with
/// the change runs too. It shows again on the next scroll or hover. Nothing with reduced
/// motion, which moves nothing. Until longbridge/nsh#3188 is fixed.
fn hide_indicators(ms: f32) {
    if reduce_motion() {
        return;
    }
    let hidden = global::<HiddenIndicators>();
    let generation = hidden.get_untracked().generation.wrapping_add(1);
    hidden.set(HiddenIndicators {
        hidden: true,
        generation,
    });
    set_timeout(Duration::from_millis(ms as u64), move || {
        if hidden.get_untracked().generation == generation {
            hidden.update(|h| h.hidden = false);
        }
    });
}

/// Whether scroll views show their indicator now ([`hide_indicators`]). Tracked.
pub fn indicators_shown() -> bool {
    !global::<HiddenIndicators>().with(|h| h.hidden)
}

/// The rows of a list change in the next commit (messages arriving, going away): the
/// rows that stay slide to their new places, the ones that come fade in, the ones that
/// go fade out. A glide asked for later in the same turn ([`glide_when`]) takes its
/// place: nsh animates a commit with the last one.
pub fn glide_rows() {
    if reduce_motion() {
        return;
    }
    animate_layout(LayoutAnimation {
        delete: Some(LayoutTransition::new(LayoutCurve::EaseIn, FAST_MS)),
        ..GLIDE
    });
}

/// Each time `on` changes, the whole commit's layout glides: a list among it slides its
/// rows only so (nsh-core's `list_motion.rs` keeps them where they showed as the list
/// scrolls).
pub fn glide_when(on: impl Fn() -> bool + 'static) {
    on_toggle(on, || {
        if reduce_motion() {
            return;
        }
        // The list's, which moves and scrolls; the sidebar's while at it.
        hide_indicators(BASE_MS.max(SCROLL_MS));
        animate_layout(GLIDE);
    });
}

/// A section that folds away under what heads it: `Collapsible`'s reveal, the clipped
/// height springing open and shut.
pub fn fold(open: impl Fn() -> bool + Copy + 'static) -> CollapsibleElement {
    on_toggle(open, || hide_indicators(reveal_spring().settle_ms()));
    collapsible()
        .open(open)
        .reveal(true)
        .reveal_spring(Some(reveal_spring()))
}

/// A chevron that points right while `open` is false and turns down as it opens.
pub fn chevron(open: impl Fn() -> bool + 'static, size: f32) -> Svg {
    icon(|| Icon::ChevronRight, size)
        .rotate(move || deg(if open() { 90.0 } else { 0.0 }))
        .transition_transform()
        .duration(BASE_MS)
        .ease_in_out()
}
