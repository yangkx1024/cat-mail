//! MIME helpers on top of `mail-parser`: summary headers, and the readable content of
//! a message (text, sanitized HTML, attachments), from the raw message or from its
//! BODYSTRUCTURE plus selected parts. Outgoing messages are built with `mail-builder`.

mod build;
mod content;
mod css;
mod html;
mod structure;
mod tokens;

use catmail_types::Address;
use mail_parser::{HeaderValue, MessageParser};

pub use build::{InvalidHeader, Outgoing, OutgoingAttachment, build_message};
pub use content::{Content, Part, content_id_from_url, decode_transfer, parse_message};
pub use html::{
    ColorUsage, InlineImage, MAX_REMOTE_IMAGES, color_usage, embed_inline_images, image_sources,
    remote_image_url, remote_image_urls, sanitize_html,
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
/// ISO-2022-JP, …) are already converted to UTF-8.
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
/// unparsable fields are left empty.
pub fn parse_headers(raw: &[u8]) -> Headers {
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
        let h = parse_headers(raw);
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
        let h = parse_headers(&raw);
        assert_eq!(h.subject.as_deref(), Some("测试"));
        assert_eq!(h.from[0].email, "t@example.com");
    }

    #[test]
    fn tolerates_garbage() {
        assert_eq!(parse_headers(b""), Headers::default());
        let h = parse_headers(b"Date: yesterday\r\nFrom: nobody\r\n\r\n");
        assert_eq!(h.date, None);
    }
}
