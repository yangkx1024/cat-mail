//! The user's clock, as the system's locale settings have it, and the calendar it puts
//! the list's days on.

use std::time::Duration;

use catmail_frontend::format::{self, Calendar, Clock, Hours};
use nsh_core::element::Owner;
use nsh_core::prelude::*;

/// The time zone and 12- or 24-hour time. Reading it subscribes, so the times shown follow
/// the settings when the user changes them, or travels.
pub fn clock() -> Clock {
    clocks().clock.get()
}

/// Today in the user's zone. Reading it subscribes, so the days follow the zone and the
/// date.
pub fn calendar() -> Calendar {
    clocks().calendar.get()
}

/// The surface's clock and calendar, made the first time they are read: one look at the
/// locale settings and the tz database for every time shown.
#[derive(Clone, Copy)]
struct Clocks {
    clock: Memo<Clock>,
    calendar: Memo<Calendar>,
}

#[derive(Clone, Copy, Default)]
struct Shared(Option<Clocks>);

/// How long the calendar goes at most without a look: a timer doesn't count the time the
/// computer sleeps, which can take it past midnight.
const DAY_CHECK: Duration = Duration::from_secs(10 * 60);

fn clocks() -> Clocks {
    if let Some(clocks) = global::<Shared>().with_untracked(|s| s.0) {
        return clocks;
    }
    // As the surface's globals are: not the reader's that reads them first.
    let clocks = Owner::detached().run(|| {
        let clock = computed(|| {
            let settings = locale_settings();
            let hours = if settings.hour_cycle.twelve_hour() {
                Hours::Twelve
            } else {
                Hours::TwentyFour
            };
            Clock {
                zone: format::zone(settings.time_zone.as_str()),
                hours,
            }
        });
        let checked = signal(0u64);
        let calendar = computed(move || {
            checked.get();
            clock.with(|c| Calendar::now(&c.zone))
        });
        spawn(async move {
            loop {
                let zone = clock.with_untracked(|c| c.zone.clone());
                let wait = format::until_tomorrow(&zone).map_or(DAY_CHECK, |d| d.min(DAY_CHECK));
                timer(wait).await;
                checked.update(|n| *n += 1);
            }
        });
        Clocks { clock, calendar }
    });
    global::<Shared>().set(Shared(Some(clocks)));
    clocks
}
