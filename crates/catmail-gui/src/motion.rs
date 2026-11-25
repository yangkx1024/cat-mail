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
        meter_frames("glide", BASE_MS.max(SCROLL_MS));
    });
}

/// Waits until a glide asked for now ([`glide_when`]) is over: at once with reduced motion,
/// which glides nothing.
pub async fn glide_over() {
    if !reduce_motion() {
        // A frame to spare.
        timer(Duration::from_millis(BASE_MS as u64 + 20)).await;
    }
}

/// With `CATMAIL_GUI_FRAMES` set: logs the display frames of the next `ms` (an
/// animation's): how long its first frame took to come, the gaps between the frames, and
/// how many were missed (a gap of more than one and a half of the usual one), to compare
/// changes by numbers rather than by eye. In the log file (`catmail_gui` at info).
fn meter_frames(what: &'static str, ms: f32) {
    if std::env::var_os("CATMAIL_GUI_FRAMES").is_none() {
        return;
    }
    let asked = nsh_core::time::now();
    let until = asked + Duration::from_millis(ms as u64);
    spawn(async move {
        let mut frames = Vec::new();
        loop {
            let at = next_frame().await;
            frames.push(at);
            if at >= until {
                break;
            }
        }
        let gaps: Vec<f64> = frames
            .windows(2)
            .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0)
            .collect();
        let first_ms = (frames[0] - asked).as_secs_f64() * 1000.0;
        let Some(stats) = FrameStats::of(gaps) else {
            return;
        };
        tracing::info!(
            what,
            frames = frames.len(),
            first_ms = format!("{first_ms:.1}"),
            fps = format!("{:.1}", stats.fps),
            p50_ms = format!("{:.1}", stats.p50),
            p95_ms = format!("{:.1}", stats.p95),
            max_ms = format!("{:.1}", stats.max),
            missed = stats.missed,
            "animation frames"
        );
        eprintln!(
            "[frames] {what}: {} frames, first {first_ms:.1} ms, {:.1} fps, gap p50 {:.1} / p95 {:.1} / max {:.1} ms, {} missed",
            frames.len(),
            stats.fps,
            stats.p50,
            stats.p95,
            stats.max,
            stats.missed,
        );
    });
}

/// The gaps between an animation's frames, summed up.
#[derive(Debug, PartialEq)]
struct FrameStats {
    fps: f64,
    p50: f64,
    p95: f64,
    max: f64,
    /// Frames that should have come and didn't: a gap of n usual ones misses n - 1. The
    /// usual gap is the median, the display's period: not the shortest, as a frame that
    /// comes early after a late one would make every gap on time look missed.
    missed: u32,
}

impl FrameStats {
    fn of(mut gaps: Vec<f64>) -> Option<Self> {
        if gaps.is_empty() {
            return None;
        }
        gaps.sort_by(f64::total_cmp);
        let at = |q: f64| gaps[((gaps.len() - 1) as f64 * q).round() as usize];
        let period = at(0.5).max(1.0);
        let missed = gaps
            .iter()
            .filter(|g| **g > period * 1.5)
            .map(|g| (g / period).round() as u32 - 1)
            .sum();
        Some(Self {
            fps: gaps.len() as f64 * 1000.0 / gaps.iter().sum::<f64>(),
            p50: at(0.5),
            p95: at(0.95),
            max: gaps[gaps.len() - 1],
            missed,
        })
    }
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

#[cfg(test)]
mod tests {
    use super::FrameStats;

    #[test]
    fn frame_stats_count_the_frames_a_long_gap_missed() {
        // At 60 Hz: one gap of three periods misses two frames.
        let s = FrameStats::of(vec![16.7, 16.7, 50.1, 16.7]).unwrap();
        assert_eq!(s.missed, 2);
        assert_eq!(s.max, 50.1);
        assert_eq!(s.p50, 16.7);
        assert!((s.fps - 4.0 * 1000.0 / 100.2).abs() < 1e-9);
    }

    #[test]
    fn frame_stats_take_an_early_frame_for_no_period() {
        // A frame 11 ms after one 22 ms late: the gaps on time miss nothing.
        let s = FrameStats::of(vec![16.6, 22.2, 11.0, 16.6, 16.7, 16.6]).unwrap();
        assert_eq!(s.missed, 0);
    }

    #[test]
    fn frame_stats_of_no_gaps_is_nothing() {
        assert_eq!(FrameStats::of(vec![]), None);
    }
}
