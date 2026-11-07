//! How message metadata reads in the window: what `catmail_core::display` doesn't say,
//! the times on the user's clock and the list's day headings.

use jiff::civil::Date;
use jiff::{Timestamp, Zoned, tz::TimeZone};

/// What reads the same as in the CLI.
pub use catmail_core::display::{addresses, names, sender, size, subject};

/// How the user reads the time of day: the system's setting, else their region's. The
/// CLI keeps 24-hour time, which scripts can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    /// `14:05`.
    TwentyFourHour,
    /// `2:05 PM`.
    TwelveHour,
}

impl Clock {
    fn time(self) -> &'static str {
        match self {
            Clock::TwentyFourHour => "%H:%M",
            Clock::TwelveHour => "%-I:%M %p",
        }
    }

    fn date(self) -> &'static str {
        match self {
            Clock::TwentyFourHour => "%Y-%m-%d %H:%M",
            Clock::TwelveHour => "%Y-%m-%d %-I:%M %p",
        }
    }
}

/// The time of day, as a list row shows it under its day's heading: `14:05`.
pub fn time(timestamp: Option<i64>, clock: Clock) -> String {
    catmail_core::display::local(timestamp, clock.time())
}

/// A date and time in full, as the reader's header and the outbox show it:
/// `2026-10-19 14:05`. Empty for none.
pub fn date(timestamp: Option<i64>, clock: Clock) -> String {
    catmail_core::display::local(timestamp, clock.date())
}

/// The local calendar as of one moment, to put many rows under day headings with one
/// look at the clock and the time zone.
pub struct Calendar {
    tz: TimeZone,
    today: Date,
}

impl Calendar {
    pub fn now() -> Self {
        let tz = TimeZone::system();
        let today = Zoned::now().with_time_zone(tz.clone()).date();
        Self { tz, today }
    }

    /// The local day `timestamp` falls on.
    pub fn day(&self, timestamp: Option<i64>) -> Option<Date> {
        zoned(timestamp, &self.tz).map(|t| t.date())
    }

    /// The heading over a day's rows: `Today`, `Yesterday`, the weekday and date this
    /// year (`Monday, October 12`), with the year before that (`October 12, 2025`).
    /// Rows without a date go under `No date`.
    pub fn heading(&self, day: Option<Date>) -> String {
        let Some(day) = day else {
            return "No date".into();
        };
        if day == self.today {
            "Today".into()
        } else if self.today.yesterday().is_ok_and(|y| y == day) {
            "Yesterday".into()
        } else if day.year() == self.today.year() {
            day.strftime("%A, %B %-d").to_string()
        } else {
            day.strftime("%B %-d, %Y").to_string()
        }
    }
}

fn zoned(timestamp: Option<i64>, tz: &TimeZone) -> Option<Zoned> {
    let t = Timestamp::from_second(timestamp?).ok()?;
    Some(t.to_zoned(tz.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> i64 {
        rfc3339.parse::<Timestamp>().unwrap().as_second()
    }

    #[test]
    fn times_follow_the_users_clock() {
        // In a fixed time zone, not the system's that `time` and `date` use.
        let tz = TimeZone::fixed(jiff::tz::offset(8));
        let show = |s, format| {
            zoned(Some(at(s)), &tz)
                .unwrap()
                .strftime(format)
                .to_string()
        };
        let afternoon = "2026-10-19T14:05:00+08:00";
        assert_eq!(show(afternoon, Clock::TwentyFourHour.time()), "14:05");
        assert_eq!(show(afternoon, Clock::TwelveHour.time()), "2:05 PM");
        assert_eq!(
            show("2026-10-19T00:30:00+08:00", Clock::TwelveHour.time()),
            "12:30 AM"
        );
        assert_eq!(
            show("2026-10-19T09:07:00+08:00", Clock::TwentyFourHour.time()),
            "09:07"
        );
        assert_eq!(
            show(afternoon, Clock::TwentyFourHour.date()),
            "2026-10-19 14:05"
        );
        assert_eq!(
            show(afternoon, Clock::TwelveHour.date()),
            "2026-10-19 2:05 PM"
        );
        assert_eq!(date(None, Clock::TwelveHour), "");
    }

    #[test]
    fn day_headings_depend_on_how_long_ago() {
        let tz = TimeZone::fixed(jiff::tz::offset(8));
        let today = "2026-10-19T15:00:00+08:00"
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned(tz.clone())
            .date();
        let calendar = Calendar { tz, today };
        let day = |s| calendar.heading(calendar.day(Some(at(s))));
        assert_eq!(day("2026-10-19T01:30:00+08:00"), "Today");
        // Still the 18th in UTC+8, though the 19th in UTC: the day is the local one.
        assert_eq!(day("2026-10-18T15:59:00Z"), "Yesterday");
        assert_eq!(day("2026-10-18T16:01:00Z"), "Today");
        assert_eq!(day("2026-10-17T23:59:00+08:00"), "Saturday, October 17");
        assert_eq!(day("2026-01-01T00:00:00+08:00"), "Thursday, January 1");
        assert_eq!(day("2025-12-31T09:00:00+08:00"), "December 31, 2025");
        assert_eq!(calendar.heading(calendar.day(None)), "No date");
    }
}
