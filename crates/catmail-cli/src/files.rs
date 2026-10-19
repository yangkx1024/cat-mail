//! Writing attachments to disk under names chosen by strangers.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::output::is_unsafe;

/// Longest file name we create, in bytes; most file systems allow 255.
const MAX_NAME_BYTES: usize = 200;

/// A file name that is safe on every platform: no directories, no characters Windows
/// rejects or terminals act on, no reserved device names, not empty. `fallback` is used
/// when nothing usable is left.
pub(crate) fn safe_filename(name: Option<&str>, fallback: &str) -> String {
    let base = name
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let mut cleaned: String = base
        .chars()
        .map(|c| {
            if is_unsafe(c) || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Windows drops trailing dots and spaces; leading dots hide files on Unix.
    cleaned = cleaned
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_owned();
    if cleaned.is_empty() {
        cleaned = fallback.to_owned();
    }
    let stem = cleaned.split('.').next().unwrap_or_default();
    let reserved = matches!(
        stem.to_uppercase().as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|device| {
        stem.get(..3)
            .is_some_and(|p| p.eq_ignore_ascii_case(device))
            && matches!(
                &stem[3..],
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
    });
    if reserved {
        cleaned.insert(0, '_');
    }
    shorten(cleaned)
}

/// Keeps the extension when cutting a long name.
fn shorten(name: String) -> String {
    if name.len() <= MAX_NAME_BYTES {
        return name;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if ext.len() < 16 => (stem, format!(".{ext}")),
        _ => (name.as_str(), String::new()),
    };
    let mut end = MAX_NAME_BYTES - ext.len();
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ext}", &stem[..end])
}

/// Writes `data` to `dir/name`, or `dir/name (1)` etc. if that exists. Never
/// overwrites. Returns the path written.
pub(crate) fn write_new(dir: &Path, name: &str, data: &[u8]) -> io::Result<PathBuf> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    for n in 0.. {
        let candidate = if n == 0 {
            dir.join(name)
        } else {
            dir.join(format!("{stem} ({n}){ext}"))
        };
        match File::options()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(data)?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("ran out of numbers")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(safe_filename(Some("报告.pdf"), "x"), "报告.pdf");
        assert_eq!(safe_filename(Some("../../etc/passwd"), "x"), "passwd");
        assert_eq!(
            safe_filename(Some("C:\\Windows\\evil.exe"), "x"),
            "evil.exe"
        );
        assert_eq!(safe_filename(Some("a<b>:c|d?.txt"), "x"), "a_b__c_d_.txt");
        assert_eq!(safe_filename(Some("\u{1b}[2Jx.txt"), "x"), "_[2Jx.txt");
        assert_eq!(safe_filename(Some(" .hidden. "), "x"), "hidden");
        assert_eq!(safe_filename(Some(".."), "attachment-3"), "attachment-3");
        assert_eq!(safe_filename(None, "attachment-3"), "attachment-3");
        assert_eq!(safe_filename(Some("con.txt"), "x"), "_con.txt");
        assert_eq!(safe_filename(Some("COM0.log"), "x"), "_COM0.log");
        assert_eq!(safe_filename(Some("lpt¹"), "x"), "_lpt¹");
        assert_eq!(safe_filename(Some("CONOUT$"), "x"), "_CONOUT$");
        assert_eq!(safe_filename(Some("COM10"), "x"), "COM10");
        assert_eq!(safe_filename(Some("😀x.txt"), "x"), "😀x.txt");
        assert_eq!(
            safe_filename(Some("invoice\u{202e}fdp.exe"), "x"),
            "invoice_fdp.exe"
        );
        let long = format!("{}.pdf", "字".repeat(100));
        let short = safe_filename(Some(&long), "x");
        assert!(
            short.len() <= MAX_NAME_BYTES && short.ends_with("字.pdf"),
            "{short}"
        );
    }

    #[test]
    fn never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_new(dir.path(), "a.txt", b"1").unwrap();
        let second = write_new(dir.path(), "a.txt", b"2").unwrap();
        let third = write_new(dir.path(), "noext", b"3").unwrap();
        let fourth = write_new(dir.path(), "noext", b"4").unwrap();
        assert_eq!(first, dir.path().join("a.txt"));
        assert_eq!(second, dir.path().join("a (1).txt"));
        assert_eq!(fourth, dir.path().join("noext (1)"));
        assert_eq!(std::fs::read(first).unwrap(), b"1");
        assert_eq!(std::fs::read(third).unwrap(), b"3");
    }
}
