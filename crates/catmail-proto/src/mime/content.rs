use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::{BTreeSet, HashMap, HashSet};

use catmail_html::{cid_image_ids, sanitize_html, split_address, text_and_preheader, visible};
use mail_parser::{Encoding, Message, MessageParser, MessagePart, PartKind, PartRole};

use super::transcode_fields;

/// Longest snippet, in characters.
const SNIPPET_CHARS: usize = 200;

/// The version of what [`parse_message`] and [`MimeNode::content`] make of a message
/// (the parsing, and the sanitizing in `catmail-html`). Stored with each body: one made
/// by an older version is made again, from the raw message where it was kept. Raise it
/// when a change should reach bodies already stored, such as any that sanitizes more
/// strictly; `fingerprint_follows_content_version` fails whenever the output changes,
/// so that the change is a decision.
///
/// [`MimeNode::content`]: super::MimeNode::content
pub const CONTENT_VERSION: u32 = 5;

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
    /// As the sender wrote it, for display only: it may hold path separators, control
    /// characters or a reserved name. Clean it before using it as a file name
    /// (`catmail_core::safe_filename`).
    pub filename: Option<String>,
    /// Lower-case `type/subtype`.
    pub mime_type: String,
    /// Without angle brackets.
    pub content_id: Option<String>,
    /// Shown within the body rather than offered as a file ([`is_inline`]).
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
        Some(message) => extract(&message, &sections(&message), &[], &HashMap::new()),
        None => Content::default(),
    }
}

/// Undoes a Content-Transfer-Encoding (`base64`, `quoted-printable`; anything else is
/// returned as is), as mail-parser does for whole messages: malformed input is decoded
/// as far as it goes, as RFC 2045 asks and other mail clients do (bytes outside the
/// base64 alphabet skipped, a malformed quoted-printable escape kept as written).
pub fn decode_transfer(encoding: Option<&str>, data: &[u8]) -> Vec<u8> {
    let encoding = match encoding.map(str::trim) {
        Some(e) if e.eq_ignore_ascii_case("base64") => Encoding::Base64,
        Some(e) if e.eq_ignore_ascii_case("quoted-printable") => Encoding::QuotedPrintable,
        _ => return data.to_vec(),
    };
    encoding.decode(data).into_owned()
}

/// `sections` are those of `message` ([`sections`]). Every part's data is its body with
/// the transfer encoding undone: text keeps its original bytes and charset. The parts with ids in `also_listed` are listed with the
/// attachments even if they belong to the body. A part's header as the sender wrote it
/// is in `headers` by section (as `BODY[<section>.MIME]` returns it), else in the
/// message.
pub(super) fn extract(
    message: &Message<'_>,
    sections: &HashMap<u32, String>,
    also_listed: &[u32],
    headers: &HashMap<String, Vec<u8>>,
) -> Content {
    let root = message.root();
    let has_html = root
        .html_body()
        .any(|part| matches!(part.kind(), PartKind::Html));
    let html = has_html
        .then(|| joined(root.html_bodies(), "<br>"))
        .flatten()
        .map(|html| sanitize_html(&html));
    let (text, preheader) = text(message, html.as_deref());
    let snippet = snippet(text.as_deref().unwrap_or_default(), preheader.as_deref());
    let text_charsets = text_charsets(message);
    // Part ids follow MIME order. A set, not `contains` on each part: a message may
    // have thousands of parts.
    let listed: BTreeSet<u32> = root
        .parts()
        .filter(is_listed)
        .map(|part| part.id())
        .chain(also_listed.iter().copied())
        .collect();
    let mut parts: Vec<Part> = listed
        .into_iter()
        .filter_map(|id| {
            let part = message.part(id)?;
            let section = sections.get(&id)?;
            let header = headers
                .get(section)
                .map_or_else(|| part.raw_headers(), Vec::as_slice);
            let data = part.decoded().into_owned();
            let content_id = part.content_id().map(str::trim).filter(|id| !id.is_empty());
            let mime_type = part
                .content_type()
                .map(|ct| match ct.subtype() {
                    Some(sub) => format!("{}/{sub}", ct.ctype()),
                    None => ct.ctype().to_owned(),
                })
                .unwrap_or_else(|| "text/plain".into())
                .to_ascii_lowercase();
            Some(Part {
                section: section.clone(),
                transfer_encoding: part
                    .content_transfer_encoding()
                    .map(|e| e.trim().to_ascii_lowercase()),
                filename: filename(&part, header, &text_charsets),
                // Below.
                inline: false,
                mime_type,
                content_id: content_id.map(String::from),
                size: Some(data.len() as u32),
                data: Some(data),
            })
        })
        .collect();
    // Most messages have no part that could be inline: their HTML isn't parsed again for
    // the images.
    if let Some(html) = &html
        && parts
            .iter()
            .any(|p| p.content_id.is_some() && may_be_image(&p.mime_type))
    {
        let images = cid_image_ids(html);
        for part in &mut parts {
            part.inline = is_inline(part.content_id.as_deref(), &part.mime_type, &images);
        }
    }

    Content {
        snippet,
        text,
        html,
        parts,
    }
}

