//! MIME helpers on top of `mail-parser`: summary headers, and the readable content of
//! a message (text, sanitized HTML, attachments), from the raw message or from its
//! BODYSTRUCTURE plus selected parts. Outgoing messages are built with `mail-builder`.

mod build;
mod content;
mod css;
mod dark;
mod html;
mod structure;
mod tokens;

use std::borrow::Cow;

use catmail_types::Address;
use encoding_rs::{Encoding, UTF_8};
use mail_parser::{HeaderValue, MessageParser};

pub use build::{InvalidHeader, Outgoing, OutgoingAttachment, build_message};
pub use content::{Content, Part, content_id_from_url, decode_transfer, parse_message};
pub use dark::DarkPalette;
pub use html::{
    ColorUsage, InlineImage, MAX_REMOTE_IMAGES, color_usage, embed_inline_images, image_sources,
    recolor_html, remote_image_url, remote_image_urls, sanitize_html,
};
pub use structure::{Leaf, MimeNode, Params};

/// The header fields the sync engine fetches for message lists (`HEADER.FIELDS`).
pub const SUMMARY_HEADER_FIELDS: &[&str] = &[
    "DATE",
    "FROM",
    "TO",
    "CC",
    "SUBJECT",
    "MESSAGE-ID",
    "IN-REPLY-TO",
    "REFERENCES",
    "REPLY-TO",
];

/// Decoded summary headers. RFC 2047 encoded words and legacy charsets (GBK, Big5,
/// ISO-2022-JP, …) are already converted to UTF-8, and so are fields in raw 8-bit bytes
/// for which there was a hint of the charset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    pub subject: Option<String>,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    /// The Date header as unix seconds.
    pub date: Option<i64>,
    /// Without angle brackets.
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
}

/// Parses a header block (or a whole message, whose body is ignored). Never fails:
/// unparsable fields are left empty. A field in raw 8-bit bytes is read in the first of
/// the charsets `hints` that makes sense of it: those of the message's text parts.
pub fn parse_headers(raw: &[u8], hints: &[&str]) -> Headers {
    summary_headers(&transcode_fields(raw, hints))
}

