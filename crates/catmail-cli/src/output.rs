//! Terminal output: escaping text from strangers, aligned tables, dates and sizes.

use std::borrow::Cow;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Shortens to at most `width` terminal columns, marking the cut with `…`.
pub(crate) fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Escapes characters that a terminal would act on rather than show: C0/C1 controls
/// (ESC sequences, line breaks) and bidi overrides that reorder the rest of the line.
/// Subjects, names and server messages come from strangers.
pub(crate) fn printable(s: &str) -> Cow<'_, str> {
    if !s.chars().any(is_unsafe) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        if is_unsafe(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Left-aligned columns, padded by display width so CJK text lines up. Cells are
/// escaped with [`printable`] before measuring.
pub(crate) fn print_table(header: &[&str], rows: Vec<Vec<String>>) {
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| match printable(&cell) {
                    Cow::Owned(escaped) => escaped,
                    Cow::Borrowed(_) => cell,
                })
                .collect()
        })
        .collect();
    let header: Vec<String> = header.iter().map(|h| h.to_string()).collect();
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            rows.iter()
                .chain([&header])
                .map(|r| r[i].width())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in [&header].into_iter().chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell}{}", " ".repeat(w - cell.width())))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}

/// C0/C1 controls and bidi overrides: what a terminal (or a file manager) would act on
/// or reorder rather than show.
pub(crate) fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Like [`printable`], for multi-line text such as a message body: line breaks and
/// tabs are kept (CRLF becomes LF), everything else unsafe is escaped.
pub(crate) fn printable_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\t' => out.push(c),
            c if is_unsafe(c) => out.extend(c.escape_unicode()),
            c => out.push(c),
        }
    }
    out
}

/// `2026-10-17 09:30` in the local time zone.
pub(crate) fn date(timestamp: Option<i64>) -> String {
    timestamp
        .and_then(|t| jiff::Timestamp::from_second(t).ok())
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// `512 B`, `1.5 KB`, `12.3 MB` (powers of 1024).
pub(crate) fn size(bytes: u64) -> String {
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

    #[test]
    fn printable_escapes_terminal_controls() {
        // =?UTF-8?B?G1sySnNwb29m?= decodes to ESC [2J followed by "spoof".
        assert_eq!(printable("\u{1b}[2Jspoof"), "\\u{1b}[2Jspoof");
        assert_eq!(printable("a\nb\tc\u{9b}"), "a\\u{a}b\\u{9}c\\u{9b}");
        assert_eq!(printable("evil\u{202e}fdp.exe"), "evil\\u{202e}fdp.exe");
        assert!(matches!(
            printable("你好，世界 ok"),
            Cow::Borrowed("你好，世界 ok")
        ));
    }

    #[test]
    fn truncate_counts_display_width() {
        assert_eq!(truncate("张三丰", 5), "张三…");
        assert_eq!(truncate("abc", 5), "abc");
    }

    #[test]
    fn printable_text_keeps_lines() {
        assert_eq!(
            printable_text("a\r\nb\tc\n\u{1b}[31m"),
            "a\nb\tc\n\\u{1b}[31m"
        );
        assert_eq!(printable_text("x\ry"), "x\\u{d}y");
    }

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(3 * 1024 * 1024), "3.0 MB");
    }
}