/// Whether a part is listed with the attachments: an attachment, or an image, audio or
/// video part that the body shows. mail-parser no longer counts media shown in both
/// bodies (a `multipart/mixed` of text and images) as an attachment (RFC 8621); listed,
/// such a part is shown inline when the HTML refers to it ([`is_inline`]) and offered as
/// a file otherwise, as it was before.
fn is_listed(part: &MessagePart<'_>) -> bool {
    part.is_attachment() || part.role() == PartRole::MediaBody
}

/// The part's file name. A name in raw 8-bit bytes comes out of the parser with U+FFFD
/// for each byte that isn't UTF-8; then the part's `header` is read again in the charset
/// of the part or of the message's text, as other mail clients do (see
/// [`transcode_fields`]).
fn filename(part: &MessagePart<'_>, header: &[u8], text_charsets: &[&str]) -> Option<String> {
    fn name_of<'a>(part: &'a MessagePart<'_>) -> Option<&'a str> {
        part.attachment_name()
            .map(str::trim)
            .filter(|n| !n.is_empty())
    }
    let name = name_of(part)?;
    if name.contains(char::REPLACEMENT_CHARACTER) {
        let own = part.content_type().and_then(|ct| ct.attribute("charset"));
        let hints: Vec<&str> = own
            .into_iter()
            .chain(text_charsets.iter().copied())
            .collect();
        if let Cow::Owned(fields) = transcode_fields(header, &hints)
            && let Some(message) = MessageParser::new().parse_headers(&fields)
            && let Some(found) = name_of(&message.root_part())
        {
            return Some(found.to_owned());
        }
    }
    Some(name.to_owned())
}

/// The charsets of the text parts, in MIME order, as [`MimeNode::text_charsets`] reads
/// them from the structure.
///
/// [`MimeNode::text_charsets`]: super::MimeNode::text_charsets
fn text_charsets<'a>(message: &'a Message<'_>) -> Vec<&'a str> {
    message
        .root()
        .parts()
        .filter_map(|part| {
            let content_type = part.content_type()?;
            content_type
                .ctype()
                .eq_ignore_ascii_case("text")
                .then(|| content_type.attribute("charset"))?
        })
        .collect()
}

/// The text of the message's body parts, and the preheader of the HTML it was converted
/// from, if it was. An HTML part's text is converted from it sanitized
/// ([`catmail_html::text`]), so that it is the text the reader shows: sanitizing takes
/// out the `<title>`, and what the style sheets hide stays out. A text part is
/// taken as the sender wrote it, unless it is markup that the HTML doesn't show
/// ([`is_markup`]); then the text is the HTML's (`html`, the sanitized HTML of the
/// message). The HTML is the same message in another form, so converting it when the
/// text part was fine loses no more than the text's layout.
fn text(message: &Message<'_>, html: Option<&str>) -> (Option<String>, Option<String>) {
    let of_html = |html: &str| {
        let (text, preheader) = text_and_preheader(html);
        (Some(text).filter(|text| !text.trim().is_empty()), preheader)
    };
    let root = message.root();
    // Most often the HTML parts are the whole body, and sanitized already.
    if root
        .text_body()
        .map(|part| part.id())
        .eq(root.html_body().map(|part| part.id()))
        && root
            .text_body()
            .all(|part| matches!(part.kind(), PartKind::Html))
    {
        return html.map_or((None, None), of_html);
    }
    let text = joined(
        root.text_body().filter_map(|part| match part.kind() {
            PartKind::Text => part.text(),
            PartKind::Html => Some(Cow::Owned(catmail_html::text(&sanitize_html(
                &part.text()?,
            )))),
            _ => None,
        }),
        "\n",
    );
    let (text, html) = match (text, html) {
        (Some(text), Some(html)) => (text, html),
        (text, _) => return (text, None),
    };
    // Converted once, and only if the text part is markup enough to ask.
    let shown = OnceCell::new();
    if is_markup(&text, || {
        let (shown, _) = shown.get_or_init(|| of_html(html));
        shown.as_deref().unwrap_or_default()
    })
        // An HTML of images alone shows no text: the text part, markup and all, still
        // has more to find than nothing.
        && let Some((Some(shown), preheader)) = shown.into_inner()
    {
        return (Some(shown), preheader);
    }
    (Some(text), None)
}

/// Characters, whitespace aside, at the start of a text part that [`is_markup`] reads.
const MARKUP_LEAD: usize = 1000;

