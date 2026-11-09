//! How message metadata reads in the window: what `catmail_core::display` doesn't say,
//! the times on the user's clock and the list's day headings.

use std::time::Duration;

use jiff::civil::Date;
use jiff::{Timestamp, Zoned, tz::TimeZone};

/// What reads the same as in the CLI.
pub use catmail_core::display::{addresses, names, sender, size, subject};

/// The user's clock: the time zone and 12- or 24-hour time, the system's settings, else
/// their region's. The CLI keeps the system's zone and 24-hour time, which scripts can
/// read.
#[derive(Debug, Clone, PartialEq)]
pub struct Clock {
    pub zone: TimeZone,
    pub hours: Hours,
}

/// How the time of day reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hours {
    /// `14:05`.
    TwentyFour,
    /// `2:05 PM`.
    Twelve,
}

impl Hours {
    fn time(self) -> &'static str {
        match self {
            Hours::TwentyFour => "%H:%M",
            Hours::Twelve => "%-I:%M %p",
        }
    }

    fn date(self) -> &'static str {
        match self {
            Hours::TwentyFour => "%Y-%m-%d %H:%M",
            Hours::Twelve => "%Y-%m-%d %-I:%M %p",
        }
    }
}

/// The zone a platform names: a tz database name (`Europe/Berlin`), or an offset from
/// GMT (`GMT+01:00`, as Android names one where the database has none); else the zone
/// the C library reads.
pub fn zone(name: &str) -> TimeZone {
    TimeZone::get(name)
        .ok()
        .or_else(|| offset_zone(name))
        .unwrap_or_else(TimeZone::system)
}

fn offset_zone(name: &str) -> Option<TimeZone> {
    let offset = name.strip_prefix("GMT")?;
    let sign = match offset.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let (hours, minutes) = offset[1..].split_once(':')?;
    let (hours, minutes): (i32, i32) = (hours.parse().ok()?, minutes.parse().ok()?);
    if !(0..60).contains(&minutes) {
        return None;
    }
    let offset = jiff::tz::Offset::from_seconds(sign * (hours * 3600 + minutes * 60)).ok()?;
    Some(TimeZone::fixed(offset))
}

impl Clock {
    fn show(&self, timestamp: Option<i64>, format: &str) -> String {
        zoned(timestamp, &self.zone)
            .map(|t| t.strftime(format).to_string())
            .unwrap_or_default()
    }
}

/// The time of day, as a list row shows it under its day's heading: `14:05`.
pub fn time(timestamp: Option<i64>, clock: &Clock) -> String {
    clock.show(timestamp, clock.hours.time())
}

/// A date and time in full, as the reader's header and the outbox show it:
/// `2026-10-19 14:05`. Empty for none.
pub fn date(timestamp: Option<i64>, clock: &Clock) -> String {
    clock.show(timestamp, clock.hours.date())
}

/// The local calendar as of one moment, to put many rows under day headings with one
/// look at the clock. Another zone, or another day come, is another calendar: the rows
/// are grouped again.
#[derive(Debug, Clone, PartialEq)]
pub struct Calendar {
    tz: TimeZone,
    today: Date,
}

impl Calendar {
    /// Today in `zone`.
    pub fn now(zone: &TimeZone) -> Self {
        Self::at(&Zoned::now().with_time_zone(zone.clone()))
    }

    /// As of `now`, in its zone.
    pub fn at(now: &Zoned) -> Self {
        Self {
            tz: now.time_zone().clone(),
            today: now.date(),
        }
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

/// How long until the next day starts in `zone`, when the day headings change; `None`
/// past the last day jiff counts.
pub fn until_tomorrow(zone: &TimeZone) -> Option<Duration> {
    until_tomorrow_from(&Zoned::now().with_time_zone(zone.clone()))
}

fn until_tomorrow_from(now: &Zoned) -> Option<Duration> {
    let tomorrow = now.tomorrow().and_then(|t| t.start_of_day()).ok()?;
    Duration::try_from(tomorrow.duration_since(now)).ok()
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

    fn clock(hours: i8, clock: Hours) -> Clock {
        Clock {
            zone: TimeZone::fixed(jiff::tz::offset(hours)),
            hours: clock,
        }
    }

    #[test]
    fn times_follow_the_users_clock() {
        let beijing = clock(8, Hours::TwentyFour);
        let afternoon = Some(at("2026-10-19T14:05:00+08:00"));
        assert_eq!(time(afternoon, &beijing), "14:05");
        assert_eq!(date(afternoon, &beijing), "2026-10-19 14:05");
        let twelve = clock(8, Hours::Twelve);
        assert_eq!(time(afternoon, &twelve), "2:05 PM");
        assert_eq!(date(afternoon, &twelve), "2026-10-19 2:05 PM");
        assert_eq!(
            time(Some(at("2026-10-19T00:30:00+08:00")), &twelve),
            "12:30 AM"
        );
        assert_eq!(
            time(Some(at("2026-10-19T09:07:00+08:00")), &beijing),
            "09:07"
        );
        assert_eq!(date(None, &twelve), "");
    }

    #[test]
    fn times_are_in_the_users_zone() {
        // One moment, in Berlin's summer time and in Beijing: another day there.
        let moment = Some(at("2026-10-19T19:30:00Z"));
        assert_eq!(time(moment, &clock(2, Hours::TwentyFour)), "21:30");
        assert_eq!(
            date(moment, &clock(8, Hours::TwentyFour)),
            "2026-10-20 03:30"
        );
    }

    #[test]
    fn zones_are_found_by_the_names_platforms_give() {
        let moment = at("2026-10-19T12:00:00Z");
        let offset = |name: &str| {
            let t = Timestamp::from_second(moment).unwrap();
            zone(name).to_offset(t).seconds()
        };
        assert_eq!(offset("Asia/Shanghai"), 8 * 3600);
        assert_eq!(offset("Europe/Berlin"), 2 * 3600, "summer time");
        assert_eq!(offset("GMT+01:00"), 3600);
        assert_eq!(offset("GMT-05:30"), -(5 * 3600 + 30 * 60));
        assert_eq!(offset_zone("GMT+1"), None);
        assert_eq!(offset_zone("GMT+01:75"), None);
        assert_eq!(zone("Not/AZone"), TimeZone::system());
    }

    #[test]
    fn day_headings_depend_on_how_long_ago() {
        let tz = TimeZone::fixed(jiff::tz::offset(8));
        let calendar = Calendar::at(&"2026-10-19T15:00:00+08:00[+08:00]".parse().unwrap());
        assert_eq!(calendar.tz, tz);
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

    #[test]
    fn the_next_day_starts_at_local_midnight() {
        let until = |now: &str| until_tomorrow_from(&now.parse().unwrap()).unwrap();
        assert_eq!(
            until("2026-10-19T23:59:30+08:00[+08:00]"),
            Duration::from_secs(30)
        );
        assert_eq!(
            until("2026-10-19T00:00:00-05:00[-05:00]"),
            Duration::from_secs(24 * 3600)
        );
    }
}
