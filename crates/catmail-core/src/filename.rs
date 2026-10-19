//! File names for attachments, which strangers choose.

/// Longest file name we create, in bytes; most file systems allow 255.
const MAX_NAME_BYTES: usize = 200;

/// A file name for saving an attachment that is safe on every platform: no directories,
/// no characters Windows rejects, no control or bidi override characters (which make
/// `invoice\u{202e}fdp.exe` look like a PDF), no reserved device names, not empty.
/// `fallback` is used when nothing usable is left. Frontends still have to avoid
/// overwriting existing files.
pub fn safe_filename(name: Option<&str>, fallback: &str) -> String {
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

fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
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
}