/// Whether `text`, from a text part, is markup rather than text: at least half of its
/// first [`MARKUP_LEAD`] characters, whitespace aside, are words of tags, CSS comments
/// or blocks in braces that the text the HTML shows (`shown`) doesn't have. Some
/// senders' text part is their HTML source or style sheet, whole or at the start.
/// Markup is judged at the start, which lists and readers show; a message about code or
/// markup shows it in its HTML too, so its words don't count.
fn is_markup<'s>(text: &str, shown: impl FnOnce() -> &'s str) -> bool {
    let mut lead = Vec::new();
    let mut total = 0;
    for word in text.split_whitespace() {
        if total >= MARKUP_LEAD {
            break;
        }
        total += word.chars().count();
        lead.push((word.as_ptr().addr() - text.as_ptr().addr(), word));
    }
    let limit = lead.last().map_or(0, |&(at, word)| at + word.len());
    let mut spans = markup_spans(text, limit).into_iter().peekable();
    let marked: Vec<&str> = lead
        .into_iter()
        .filter(|&(at, _)| {
            while spans.next_if(|&(_, end)| end <= at).is_some() {}
            spans.peek().is_some_and(|&(start, _)| start <= at)
        })
        .map(|(_, word)| word)
        .collect();
    let in_markup: usize = marked.iter().map(|word| word.chars().count()).sum();
    // Too little markup to matter whatever the HTML shows: most text parts with any
    // stop here, before the HTML is converted.
    if marked.is_empty() || in_markup * 2 < total {
        return false;
    }
    let shown: HashSet<&str> = shown().split_whitespace().collect();
    let hidden: usize = marked
        .iter()
        .filter(|word| !shown.contains(**word))
        .map(|word| word.chars().count())
        .sum();
    hidden * 2 >= total
}

