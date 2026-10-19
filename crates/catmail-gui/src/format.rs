//! How message metadata reads in the window.

use catmail_core::{Address, MessageSummary};
use jiff::{Timestamp, Zoned, tz::TimeZone};

/// `Name <a@example.com>` for each address, comma separated.
pub fn addresses(list: &[Address]) -> String {
    list.iter()
        .map(Address::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn subject(message: &MessageSummary) -> String {
    message
        .subject
        .clone()
        .unwrap_or_else(|| "(no subject)".into())
}

/// The sender as a list row shows it: the first name, or its address.
pub fn sender(from: &[Address]) -> String {
    from.first()
        .map(|a| a.name.clone().unwrap_or_else(|| a.email.clone()))
        .unwrap_or_default()
}

/// The full date, as in the reader's header: `2026-10-19 14:05`.
pub fn date(timestamp: Option<i64>) -> String {
    zoned(timestamp, &TimeZone::system())
        .map(|t| t.strftime("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// The date as a list row shows it: the time for today, the day for this year, the full
/// date otherwise.
pub fn short_date(timestamp: Option<i64>) -> String {
    let tz = TimeZone::system();
    short_date_at(timestamp, &Zoned::now().with_time_zone(tz.clone()), &tz)
}

fn short_date_at(timestamp: Option<i64>, now: &Zoned, tz: &TimeZone) -> String {
    let Some(t) = zoned(timestamp, tz) else {
        return String::new();
    };
    let format = if t.date() == now.date() {
        "%H:%M"
    } else if t.year() == now.year() {
        "%m-%d"
    } else {
        "%Y-%m-%d"
    };
    t.strftime(format).to_string()
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
    fn short_date_depends_on_how_long_ago() {
        let tz = TimeZone::fixed(jiff::tz::offset(8));
        let now = "2026-10-19T15:00:00+08:00"
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned(tz.clone());
        let short = |s| short_date_at(Some(at(s)), &now, &tz);
        assert_eq!(short("2026-10-19T01:30:00+08:00"), "01:30");
        // Still the 18th in UTC+8, though the 19th in UTC: the day is the local one.
        assert_eq!(short("2026-10-18T15:59:00Z"), "10-18");
        assert_eq!(short("2026-10-18T16:01:00Z"), "00:01");
        assert_eq!(short("2025-12-31T09:00:00+08:00"), "2025-12-31");
        assert_eq!(short_date_at(None, &now, &tz), "");
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
