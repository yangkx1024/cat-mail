//! The user's clock, as the system's locale settings have it.

use catmail_frontend::format::Clock;
use nsh_core::prelude::*;

/// 12- or 24-hour time. Reading it subscribes, so the times shown follow the setting
/// when the user changes it.
pub fn clock() -> Clock {
    if locale_settings().hour_cycle.twelve_hour() {
        Clock::TwelveHour
    } else {
        Clock::TwentyFourHour
    }
}
