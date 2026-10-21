//! How message metadata reads in the window: what `catmail_core::display` doesn't say,
//! the time of day and the list's day headings.

use jiff::civil::Date;
use jiff::{Timestamp, Zoned, tz::TimeZone};

/// What reads the same as in the CLI.
pub use catmail_core::display::{addresses, date, names, sender, size, subject};

/// The time of day, as a list row shows it under its day's heading: `14:05`.
pub fn time(timestamp: Option<i64>) -> String {
    catmail_core::display::local(timestamp, "%H:%M")
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