fn summary_headers(raw: &[u8]) -> Headers {
    let Some(msg) = MessageParser::new().parse_headers(raw) else {
        return Headers::default();
    };
    let addresses = |a: Option<&mail_parser::Address<'_>>| -> Vec<Address> {
        a.map(|a| {
            a.iter()
                .filter_map(|addr| {
                    Some(Address {
                        name: addr
                            .name
                            .as_deref()
                            .map(str::trim)
                            .filter(|n| !n.is_empty())
                            .map(String::from),
                        email: addr.address.as_deref()?.trim().to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    };
    Headers {
        subject: msg.subject().map(String::from),
        from: addresses(msg.from()),
        to: addresses(msg.to()),
        cc: addresses(msg.cc()),
        reply_to: addresses(msg.reply_to()),
        date: msg.date().map(|d| d.to_timestamp()),
        message_id: msg
            .message_id()
            .filter(|id| fits_in_angle_brackets(id))
            .map(String::from),
        in_reply_to: id_list(msg.in_reply_to()),
        references: id_list(msg.references()),
    }
}

/// The header fields of `raw` (a header block) as UTF-8, with each field in raw 8-bit
/// bytes that aren't UTF-8 read in the first of the charsets `hints` that decodes all of
/// it. Some mail programs write the sender's local charset (GBK, …) instead of RFC 2047 /
/// 2231 encoding, and mail-parser reads such bytes as UTF-8, each one a U+FFFD. A field
/// is converted before it is parsed: a byte of a GBK character can be `\` or `"`.
/// Borrows `raw` if no field was converted.
pub(crate) fn transcode_fields<'a>(raw: &'a [u8], hints: &[&str]) -> Cow<'a, [u8]> {
    if std::str::from_utf8(raw).is_ok() {
        return Cow::Borrowed(raw);
    }
    let encodings = legacy_encodings(hints);
    if encodings.is_empty() {
        return Cow::Borrowed(raw);
    }
    let Some(message) = MessageParser::new().parse_headers(raw) else {
        return Cow::Borrowed(raw);
    };
    let mut fields = String::new();
    let mut converted = false;
    for header in message.headers() {
        let Some(field) = raw.get(header.offset_field as usize..header.offset_end as usize) else {
            return Cow::Borrowed(raw);
        };
        if let Ok(field) = std::str::from_utf8(field) {
            fields.push_str(field);
        } else if let Some(field) = encodings
            .iter()
            .find_map(|e| e.decode_without_bom_handling_and_without_replacement(field))
        {
            fields.push_str(&field);
            converted = true;
        } else {
            fields.push_str(&String::from_utf8_lossy(field));
        }
    }
    fields.push_str("\r\n");
    if converted {
        Cow::Owned(fields.into_bytes())
    } else {
        Cow::Borrowed(raw)
    }
}

/// The encodings named by the charset `labels` that hint at the charset of fields in raw
/// 8-bit bytes, in order and once each. Only those that keep ASCII as it is: others
/// (UTF-16, ISO-2022-JP) would turn the field's name, colon and line end into something
/// else. Not UTF-8 (those fields aren't), nor ASCII, which says nothing about the
/// sender's charset (and its labels name windows-1252, which takes any byte).
fn legacy_encodings(labels: &[&str]) -> Vec<&'static Encoding> {
    let mut encodings: Vec<&'static Encoding> = Vec::new();
    for label in labels {
        let ascii = ["us-ascii", "ascii", "ansi_x3.4-1968"]
            .iter()
            .any(|known| label.trim().eq_ignore_ascii_case(known));
        if let Some(encoding) = Encoding::for_label(label.as_bytes())
            && encoding.is_ascii_compatible()
            && encoding != UTF_8
            && !ascii
            && !encodings.contains(&encoding)
        {
            encodings.push(encoding);
        }
    }
    encodings
}

fn id_list(value: &HeaderValue<'_>) -> Vec<String> {
    value
        .as_text_list()
        .map(|ids| {
            ids.iter()
                .filter(|id| fits_in_angle_brackets(id))
                .map(|id| id.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// What may stand between `<` and `>` in a header or an SMTP command: visible ASCII
/// other than the brackets. mail-parser keeps control characters in Message-IDs, and a
/// bare CR would become a line break on the way out (SMTP and IMAP want CRLF), starting
/// a header or command of the sender's choosing.
pub(crate) fn fits_in_angle_brackets(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'<' | b'>'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_encoded_words_and_addresses() {
        let raw = b"From: =?UTF-8?B?5byg5LiJ?= <zhang@example.com>\r\n\
            To: a@example.com, \"B\" <b@example.com>\r\n\
            Cc: Team: c@example.com, d@example.com;\r\n\
            Subject: =?UTF-8?B?5L2g5aW9?= world\r\n\
            Date: Sat, 17 Oct 2026 09:30:00 +0800\r\n\
            Message-ID: <abc@example.com>\r\n\
            In-Reply-To: <parent@example.com>\r\n\
            References: <root@example.com>\r\n <parent@example.com>\r\n\
            Reply-To: list@example.com\r\n\
            \r\n";
        let h = parse_headers(raw, &[]);
        assert_eq!(h.subject.as_deref(), Some("你好 world"));
        assert_eq!(
            h.from,
            vec![Address {
                name: Some("张三".into()),
                email: "zhang@example.com".into()
            }]
        );
        let emails = |v: &[Address]| v.iter().map(|a| a.email.clone()).collect::<Vec<_>>();
        assert_eq!(emails(&h.to), ["a@example.com", "b@example.com"]);
        assert_eq!(emails(&h.cc), ["c@example.com", "d@example.com"]);
        assert_eq!(emails(&h.reply_to), ["list@example.com"]);
        assert_eq!(h.date, Some(1_792_200_600));
        assert_eq!(h.message_id.as_deref(), Some("abc@example.com"));
        assert_eq!(h.in_reply_to, ["parent@example.com"]);
        assert_eq!(h.references, ["root@example.com", "parent@example.com"]);
    }

    #[test]
    fn drops_message_ids_that_could_break_a_line() {
        let h = parse_headers(
            b"Message-ID: <evil\rX-Injected: yes@x>\r\n\
              References: <ok@x> <r1\rX-R: 1@x>\r\n\r\n",
            &[],
        );
        assert_eq!(h.message_id, None);
        assert_eq!(h.references, ["ok@x"]);
    }

    #[test]
    fn decodes_legacy_charsets() {
        // "测试" in GBK, both as an encoded word and as raw 8-bit bytes.
        let mut raw = b"Subject: =?GBK?B?suLK1A==?=\r\nFrom: ".to_vec();
        raw.extend_from_slice(&[0xb2, 0xe2, 0xca, 0xd4]);
        raw.extend_from_slice(b" <t@example.com>\r\n\r\n");
        let h = parse_headers(&raw, &["gbk"]);
        assert_eq!(h.subject.as_deref(), Some("测试"));
        assert_eq!(h.from[0].name.as_deref(), Some("测试"));
        assert_eq!(h.from[0].email, "t@example.com");

        // Without a hint of the charset, the raw bytes are lost.
        let h = parse_headers(&raw, &[]);
        assert_eq!(h.subject.as_deref(), Some("测试"));
        assert!(h.from[0].name.as_deref().unwrap().contains('\u{FFFD}'));
    }

    #[test]
    fn reads_each_raw_field_in_the_first_charset_that_fits() {
        // 张三 in GBK; its last byte is invalid in Shift_JIS. The UTF-8 field is left
        // alone.
        let mut raw = b"From: \"\xd5\xc5\xc8\xfd\" <z@example.com>\r\n".to_vec();
        raw.extend_from_slice("To: 李四 <l@example.com>\r\n\r\n".as_bytes());
        let h = parse_headers(&raw, &["shift_jis", "gbk"]);
        assert_eq!(h.from[0].name.as_deref(), Some("张三"));
        assert_eq!(h.to[0].name.as_deref(), Some("李四"));

        // 乗 in GBK ends in a backslash, which mustn't escape the closing quote.
        let h = parse_headers(b"Cc: \"\x81\x5c\" <c@example.com>\r\n\r\n", &["gbk"]);
        assert_eq!(h.cc[0].name.as_deref(), Some("乗"));
        assert_eq!(h.cc[0].email, "c@example.com");
    }

    #[test]
    fn hints_that_would_change_ascii_are_not_used() {
        let raw = b"Subject: caf\xe9s\r\nTo: a@example.com\r\n\r\n";
        // UTF-16 would take the whole field (an even number of bytes), name and line end
        // included, for text.
        let h = parse_headers(raw, &["utf-16le", "iso-8859-1"]);
        assert_eq!(h.subject.as_deref(), Some("cafés"));
        assert_eq!(h.to[0].email, "a@example.com");
        let h = parse_headers(raw, &["utf-16be", "iso-2022-jp"]);
        assert!(h.subject.unwrap().contains('\u{FFFD}'));
        assert_eq!(h.to[0].email, "a@example.com");
    }

    #[test]
    fn hints_skip_utf8_ascii_unknown_and_repeats() {
        let names = |labels: &[&str]| -> Vec<&str> {
            legacy_encodings(labels).iter().map(|e| e.name()).collect()
        };
        assert_eq!(
            names(&[
                "UTF-8",
                "unicode-1-1-utf-8",
                "gbk",
                "us-ascii",
                "ANSI_X3.4-1968"
            ]),
            ["GBK"]
        );
        assert_eq!(
            names(&["gb2312", "GBK", "x-unknown", "big5"]),
            ["GBK", "Big5"]
        );
    }

    #[test]
    fn tolerates_garbage() {
        assert_eq!(parse_headers(b"", &[]), Headers::default());
        let h = parse_headers(b"Date: yesterday\r\nFrom: nobody\r\n\r\n", &[]);
        assert_eq!(h.date, None);
    }
}
