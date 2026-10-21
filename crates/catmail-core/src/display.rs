//! How message metadata reads, the same in every frontend: sizes, dates, names, subjects.
//! Frontends escape or style the text themselves.

use catmail_types::Address;

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

/// The full date in the local time zone, `2026-10-19 14:05`; empty for none.
pub fn date(timestamp: Option<i64>) -> String {
    local(timestamp, "%Y-%m-%d %H:%M")
}

/// The time in the local time zone, as `format` (strftime) has it; empty for none.
pub fn local(timestamp: Option<i64>, format: &str) -> String {
    timestamp
        .and_then(|t| jiff::Timestamp::from_second(t).ok())
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime(format)
                .to_string()
        })
        .unwrap_or_default()
}

/// The name, or the address where there is none.
fn name(address: &Address) -> &str {
    address.name.as_deref().unwrap_or(&address.email)
}

/// The names alone (addresses where there is no name), comma separated: `Bob, Carol`.
pub fn names<'a>(list: impl IntoIterator<Item = &'a Address>) -> String {
    list.into_iter().map(name).collect::<Vec<_>>().join(", ")
}

/// `Name <a@example.com>` for each address, comma separated.
pub fn addresses(list: &[Address]) -> String {
    list.iter()
        .map(Address::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The sender as a list shows it: the first name, or its address.
pub fn sender(from: &[Address]) -> &str {
    from.first().map(name).unwrap_or_default()
}

/// The subject, or a placeholder for none (or only spaces).
pub fn subject(subject: Option<&str>) -> &str {
    match subject.map(str::trim) {
        Some(subject) if !subject.is_empty() => subject,
        _ => "(no subject)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(512), "512 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn names_and_addresses() {
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
        assert_eq!(names(&list), "张三, b@example.com");
        assert_eq!(sender(&list), "张三");
        assert_eq!(sender(&list[1..]), "b@example.com");
        assert_eq!(sender(&[]), "");
    }

    #[test]
    fn subjects() {
        assert_eq!(subject(Some(" Hi ")), "Hi");
        assert_eq!(subject(Some("  ")), "(no subject)");
        assert_eq!(subject(None), "(no subject)");
    }

    #[test]
    fn dates() {
        assert_eq!(date(None), "");
        assert_eq!(date(Some(0)).len(), "1970-01-01 00:00".len());
    }
}
