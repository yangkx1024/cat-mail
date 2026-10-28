//! Noticing on its own that the computer slept, so that background work catches up
//! ([`Engine::resume`]) whether or not the frontend hears of the wake from the system
//! (`catmail watch` doesn't). Runs for as long as the engine: a pooled connection goes
//! stale over a sleep whether or not anything runs in the background.
//!
//! The wall clock counts the sleep and the monotonic one (`Instant`) doesn't, on Linux
//! and macOS: checked now and then, the wall clock having moved much further than
//! `Instant` means the computer slept in between. The wake is taken to be when this is
//! noticed, up to [`CHECK_EVERY`] late, so work begun in between is done again: on the
//! safe side. Where `Instant` counts the sleep too (it may on Windows), nothing is
//! noticed.

use std::sync::Weak;
use std::time::{Duration, Instant, SystemTime};

use crate::engine::{Closing, Engine};

/// How often the clocks are compared: the longest it takes to notice a wake.
pub(crate) const CHECK_EVERY: Duration = Duration::from_secs(30);

/// How much further than `Instant` the wall clock must have moved between two checks
/// for that to be a sleep. Far more than drift, or than a time sync usually sets the
/// clock forward by: a false alarm costs new connections and a sync of every folder,
/// as does the user setting the clock forward. A shorter sleep goes unnoticed, which
/// costs little: connections mostly survive it, and timers run late by as much.
const SLEPT: Duration = Duration::from_secs(60);

/// Both clocks, read at one moment.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Clocks {
    pub wall: SystemTime,
    pub mono: Instant,
}

impl Clocks {
    pub fn now() -> Self {
        Self {
            wall: SystemTime::now(),
            mono: Instant::now(),
        }
    }

    /// Whether the computer slept between `self` and `later`. A wall clock set back
    /// says nothing either way.
    fn slept_until(self, later: Clocks) -> bool {
        let wall = later.wall.duration_since(self.wall).unwrap_or_default();
        wall.saturating_sub(later.mono - self.mono) > SLEPT
    }
}

/// Compares the clocks, as `now` reads them, every `every` for as long as the engine
/// exists, and resumes it after each sleep the frontend didn't already resume it for.
pub(crate) async fn run(
    engine: Weak<Engine>,
    every: Duration,
    mut now: impl FnMut() -> Clocks,
    mut closing: Closing,
) {
    let mut last = now();
    loop {
        tokio::select! {
            () = tokio::time::sleep(every) => {}
            () = closing.closed() => return,
        }
        let clocks = now();
        if last.slept_until(clocks) {
            let Some(engine) = engine.upgrade() else {
                return;
            };
            // Unless the frontend already said so: resuming again would drop the
            // connections and syncs made since.
            if engine.since_woke(last.mono) {
                tracing::info!("the computer slept; starting over on new connections");
                engine.resume();
            }
        }
        last = clocks;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(start: Clocks, wall: u64, mono: u64) -> Clocks {
        Clocks {
            wall: start.wall + Duration::from_secs(wall),
            mono: start.mono + Duration::from_secs(mono),
        }
    }

    #[test]
    fn only_a_wall_clock_far_ahead_is_a_sleep() {
        let start = Clocks::now();
        // Both moved alike: awake.
        assert!(!start.slept_until(after(start, 30, 30)));
        // A check held up (a busy runtime, a stopped process): both moved alike.
        assert!(!start.slept_until(after(start, 600, 600)));
        // A time sync setting the clock a little forward.
        assert!(!start.slept_until(after(start, 35, 30)));
        // Slept for an hour.
        assert!(start.slept_until(after(start, 3630, 30)));

        // The wall clock set back.
        let back = Clocks {
            wall: start.wall - Duration::from_secs(3600),
            mono: start.mono + Duration::from_secs(30),
        };
        assert!(!start.slept_until(back));
    }
}
