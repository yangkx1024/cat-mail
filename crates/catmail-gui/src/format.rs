//! How message metadata reads in the window.

use catmail_core::Address;
use jiff::civil::Date;
use jiff::{Timestamp, Zoned, tz::TimeZone};

/// `Name <a@example.com>` for each address, comma separated.
pub fn addresses(list: &[Address]) -> String {
    list.iter()
        .map(Address::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The name, or the address where there is none.
fn name(address: &Address) -> String {
    address
        .name
        .clone()
        .unwrap_or_else(|| address.email.clone())
}

/// The names alone (addresses where there is no name), comma separated: `Bob, Carol`.
pub fn names<'a>(list: impl IntoIterator<Item = &'a Address>) -> String {
    list.into_iter().map(name).collect::<Vec<_>>().join(", ")
}

/// The subject, or a placeholder for none.
pub fn subject(subject: Option<&str>) -> String {
    match subject.map(str::trim) {
        Some(subject) if !subject.is_empty() => subject.to_owned(),
        _ => "(no subject)".into(),
    }
}

/// The sender as a list row shows it: the first name, or its address.
pub fn sender(from: &[Address]) -> String {
    from.first().map(name).unwrap_or_default()
}

/// The full date, as in the reader's header: `2026-10-19 14:05`.
pub fn date(timestamp: Option<i64>) -> String {
    local(timestamp, "%Y-%m-%d %H:%M")
}

/// The time of day, as a list row shows it under its day's heading: `14:05`.
pub fn time(timestamp: Option<i64>) -> String {
    local(timestamp, "%H:%M")
}

fn local(timestamp: Option<i64>, format: &str) -> String {
    zoned(timestamp, &TimeZone::system())
        .map(|t| t.strftime(format).to_string())
        .unwrap_or_default()
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

/// `512 B`, `1.5 KB`, `12.3 MB` (powers of 1024).
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
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

    #[test]
    fn addresses_and_sender() {
        let list = [
            Address {
                name: Some("张三".into()),
                email: "zhang@example.com".into(),
            },
            Address {
                name: None,
                email: "b@example.com".into(),
            },
        ];
        assert_eq!(addresses(&list), "张三 <zhang@example.com>, b@example.com");
        assert_eq!(sender(&list), "张三");
        assert_eq!(sender(&list[1..]), "b@example.com");
        assert_eq!(sender(&[]), "");
    }

    #[test]
    fn sizes() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(3 * 1024 * 1024), "3.0 MB");
    }
}
