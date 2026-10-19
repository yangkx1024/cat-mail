use std::collections::{HashMap, HashSet};

use mail_parser::parsers::MessageStream;
use mail_parser::{Encoding, Message, MessageParser, MimeHeaders, PartType};

use super::html::sanitize_html;

/// Longest snippet, in characters.
const SNIPPET_CHARS: usize = 200;

/// What a reader shows of a message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Content {
    /// Plain text; converted from HTML if the message has no text part.
    pub text: Option<String>,
    /// Sanitized HTML, if the message has an HTML part.
    pub html: Option<String>,
    /// The start of the text, on one line, for message lists.
    pub snippet: Option<String>,
    /// Attachments and inline parts, in MIME order.
    pub parts: Vec<Part>,
}

/// An attachment or inline part.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Part {
    /// IMAP body section, e.g. `2` or `1.2`.
    pub section: String,
    /// Content-Transfer-Encoding, lower-case.
    pub transfer_encoding: Option<String>,
    pub filename: Option<String>,
    /// Lower-case `type/subtype`.
    pub mime_type: String,
    /// Without angle brackets.
    pub content_id: Option<String>,
    /// Shown within the body rather than offered as a file: the HTML refers to it by
    /// `cid:`. (An image that nothing refers to is an attachment, wherever it sits.)
    pub inline: bool,
    /// Decoded size; estimated from the encoded size when `data` is `None`.
    pub size: Option<u32>,
    /// Decoded content; `None` if it wasn't fetched.
    pub data: Option<Vec<u8>>,
}

/// Extracts the content of a whole raw message. Never fails: a message mail-parser can't
/// make sense of has no content.
pub fn parse_message(raw: &[u8]) -> Content {
    match MessageParser::new().parse(raw) {
        Some(message) => extract(&message, raw),
        None => Content::default(),
    }
}

/// Undoes a Content-Transfer-Encoding (`base64`, `quoted-printable`; anything else is
/// returned as is), as leniently as mail-parser does for whole messages.
pub fn decode_transfer(encoding: Option<&str>, data: &[u8]) -> Vec<u8> {
    let mut stream = MessageStream::new(data);
    let (end, decoded) = match encoding.map(str::to_ascii_lowercase).as_deref() {
        Some("base64") => stream.decode_base64_mime(b""),
        Some("quoted-printable") => stream.decode_quoted_printable_mime(b""),
        _ => return data.to_vec(),
    };
    // Undecodable: keep the bytes, as mail-parser does for a message body.
    if end == usize::MAX {
        data.to_vec()
    } else {
        decoded.into_owned()
    }
}

/// `raw` is what `message` was parsed from. Binary parts come decoded from the parser;
/// text parts and attached messages are cut from `raw` and decoded again, so text keeps
/// its original bytes and charset (the parser converts it to UTF-8).
pub(super) fn extract(message: &Message<'_>, raw: &[u8]) -> Content {
    let text = joined(
        (0..message.text_body_count()).filter_map(|i| message.body_text(i)),
        "\n",
    );
    let has_html = message
        .html_body
        .iter()
        .any(|&id| matches!(message.parts[id as usize].body, PartType::Html(_)));
    let html = has_html
        .then(|| {
            joined(
                (0..message.html_body_count()).filter_map(|i| message.body_html(i)),
                "<br>",
            )
        })
        .flatten()
        .map(|html| sanitize_html(&html));

    let referenced = html.as_deref().map(cid_references).unwrap_or_default();
    let sections = sections(message);
    let parts = message
        .attachments
        .iter()
        .filter_map(|&id| {
            let part = &message.parts[id as usize];
            let data = match &part.body {
                PartType::Binary(data) | PartType::InlineBinary(data) => Some(data.to_vec()),
                _ => raw
                    .get(part.offset_body as usize..part.offset_end as usize)
                    .map(|encoded| {
                        let encoding = match part.encoding {
                            Encoding::Base64 => Some("base64"),
                            Encoding::QuotedPrintable => Some("quoted-printable"),
                            Encoding::None => None,
                        };
                        decode_transfer(encoding, encoded)
                    }),
            };
            let content_id = part
                .content_id()
                .map(|id| id.trim().trim_start_matches('<').trim_end_matches('>'))
                .filter(|id| !id.is_empty());
            Some(Part {
                section: sections.get(&id)?.clone(),
                transfer_encoding: part
                    .content_transfer_encoding()
                    .map(|e| e.trim().to_ascii_lowercase()),
                filename: part
                    .attachment_name()
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(String::from),
                mime_type: part
                    .content_type()
                    .map(|ct| match ct.subtype() {
                        Some(sub) => format!("{}/{sub}", ct.ctype()),
                        None => ct.ctype().to_owned(),
                    })
                    .unwrap_or_else(|| "text/plain".into())
                    .to_ascii_lowercase(),
                inline: content_id.is_some_and(|id| referenced.contains(id)),
                content_id: content_id.map(String::from),
                size: data.as_ref().map(|d| d.len() as u32),
                data,
            })
        })
        .collect();

    Content {
        snippet: text.as_deref().and_then(snippet),
        text,
        html,
        parts,
    }
}