/// Byte ranges of `text` that look like markup, in order, from those that start before
/// `limit`: tags (`<p>`, `</td>`, `<!doctype …>`, not `<https://…>`), `/* … */`
/// comments, and blocks in braces with what they hold. Linear in the text's length,
/// which the sender chooses.
fn markup_spans(text: &str, limit: usize) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let (mut i, mut depth, mut block) = (0, 0, 0);
    // Once a comment has no end, no later one has.
    let mut comments_end = true;
    while i < bytes.len() && (i < limit || depth > 0) {
        if depth == 0 {
            let rest = &bytes[i..];
            // Both start after an ASCII byte, at a character boundary.
            let end = if comments_end && rest.starts_with(b"/*") {
                let end = text[i + 2..].find("*/").map(|end| i + 2 + end + 2);
                comments_end = end.is_some();
                end
            } else if is_tag_start(rest) {
                // Up to the next `<` at most, so that each byte is read once.
                rest[1..]
                    .iter()
                    .position(|&b| matches!(b, b'<' | b'>'))
                    .filter(|&end| rest[1 + end] == b'>')
                    .map(|end| i + 1 + end + 1)
            } else {
                None
            };
            if let Some(end) = end {
                spans.push((i, end));
                i = end;
                continue;
            }
        }
        match bytes[i] {
            b'{' => {
                if depth == 0 {
                    block = i;
                }
                depth += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    spans.push((block, i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    spans
}

/// Whether `rest` starts with what starts a tag: `<!`, or `<` or `</` and a name ending
/// at a space, `/` or `>`.
fn is_tag_start(rest: &[u8]) -> bool {
    let Some(rest) = rest.strip_prefix(b"<") else {
        return false;
    };
    if rest.starts_with(b"!") {
        return true;
    }
    let name = rest.strip_prefix(b"/").unwrap_or(rest);
    let len = name
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'-')
        .count();
    name.first().is_some_and(u8::is_ascii_alphabetic)
        && name
            .get(len)
            .is_some_and(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
}

fn joined<S: AsRef<str>>(pieces: impl Iterator<Item = S>, separator: &str) -> Option<String> {
    let pieces: Vec<S> = pieces.filter(|p| !p.as_ref().trim().is_empty()).collect();
    let pieces: Vec<&str> = pieces.iter().map(AsRef::as_ref).collect();
    (!pieces.is_empty()).then(|| pieces.join(separator))
}

/// HTML stored by an older [`CONTENT_VERSION`] without its raw message, made again as
/// parsing makes it (sanitizing is idempotent), with the Content-IDs that its images
/// refer to by `cid:` ([`is_inline`]).
pub fn sanitized_html(html: &str) -> (String, HashSet<String>) {
    let html = sanitize_html(html);
    let images = cid_image_ids(&html);
    (html, images)
}

/// Whether a part is shown within the body rather than offered as a file: an image in
/// the HTML refers to it by its Content-ID (in `images`, from
/// [`cid_image_ids`](catmail_html::cid_image_ids)), and it can be shown as one. Every other part is an attachment, wherever it sits and whatever
/// else refers to it (a `cid:` link, the text `cid:…`), so that none is out of reach.
pub fn is_inline(content_id: Option<&str>, mime_type: &str, images: &HashSet<String>) -> bool {
    content_id.is_some_and(|id| images.contains(id)) && may_be_image(mime_type)
}

/// Whether a part of `mime_type` can be shown as an image: an image type, or the generic
/// `application/octet-stream` some senders label images with (a reader then takes the
/// type from its bytes, `catmail_html::sniff_image`).
fn may_be_image(mime_type: &str) -> bool {
    mime_type.starts_with("image/") || mime_type == "application/octet-stream"
}

/// IMAP section numbers of the parts of `message` (RFC 3501 §6.4.5): the children of a
/// multipart are numbered from 1 below its own number, and a message that isn't
/// multipart has the single part `1`. Attached messages count as one part.
pub(super) fn sections(message: &Message<'_>) -> HashMap<u32, String> {
    // The children of an attached message's part are none: its parts aren't numbered.
    fn number(part: MessagePart<'_>, section: String, out: &mut HashMap<u32, String>) {
        for (i, child) in part.children().enumerate() {
            number(child, format!("{section}.{}", i + 1), out);
        }
        out.insert(part.id(), section);
    }
    let mut out = HashMap::new();
    let root = message.root_part();
    if root.is_multipart() {
        for (i, child) in root.children().enumerate() {
            number(child, (i + 1).to_string(), &mut out);
        }
    } else {
        out.insert(root.id(), "1".into());
    }
    out
}

/// The start of the message for lists, on one line: the preheader of the HTML the text
/// was converted from, if it has one that the text doesn't start with (as other mail
/// clients' lists show it), then the text. Quoted lines (`> …`), link addresses
/// (`<https://…>`) and characters that take no room ([`visible`]) are left out,
/// whitespace is collapsed, and it is cut at a character boundary, between words where
/// the next doesn't fit.
fn snippet(text: &str, preheader: Option<&str>) -> Option<String> {
    let lead = preheader
        .map(|p| snippet_words(p).collect::<Vec<_>>())
        .filter(|lead| {
            !snippet_words(text)
                .take(lead.len())
                .eq(lead.iter().cloned())
        })
        .unwrap_or_default();
    let mut out = String::new();
    let mut chars = 0;
    for word in lead.into_iter().chain(snippet_words(text)) {
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

/// The words of `text` that [`snippet`] shows, a line at a time.
fn snippet_words(text: &str) -> impl Iterator<Item = String> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('>'))
        .flat_map(|line| {
            let line = visible(&without_addresses(line));
            line.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
}

/// `line` without the addresses in angle brackets that follow links' text
/// (`text <https://…>`), as the HTML's text and many senders' plain text write them.
fn without_addresses(line: &str) -> Cow<'_, str> {
    if !line.contains('<') {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find('<') {
        let inside = &rest[at + 1..];
        match inside.find(|c: char| c == '>' || c.is_whitespace()) {
            Some(end)
                if inside[end..].starts_with('>') && split_address(&inside[..end]).is_some() =>
            {
                out.push_str(rest[..at].trim_end());
                rest = &inside[end + 1..];
            }
            _ => {
                out.push_str(&rest[..=at]);
                rest = inside;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
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

    /// A message whose text is in `charset` and whose attachments are named in raw GBK
    /// bytes, once in each header that can carry the name.
    fn raw_gbk_names(charset: &str) -> Vec<u8> {
        let mut raw = format!(
            "Content-Type: multipart/mixed; boundary=m\r\n\r\n\
            --m\r\n\
            Content-Type: text/plain; charset={charset}\r\n\r\n\
            hi\r\n\
            --m\r\n"
        )
        .into_bytes();
        // 报告 and 淺记 in GBK; the second byte of 淺 is a backslash.
        raw.extend_from_slice(
            b"Content-Type: application/pdf; name=\"\xb1\xa8\xb8\xe6.pdf\"\r\n\
            Content-Transfer-Encoding: base64\r\n\r\n\
            JVBERi0xLjQK\r\n\
            --m\r\n\
            Content-Type: text/plain; charset=us-ascii\r\n\
            Content-Disposition: attachment; filename=\"\x9c\\\xbc\xc7.txt\"\r\n\r\n\
            note\r\n\
            --m--\r\n",
        );
        raw
    }

    fn filenames(content: &Content) -> Vec<Option<&str>> {
        content
            .parts
            .iter()
            .map(|p| p.filename.as_deref())
            .collect()
    }

    #[test]
    fn names_in_raw_bytes_are_read_in_the_charset_of_the_text() {
        let content = parse_message(&raw_gbk_names("gb2312"));
        assert_eq!(filenames(&content), [Some("报告.pdf"), Some("淺记.txt")]);

        // Without a hint of the charset, the lost bytes stay lost.
        let content = parse_message(&raw_gbk_names("utf-8"));
        let names = filenames(&content);
        assert_eq!(names.len(), 2);
        assert!(
            names
                .iter()
                .all(|n| n.unwrap().contains(char::REPLACEMENT_CHARACTER)),
            "{names:?}"
        );
    }

    #[test]
    fn a_part_names_itself_in_its_own_charset_first() {
        let raw = b"Content-Type: multipart/mixed; boundary=m\r\n\r\n\
            --m\r\n\
            Content-Type: text/plain; charset=iso-8859-1\r\n\r\n\
            hi\r\n\
            --m\r\n\
            Content-Type: text/plain; charset=gbk; name=\"\xb1\xa8\xb8\xe6.txt\"\r\n\
            Content-Disposition: attachment\r\n\r\n\
            report\r\n\
            --m--\r\n";
        assert_eq!(filenames(&parse_message(raw)), [Some("报告.txt")]);
    }

    #[test]
    fn a_part_in_ascii_defers_to_the_text() {
        // ANSI_X3.4-1968 is ASCII, but the label names windows-1252, which would take
        // the GBK bytes.
        let raw = b"Content-Type: multipart/mixed; boundary=m\r\n\r\n\
            --m\r\n\
            Content-Type: text/plain; charset=gbk\r\n\r\n\
            hi\r\n\
            --m\r\n\
            Content-Type: text/plain; charset=ansi_x3.4-1968\r\n\
            Content-Disposition: attachment; filename=\"\xb1\xa8\xb8\xe6.txt\"\r\n\r\n\
            report\r\n\
            --m--\r\n";
        assert_eq!(filenames(&parse_message(raw)), [Some("报告.txt")]);
    }

    #[test]
    fn a_name_valid_in_utf8_is_left_alone() {
        // The sender's own U+FFFD, beside bytes that would mean something else in the
        // text's charset.
        let raw = "Content-Type: multipart/mixed; boundary=m\r\n\r\n\
            --m\r\n\
            Content-Type: text/plain; charset=iso-8859-1\r\n\r\n\
            hi\r\n\
            --m\r\n\
            Content-Type: application/pdf; name=\"\u{FFFD}é.pdf\"\r\n\
            Content-Transfer-Encoding: base64\r\n\r\n\
            JVBERi0xLjQK\r\n\
            --m--\r\n";
        assert_eq!(
            filenames(&parse_message(raw.as_bytes())),
            [Some("\u{FFFD}é.pdf")]
        );
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

    /// Images in a `multipart/mixed` between the texts are in both bodies, which RFC 8621
    /// doesn't count as attachments: still listed, inline when the HTML shows them.
    #[test]
    fn media_shown_in_both_bodies_are_listed() {
        let raw = b"Content-Type: multipart/mixed; boundary=m\r\n\r\n\
            --m\r\n\
            Content-Type: text/html\r\n\r\n\
            <p>Look: <img src=\"cid:pic@x\"></p>\r\n\
            --m\r\n\
            Content-Type: image/png\r\n\
            Content-ID: <pic@x>\r\n\
            Content-Disposition: inline\r\n\
            Content-Transfer-Encoding: base64\r\n\r\n\
            iVBORw0KGgo=\r\n\
            --m\r\n\
            Content-Type: text/html\r\n\r\n\
            <p>Bye</p>\r\n\
            --m--\r\n";
        let content = parse_message(raw);
        let parts: Vec<_> = content
            .parts
            .iter()
            .map(|p| (p.section.as_str(), p.mime_type.as_str(), p.inline))
            .collect();
        assert_eq!(parts, [("2", "image/png", true)]);
        assert_eq!(
            content.parts[0].data.as_deref(),
            Some(&b"\x89PNG\r\n\x1a\n"[..])
        );

        // No HTML to show it in: a file to open.
        let raw = b"Content-Type: multipart/mixed; boundary=m\r\n\r\n\
            --m\r\n\
            Content-Type: text/plain\r\n\r\n\
            Before.\r\n\
            --m\r\n\
            Content-Type: image/jpeg; name=photo.jpg\r\n\
            Content-Disposition: inline; filename=photo.jpg\r\n\r\n\
            JPEG\r\n\
            --m\r\n\
            Content-Type: text/plain\r\n\r\n\
            After.\r\n\
            --m--\r\n";
        let content = parse_message(raw);
        assert_eq!(content.text.as_deref(), Some("Before.\nAfter."));
        let parts: Vec<_> = content
            .parts
            .iter()
            .map(|p| (p.section.as_str(), p.filename.as_deref(), p.inline))
            .collect();
        assert_eq!(parts, [("2", Some("photo.jpg"), false)]);
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

    /// A text part that is the HTML's style sheet, then its text, as some senders make it
    /// (a Shopify order confirmation).
    const STYLE_AS_TEXT: &[u8] = b"Subject: Order confirmed\r\n\
        Content-Type: multipart/alternative; boundary=a\r\n\r\n\
        --a\r\n\
        Content-Type: text/plain; charset=utf-8\r\n\r\n\
        96\r\n\r\n\
        /* What it does: Remove spaces around the email design\r\n\
        added by some email clients. */\r\n\
        html, body { Margin: 0 auto !important; padding: 0 !important; height: 100% !important; }\r\n\
        table, td { mso-table-lspace: 0pt !important; mso-table-rspace: 0pt !important; }\r\n\
        @media screen and (max-width: 600px) { .email-container { width: 100% !important; } }\r\n\
        \r\n\
        Thank you for your purchase. Order #1234 is confirmed.\r\n\
        --a\r\n\
        Content-Type: text/html; charset=utf-8\r\n\r\n\
        <html><head><title>Order</title><style>td{padding:0}</style></head><body>\
        <p>Thank you for your purchase.</p><p>Order #1234 is confirmed.</p></body></html>\r\n\
        --a--\r\n";

    #[test]
    fn a_text_part_of_markup_gives_way_to_the_html() {
        let content = parse_message(STYLE_AS_TEXT);
        let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        let expected = "Thank you for your purchase. Order #1234 is confirmed.";
        assert_eq!(
            content.text.as_deref().map(words).as_deref(),
            Some(expected)
        );
        assert_eq!(content.snippet.as_deref(), Some(expected));

        // The HTML source itself.
        let raw = alternative(
            "<!doctype html>\r\n<html>\r\n<head>\r\n<meta charset=\"utf-8\">\r\n\
            <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\r\n</head>\r\n\
            <body>\r\n<table cellpadding=\"0\" cellspacing=\"0\" width=\"100%\" role=\"presentation\">\r\n\
            <tr><td>Happy birthday!</td></tr>\r\n</table>\r\n</body>\r\n</html>",
            "<table cellpadding=\"0\"><tr><td>Happy birthday!</td></tr></table>",
        );
        let content = parse_message(&raw);
        assert_eq!(
            content.text.as_deref().map(str::trim),
            Some("Happy birthday!")
        );

        // But not to an HTML with no text.
        let text = "/* What it does: Remove spaces around the email design. */\r\n\
            html, body { Margin: 0 auto !important; padding: 0 !important; }\r\n\
            table, td { mso-table-lspace: 0pt !important; mso-table-rspace: 0pt !important; }\r\n\
            Order #1234 is confirmed.";
        let raw = alternative(text, "<img src=\"https://example.com/order.png\">");
        assert_eq!(parse_message(&raw).text.as_deref(), Some(text));
    }

    /// A `multipart/alternative` message of `text` and `html`.
    fn alternative(text: &str, html: &str) -> Vec<u8> {
        format!(
            "Content-Type: multipart/alternative; boundary=a\r\n\r\n\
            --a\r\nContent-Type: text/plain\r\n\r\n{text}\r\n\
            --a\r\nContent-Type: text/html\r\n\r\n{html}\r\n\
            --a--\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn a_text_part_about_markup_is_kept() {
        // Mostly style rules, which the HTML shows too; and links in angle brackets.
        let text = "Try these rules:\r\n\r\n\
            .button { color: red !important; background: white; }\r\n\
            .link { text-decoration: none !important; }\r\n\r\n\
            From <https://example.com/css>, <a@example.com>.";
        let html = "<p>Try these rules:</p><pre>.button { color: red !important; background: white; }\n\
            .link { text-decoration: none !important; }</pre>\
            <p>From https://example.com/css, a@example.com.</p>";
        let content = parse_message(&alternative(text, html));
        assert_eq!(content.text.as_deref(), Some(text));

        // Markup that the HTML doesn't show, but little of it.
        let raw = alternative(
            "Your code is 4721. It expires in ten minutes; if you didn't ask for it,\r\n\
            you can ignore this message. <b>Don't share it.</b>",
            "<p>Your code is <b>4721</b>.</p>",
        );
        let text = parse_message(&raw).text.unwrap();
        assert!(text.starts_with("Your code is 4721. It expires"), "{text}");
    }

    #[test]
    fn html_text_leaves_out_style_sheets_and_the_title() {
        // mail-parser's own conversion skips only a `<style>` without attributes.
        let raw = b"Content-Type: text/html; charset=utf-8\r\n\r\n\
            <html><head><title>Newsletter</title></head><body>\
            <style type=\"text/css\">div.preheader { display: none !important; }</style>\
            <p>Your parcel has arrived.</p></body></html>\r\n";
        let content = parse_message(raw);
        assert_eq!(
            content.text.as_deref().map(str::trim),
            Some("Your parcel has arrived.")
        );
        assert_eq!(content.snippet.as_deref(), Some("Your parcel has arrived."));

        // With a text part beside it, each converted on its own.
        let raw = b"Content-Type: multipart/mixed; boundary=m\r\n\r\n\
            --m\r\n\
            Content-Type: text/plain\r\n\r\n\
            Hello\r\n\
            --m\r\n\
            Content-Type: text/html\r\n\r\n\
            <style media=\"screen\">p { margin: 0 }</style><p>World</p>\r\n\
            --m--\r\n";
        let text = parse_message(raw).text.unwrap();
        assert_eq!(
            text.split_whitespace().collect::<Vec<_>>(),
            ["Hello", "World"]
        );
    }

    /// Mail for lists: a preheader hidden in front, then characters that take no room.
    const PREHEADER: &[u8] = b"Subject: Sale\r\n\
        Content-Type: text/html; charset=utf-8\r\n\r\n\
        <html><head><style>.pre { display: none; max-height: 0 }</style></head><body>\
        <div class=\"pre\">Up to 50% off this week</div>\
        <div style=\"display:none\">&zwnj;&#847; &zwnj;&#847; &zwnj;&#847; &#8203;</div>\
        <p>Dear reader,</p><p>Our <a href=\"https://shop.example/sale\">sale</a> starts.</p>\
        </body></html>\r\n";

    #[test]
    fn snippet_starts_with_the_html_preheader() {
        let content = parse_message(PREHEADER);
        assert_eq!(
            content.text.as_deref(),
            Some("Dear reader,\n\nOur sale <https://shop.example/sale> starts.\n")
        );
        assert_eq!(
            content.snippet.as_deref(),
            Some("Up to 50% off this week Dear reader, Our sale starts.")
        );

        // An unfilled preheader is left out.
        let raw = b"Content-Type: text/html\r\n\r\n\
            <span style=\"display:none\">*|MC_PREVIEW_TEXT|*</span><p>Hello</p>\r\n";
        assert_eq!(parse_message(raw).snippet.as_deref(), Some("Hello"));

        // A text part is the sender's own: its snippet has no preheader.
        let raw = b"Content-Type: multipart/alternative; boundary=a\r\n\r\n\
            --a\r\n\
            Content-Type: text/plain\r\n\r\n\
            Hello\r\n\
            --a\r\n\
            Content-Type: text/html\r\n\r\n\
            <span style=\"display:none\">Preview</span><p>Hello</p>\r\n\
            --a--\r\n";
        assert_eq!(parse_message(raw).snippet.as_deref(), Some("Hello"));
    }

    #[test]
    fn snippet_leaves_out_what_takes_no_room_or_is_not_read() {
        assert_eq!(
            snippet(
                "Read <https://x.example/a?b>. \u{200c}\u{34f} \u{feff}\n\
                 1 <2> <mailto:a@b.example> <a@b.example> <https:// x>",
                None
            )
            .as_deref(),
            Some("Read. 1 <2> <a@b.example> <https:// x>")
        );
        // Words of filler alone are left out.
        assert_eq!(
            snippet("Hi \u{200d} \u{200d} there", None).as_deref(),
            Some("Hi there")
        );
        // A preheader that the text starts with is said once.
        assert_eq!(
            snippet("Hello there\nmore", Some("Hello  there")).as_deref(),
            Some("Hello there more")
        );
        assert_eq!(snippet("", Some("\u{200c} Only")).as_deref(), Some("Only"));
    }

    #[test]
    fn markup_spans_are_tags_comments_and_blocks() {
        fn spans(text: &str) -> Vec<&str> {
            let spans = markup_spans(text, text.len()).into_iter();
            spans.map(|(start, end)| &text[start..end]).collect()
        }
        assert_eq!(
            spans("a <p class=\"x\">b</p> <https://x.y> 1 < 2 > 0 /* c */ s { t { u } } v"),
            ["<p class=\"x\">", "</p>", "/* c */", "{ t { u } }"]
        );
        // What doesn't end is no span, and leaves the rest to be read.
        assert_eq!(spans("/* open <a <b> é { x"), ["<b>"]);
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
        assert_eq!(decode_transfer(Some("x-unknown"), b"as is"), b"as is");
        assert_eq!(decode_transfer(None, b"as is"), b"as is");
    }

    const MALFORMED_QP: &[u8] = b"Content-Type: text/plain; charset=utf-8\r\n\
        Content-Transfer-Encoding: quoted-printable\r\n\r\n\
        1+1==2 caf=C3=A9\r\n";

    /// Malformed input is decoded as far as it goes, as RFC 2045 asks: one bad escape or
    /// stray byte doesn't leave a whole part encoded.
    #[test]
    fn decodes_malformed_transfer_encodings_as_far_as_they_go() {
        assert_eq!(decode_transfer(Some("base64"), b"aGVs!bG8=\r\n"), b"hello");
        assert_eq!(
            decode_transfer(Some("quoted-printable"), b"1+1==2 caf=C3=A9 =ZZ"),
            "1+1==2 caf\u{e9} =ZZ".as_bytes()
        );
        let content = parse_message(MALFORMED_QP);
        assert_eq!(
            content.text.as_deref().map(str::trim),
            Some("1+1==2 caf\u{e9}")
        );
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
    fn inline_means_shown_as_an_image() {
        let raw = b"Content-Type: multipart/related; boundary=r\r\n\r\n\
            --r\r\n\
            Content-Type: text/html\r\n\r\n\
            <a href=\"cid:report@x\">report</a> see cid:note@x\r\n\
            <img src=\"cid:photo@x\"><img src=\"cid:clip@x\">\r\n\
            --r\r\n\
            Content-Type: application/pdf; name=report.pdf\r\n\
            Content-ID: <report@x>\r\n\r\n\
            %PDF\r\n\
            --r\r\n\
            Content-Type: text/plain; name=note.txt\r\n\
            Content-ID: <note@x>\r\n\r\n\
            note\r\n\
            --r\r\n\
            Content-Type: application/octet-stream; name=photo.png\r\n\
            Content-ID: <photo@x>\r\n\r\n\
            png\r\n\
            --r\r\n\
            Content-Type: video/mp4; name=clip.mp4\r\n\
            Content-ID: <clip@x>\r\n\r\n\
            mp4\r\n\
            --r--\r\n";
        let inline: Vec<_> = parse_message(raw)
            .parts
            .iter()
            .map(|p| (p.content_id.clone().unwrap(), p.inline))
            .collect();
        assert_eq!(
            inline,
            [
                ("report@x".to_owned(), false), // a link: listed, so it can be opened
                ("note@x".to_owned(), false),   // text, not a URL
                ("photo@x".to_owned(), true),   // an image labelled as bytes
                ("clip@x".to_owned(), false),   // can't be shown as an image
            ]
        );
        assert!(may_be_image("image/svg+xml"));
        assert!(!may_be_image("application/pdf"));
    }

    #[test]
    fn snippet_stops_at_the_limit_between_words() {
        let text = format!("{} b c d", "a".repeat(SNIPPET_CHARS));
        assert_eq!(snippet(&text, None).unwrap(), "a".repeat(SNIPPET_CHARS));
        let text = format!("{} {}", "a".repeat(SNIPPET_CHARS - 1), "bcd ".repeat(100));
        assert_eq!(snippet(&text, None).unwrap(), "a".repeat(SNIPPET_CHARS - 1));
        let text = format!("{} {}", "a".repeat(SNIPPET_CHARS - 2), "bcd ".repeat(100));
        assert_eq!(
            snippet(&text, None).unwrap(),
            format!("{} b", "a".repeat(SNIPPET_CHARS - 2))
        );
    }

    #[test]
    fn snippet_is_cut_at_char_boundary() {
        let long = "字".repeat(300);
        let s = snippet(&long, None).unwrap();
        assert_eq!(s.chars().count(), SNIPPET_CHARS);
        assert_eq!(snippet("  \n> only quoted\n", None), None);
    }

    /// What a sender may write in HTML, for the sanitizer.
    const HTML_ONLY: &[u8] = b"From: news@example.com\r\n\
        Subject: Offers\r\n\
        MIME-Version: 1.0\r\n\
        Content-Type: multipart/related; boundary=\"rel\"\r\n\
        \r\n\
        --rel\r\n\
        Content-Type: text/html; charset=utf-8\r\n\
        \r\n\
        <html><head><style>body{color:#333;background:url(https://t.example/a.png)}\
        @media (max-width:600px){td{display:block}}@import url(x.css);\
        .a:has(b){color:red}p{color:var(--c)}</style>\
        <style media=\"print\">p{color:black}</style></head>\
        <body bgcolor=\"#ffffff\" onload=\"x()\"><table width=\"600\"><tr>\
        <td style=\"color:red;background-image:url(x);mso-line-height-rule:exactly\">\
        <font color=\"blue\">Hi</font> <a href=\"javascript:alert(1)\">x</a> \
        <a href=\"java\tscript:alert(1)\">y</a> <a href=\"https://example.com/?a=1\">z</a>\
        <img src=\"cid:logo@x\"><img src=\"data:image/png;base64,iVBORw0KGgo=\">\
        <img src=\"https://example.com/p.gif\"><script>alert(1)</script>\
        <form><input name=\"q\"></form><iframe src=\"https://example.com\"></iframe>\
        <svg><style>p{}</style></svg></td></tr></table></body></html>\r\n\
        --rel\r\n\
        Content-Type: image/png\r\n\
        Content-ID: <logo@x>\r\n\
        Content-Transfer-Encoding: base64\r\n\
        \r\n\
        iVBORw0KGgo=\r\n\
        --rel--\r\n";

    const PLAIN: &[u8] = b"From: a@example.com\r\n\
        Subject: Plain\r\n\
        Content-Type: text/plain; charset=iso-8859-1\r\n\
        Content-Transfer-Encoding: quoted-printable\r\n\
        \r\n\
        Gr=FC=DFe aus Z=FCrich,\r\n\
        bis bald.\r\n";

    /// FNV-1a: stable across Rust versions, unlike `DefaultHasher`. `None` hashes apart
    /// from any value.
    fn fnv(hash: &mut u64, bytes: Option<&[u8]>) {
        for &b in bytes.unwrap_or(b"\0none").iter().chain(&[0xff]) {
            *hash ^= u64::from(b);
            *hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }

    /// Pins what parsing and sanitizing make of a corpus, so that a change to either is
    /// a decision on [`CONTENT_VERSION`].
    #[test]
    fn fingerprint_follows_content_version() {
        let mut hash = 0xcbf2_9ce4_8422_2325;
        for raw in [
            MIXED,
            &raw_gbk_names("gbk"),
            HTML_ONLY,
            PLAIN,
            STYLE_AS_TEXT,
            MALFORMED_QP,
            PREHEADER,
        ] {
            let content = parse_message(raw);
            let h = &mut hash;
            fnv(h, content.text.as_deref().map(str::as_bytes));
            fnv(h, content.html.as_deref().map(str::as_bytes));
            fnv(h, content.snippet.as_deref().map(str::as_bytes));
            for part in &content.parts {
                fnv(h, Some(part.section.as_bytes()));
                fnv(h, part.transfer_encoding.as_deref().map(str::as_bytes));
                fnv(h, part.filename.as_deref().map(str::as_bytes));
                fnv(h, Some(part.mime_type.as_bytes()));
                fnv(h, part.content_id.as_deref().map(str::as_bytes));
                fnv(h, Some(&[u8::from(part.inline)]));
                fnv(h, part.size.map(u32::to_le_bytes).as_ref().map(|b| &b[..]));
                fnv(h, part.data.as_deref());
            }
        }
        // What parsing or sanitizing makes of messages changed. If bodies already stored
        // should change too (always for stricter sanitizing), raise CONTENT_VERSION;
        // either way, record the new fingerprint here.
        assert_eq!(
            (CONTENT_VERSION, format!("{hash:016x}")),
            (5, "82f9c788d385ac37".to_owned()),
        );
    }
}
