//! Modified UTF-7 mailbox names (RFC 3501 §5.1.3).
//!
//! IMAP servers without UTF8=ACCEPT send non-ASCII mailbox names as `&`-escaped,
//! modified-base64 UTF-16BE, e.g. `&V4NXPpCuTvY-` for `垃圾邮件`.

use base64::Engine;
use base64::alphabet::Alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

/// Standard base64 with `,` for `/` and no padding. Trailing bits must be zero.
const MODIFIED_BASE64: GeneralPurpose = GeneralPurpose::new(
    &match Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,") {
        Ok(alphabet) => alphabet,
        Err(_) => panic!("valid alphabet"),
    },
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone),
);

/// Decodes a wire mailbox name for display. Returns `None` if it is not valid
/// modified UTF-7. A name that already contains non-ASCII characters (sent by a
/// UTF8=ACCEPT server) is returned unchanged.
pub fn decode(name: &str) -> Option<String> {
    if !name.is_ascii() {
        return Some(name.to_owned());
    }
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let end = after.find('-')?;
        let encoded = &after[..end];
        if encoded.is_empty() {
            out.push('&');
        } else {
            out.push_str(&decode_utf16_run(encoded)?);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Encodes a mailbox name for the wire.
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush_utf16_run(&mut out, &mut pending);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush_utf16_run(&mut out, &mut pending);
    out
}

fn decode_utf16_run(encoded: &str) -> Option<String> {
    let bytes = MODIFIED_BASE64.decode(encoded).ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_be_bytes(pair))
        .collect();
    let decoded = String::from_utf16(&units).ok()?;
    // A run that only encodes printable ASCII is not canonical; reject it so that
    // `encode(decode(x)) == x` holds for everything we accept.
    if decoded.chars().any(|c| (' '..='~').contains(&c)) {
        return None;
    }
    Some(decoded)
}

fn flush_utf16_run(out: &mut String, units: &mut Vec<u16>) {
    if units.is_empty() {
        return;
    }
    let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_be_bytes()).collect();
    out.push('&');
    MODIFIED_BASE64.encode_string(bytes, out);
    out.push('-');
    units.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for (wire, name) in [
            ("INBOX", "INBOX"),
            ("Tom &- Jerry", "Tom & Jerry"),
            ("&V4NXPpCuTvY-", "垃圾邮件"),
            ("&XfJT0ZAB-", "已发送"),
            ("Archive/&ZeVnLIqe-", "Archive/日本語"),
            ("~peter/mail/&U,BTFw-/&ZeVnLIqe-", "~peter/mail/台北/日本語"),
            ("&2D3eAA-", "😀"),
        ] {
            assert_eq!(decode(wire).as_deref(), Some(name), "{wire}");
            assert_eq!(encode(name), wire, "{name}");
        }
    }

    #[test]
    fn utf8_names_pass_through() {
        assert_eq!(decode("已发送").as_deref(), Some("已发送"));
    }

    #[test]
    fn rejects_invalid() {
        for bad in ["&", "&abc", "&Jjo!-", "&AGE-", "&2D3-"] {
            assert_eq!(decode(bad), None, "{bad}");
        }
    }
}