fn joined<S: AsRef<str>>(pieces: impl Iterator<Item = S>, separator: &str) -> Option<String> {
    let pieces: Vec<S> = pieces.filter(|p| !p.as_ref().trim().is_empty()).collect();
    let pieces: Vec<&str> = pieces.iter().map(AsRef::as_ref).collect();
    (!pieces.is_empty()).then(|| pieces.join(separator))
}

/// The Content-IDs that `cid:` URLs in the HTML refer to (RFC 2392: the scheme in any
/// case, the rest percent-encoded).
fn cid_references(html: &str) -> HashSet<String> {
    let lower = html.to_ascii_lowercase();
    lower
        .match_indices("cid:")
        .map(|(start, _)| {
            let rest = &html[start + 4..];
            let end = rest
                .find(|c: char| c.is_whitespace() || "\"'()<>".contains(c))
                .unwrap_or(rest.len());
            percent_decode(&rest[..end])
        })
        .filter(|id| !id.is_empty())
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// IMAP section numbers of the parts of `message` (RFC 3501 §6.4.5): the children of a
/// multipart are numbered from 1 below its own number, and a message that isn't
/// multipart has the single part `1`. Attached messages count as one part.
pub(super) fn sections(message: &Message<'_>) -> HashMap<u32, String> {
    fn number(message: &Message<'_>, id: u32, section: String, out: &mut HashMap<u32, String>) {
        if let Some(children) = message.parts[id as usize].sub_parts() {
            for (i, &child) in children.iter().enumerate() {
                number(message, child, format!("{section}.{}", i + 1), out);
            }
        }
        out.insert(id, section);
    }
    let mut out = HashMap::new();
    match message.parts.first().and_then(|root| root.sub_parts()) {
        Some(children) => {
            for (i, &child) in children.iter().enumerate() {
                number(message, child, (i + 1).to_string(), &mut out);
            }
        }
        None => {
            out.insert(0, "1".into());
        }
    }
    out
}

/// The text on one line: quoted lines (`> …`) dropped, whitespace collapsed, cut at a
/// character boundary.
fn snippet(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = 0;
    for word in text
        .lines()
        .filter(|line| !line.trim_start().starts_with('>'))
        .flat_map(str::split_whitespace)
    {
        if chars > 0 {
            // Only if a character of the next word still fits after it.
            if chars + 1 >= SNIPPET_CHARS {
                break;
            }
            out.push(' ');
            chars += 1;
        }
        for c in word.chars() {
            if chars >= SNIPPET_CHARS {
                return Some(out);
            }
            out.push(c);
            chars += 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED: &[u8] = b"From: a@example.com\r\n\
        Subject: =?UTF-8?B?5oql5ZGK?=\r\n\
        MIME-Version: 1.0\r\n\
        Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
        \r\n\
        --outer\r\n\
        Content-Type: multipart/alternative; boundary=\"alt\"\r\n\
        \r\n\
        --alt\r\n\
        Content-Type: text/plain; charset=gbk\r\n\
        Content-Transfer-Encoding: base64\r\n\
        \r\n\
        x+uy6crV\r\n\
        --alt\r\n\
        Content-Type: text/html; charset=utf-8\r\n\
        \r\n\
        <p onclick=\"evil()\">\xe8\xaf\xb7\xe6\x9f\xa5\xe6\x94\xb6<script>alert(1)</script></p>\r\n\
        --alt--\r\n\
        \r\n\
        --outer\r\n\
        Content-Type: application/pdf; name=\"=?UTF-8?B?5oql5ZGKLnBkZg==?=\"\r\n\
        Content-Disposition: attachment\r\n\
        Content-Transfer-Encoding: base64\r\n\
        \r\n\
        JVBERi0xLjQK\r\n\
        --outer\r\n\
        Content-Type: text/plain; charset=gbk; name=notes.txt\r\n\
        Content-Disposition: attachment; filename*=utf-8''%E7%AC%94%E8%AE%B0.txt\r\n\
        \r\n\
        \xb1\xca\xbc\xc7\r\n\
        --outer--\r\n";

    #[test]
    fn extracts_text_html_and_attachments() {
        let content = parse_message(MIXED);
        // "请查收" in GBK, base64 encoded.
        assert_eq!(content.text.as_deref(), Some("请查收"));
        assert_eq!(content.snippet.as_deref(), Some("请查收"));
        let html = content.html.unwrap();
        assert!(html.contains("请查收"), "{html}");
        assert!(
            !html.contains("script") && !html.contains("onclick"),
            "{html}"
        );

        let parts: Vec<_> = content
            .parts
            .iter()
            .map(|p| {
                (
                    p.section.as_str(),
                    p.filename.as_deref(),
                    p.mime_type.as_str(),
                    p.inline,
                    p.data.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            parts,
            [
                (
                    "2",
                    Some("报告.pdf"),
                    "application/pdf",
                    false,
                    Some(&b"%PDF-1.4\n"[..])
                ),
                // Original bytes, not converted from GBK.
                (
                    "3",
                    Some("笔记.txt"),
                    "text/plain",
                    false,
                    Some(&b"\xb1\xca\xbc\xc7"[..])
                ),
            ]
        );
        assert_eq!(
            content.parts[0].transfer_encoding.as_deref(),
            Some("base64")
        );
        assert_eq!(content.parts[0].size, Some(9));
    }

    #[test]
    fn related_images_are_inline_with_content_ids() {
        let raw = b"Content-Type: multipart/related; boundary=r\r\n\r\n\
            --r\r\n\
            Content-Type: text/html\r\n\r\n\
            <img src=\"cid:logo@x\"> hi\r\n\
            --r\r\n\
            Content-Type: image/png\r\n\
            Content-ID: <logo@x>\r\n\
            Content-Transfer-Encoding: base64\r\n\r\n\
            iVBORw0KGgo=\r\n\
            --r--\r\n";
        let content = parse_message(raw);
        assert!(content.html.unwrap().contains("src=\"cid:logo@x\""));
        assert_eq!(content.text.as_deref().map(str::trim), Some("hi"));
        let png = &content.parts[0];
        assert_eq!(
            (png.section.as_str(), png.content_id.as_deref(), png.inline),
            ("2", Some("logo@x"), true)
        );
        assert_eq!(png.data.as_deref(), Some(&b"\x89PNG\r\n\x1a\n"[..]));
    }

    #[test]
    fn single_part_and_attached_messages() {
        let plain = parse_message(b"Subject: hi\r\n\r\nJust text.\r\n> quoted\r\nmore\r\n");
        assert_eq!(plain.html, None);
        assert!(plain.parts.is_empty());
        assert_eq!(plain.snippet.as_deref(), Some("Just text. more"));

        let raw = b"Content-Type: multipart/mixed; boundary=m\r\n\r\n\
            --m\r\n\
            Content-Type: text/plain\r\n\r\n\
            See below.\r\n\
            --m\r\n\
            Content-Type: message/rfc822\r\n\r\n\
            Subject: inner\r\n\
            Content-Type: multipart/mixed; boundary=n\r\n\r\n\
            --n\r\n\
            Content-Type: text/plain\r\n\r\n\
            inner text\r\n\
            --n--\r\n\
            --m\r\n\
            Content-Type: application/zip; name=a.zip\r\n\r\n\
            ZIP\r\n\
            --m--\r\n";
        let content = parse_message(raw);
        assert_eq!(content.text.as_deref().map(str::trim), Some("See below."));
        let sections: Vec<_> = content
            .parts
            .iter()
            .map(|p| (p.section.as_str(), p.mime_type.as_str()))
            .collect();
        assert_eq!(
            sections,
            [("2", "message/rfc822"), ("3", "application/zip")]
        );
        let inner = content.parts[0].data.as_deref().unwrap();
        assert!(inner.starts_with(b"Subject: inner\r\n"));
        assert!(inner.ends_with(b"--n--"));
    }

    #[test]
    fn decodes_transfer_encodings_leniently() {
        assert_eq!(
            decode_transfer(Some("BASE64"), b"aGVs\r\nbG8=\r\n"),
            b"hello"
        );
        assert_eq!(
            decode_transfer(Some("quoted-printable"), b"caf=C3=A9=\r\n!"),
            "café!".as_bytes()
        );
        assert_eq!(decode_transfer(Some("7bit"), b"as is"), b"as is");
        assert_eq!(
            decode_transfer(Some("base64"), b"not base64!"),
            b"not base64!"
        );
        assert_eq!(decode_transfer(None, b"as is"), b"as is");
    }

    #[test]
    fn inline_means_referenced_by_its_complete_content_id() {
        let raw = b"Content-Type: multipart/related; boundary=r\r\n\r\n\
            --r\r\n\
            Content-Type: text/html\r\n\r\n\
            <img src=\"cid:logo-long\"><img src='CID:a%40b'>\r\n\
            --r\r\n\
            Content-Type: image/png\r\n\
            Content-ID: <logo>\r\n\r\n\
            x\r\n\
            --r\r\n\
            Content-Type: image/png\r\n\
            Content-ID: <logo-long>\r\n\r\n\
            y\r\n\
            --r\r\n\
            Content-Type: image/png\r\n\
            Content-ID: <a@b>\r\n\r\n\
            z\r\n\
            --r--\r\n";
        let inline: Vec<_> = parse_message(raw)
            .parts
            .iter()
            .map(|p| (p.content_id.clone().unwrap(), p.inline))
            .collect();
        assert_eq!(
            inline,
            [
                ("logo".to_owned(), false),
                ("logo-long".to_owned(), true),
                ("a@b".to_owned(), true), // RFC 2392: case-insensitive scheme, %-encoded
            ]
        );
    }

    #[test]
    fn snippet_stops_at_the_limit_between_words() {
        let text = format!("{} b c d", "a".repeat(SNIPPET_CHARS));
        assert_eq!(snippet(&text).unwrap(), "a".repeat(SNIPPET_CHARS));
        let text = format!("{} {}", "a".repeat(SNIPPET_CHARS - 1), "bcd ".repeat(100));
        assert_eq!(snippet(&text).unwrap(), "a".repeat(SNIPPET_CHARS - 1));
        let text = format!("{} {}", "a".repeat(SNIPPET_CHARS - 2), "bcd ".repeat(100));
        assert_eq!(
            snippet(&text).unwrap(),
            format!("{} b", "a".repeat(SNIPPET_CHARS - 2))
        );
    }

    #[test]
    fn snippet_is_cut_at_char_boundary() {
        let long = "字".repeat(300);
        let s = snippet(&long).unwrap();
        assert_eq!(s.chars().count(), SNIPPET_CHARS);
        assert_eq!(snippet("  \n> only quoted\n"), None);
    }
}
