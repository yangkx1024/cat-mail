//! Building outgoing messages with `mail-builder`.

use std::borrow::Cow;
use std::fmt::Write as _;

use catmail_html::escape;
use catmail_types::{Address, FORWARD_SEPARATOR, Quote, QuoteHead};
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address as BuilderAddress;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::message_id::MessageId;
use mail_builder::mime::MimePart;

use super::fits_in_angle_brackets;

/// A message to send, or a draft to store.
#[derive(Debug, Clone)]
pub struct Outgoing<'a> {
    pub from: &'a Address,
    pub to: &'a [Address],
    pub cc: &'a [Address],
    /// Only for a draft. A message to send leaves them out: they only go into the SMTP
    /// envelope.
    pub bcc: &'a [Address],
    pub subject: &'a str,
    /// Plain text; line breaks may be LF or CRLF. With `html`, its alternative for
    /// readers that show only text.
    pub text: &'a str,
    /// The body formatted: the content of an HTML `<body>`, its images by `cid:` URL.
    pub html: Option<&'a str>,
    /// Sent below the body, in the text and in the HTML. A quote of HTML makes the
    /// message HTML, the body's text written as paragraphs if it has no HTML.
    pub quote: Option<&'a Quote>,
    /// Without angle brackets.
    pub message_id: &'a str,
    pub in_reply_to: Option<&'a str>,
    pub references: &'a [String],
    /// Unix seconds.
    pub date: i64,
    pub attachments: Vec<OutgoingAttachment<'a>>,
    /// Whether what it goes through takes 8-bit content: an IMAP server does, an SMTP
    /// server if it announces 8BITMIME. Only a message attached whole needs it.
    pub eight_bit: bool,
}

#[derive(Debug, Clone)]
pub struct OutgoingAttachment<'a> {
    pub filename: &'a str,
    /// `type/subtype`; anything else is sent as `application/octet-stream`.
    pub mime_type: &'a str,
    /// An image of the HTML, which shows it by this Content-ID (without angle
    /// brackets), rather than a file attached. Left out unless the HTML shows it.
    pub content_id: Option<&'a str>,
    pub data: &'a [u8],
}

/// An address or Message-ID that can't go between `<` and `>` in a header.
#[derive(Debug, thiserror::Error)]
#[error("invalid {field} {value:?}")]
pub struct InvalidHeader {
    field: &'static str,
    value: String,
}

/// The message in wire form: CRLF line endings, 7-bit clean (headers as RFC 2047
/// encoded words, bodies as quoted-printable or base64 where needed), except a message
/// attached whole, which goes as it is (see `attached_message`).
pub fn build_message(message: &Outgoing<'_>) -> Result<Vec<u8>, InvalidHeader> {
    let mut builder = MessageBuilder::new()
        .message_id(bracketed("Message-ID", message.message_id)?)
        .date(message.date)
        .subject(one_line(message.subject))
        .from(address(message.from)?);
    if !message.to.is_empty() {
        builder = builder.to(address_list(message.to)?);
    }
    if !message.cc.is_empty() {
        builder = builder.cc(address_list(message.cc)?);
    }
    if !message.bcc.is_empty() {
        builder = builder.bcc(address_list(message.bcc)?);
    }
    // Threading IDs come from received mail: copy only what a msg-id can hold, whatever
    // the caller passes.
    if let Some(parent) = message.in_reply_to.filter(|id| fits_in_angle_brackets(id)) {
        builder = builder.in_reply_to(parent);
    }
    let references: Vec<&str> = message
        .references
        .iter()
        .map(String::as_str)
        .filter(|id| fits_in_angle_brackets(id))
        .collect();
    if !references.is_empty() {
        builder = builder.references(MessageId::new_list(references.into_iter()));
    }
    Ok(builder
        .body(body(message))
        .write_to_vec()
        .expect("writing into a Vec can't fail"))
}

/// The text alone, or as the alternative of the HTML, which is `multipart/related` to
/// the images it shows; then the files attached (`multipart/mixed`). An inline image the
/// HTML doesn't show (any, without HTML) is left out: the user took it out of the text.
fn body<'a>(message: &Outgoing<'a>) -> MimePart<'a> {
    let text = match message.quote {
        Some(quote) => MimePart::new("text/plain", quote.below(message.text)),
        None => MimePart::new("text/plain", message.text),
    };
    let content = match html(message) {
        None => text,
        Some(document) => {
            let shown = catmail_html::cid_image_ids(&document);
            let alternative = MimePart::new(
                "multipart/alternative",
                vec![text, MimePart::new("text/html", document)],
            );
            let images: Vec<MimePart<'a>> = message
                .attachments
                .iter()
                .filter_map(|a| {
                    let cid = a.content_id.filter(|cid| shown.contains(*cid))?;
                    Some(
                        MimePart::new(content_type(a.mime_type), a.data)
                            .header(
                                "Content-Disposition",
                                ContentType::new("inline")
                                    .attribute("filename", one_line(a.filename).into_owned()),
                            )
                            .cid(cid),
                    )
                })
                .collect();
            wrap("multipart/related", alternative, images)
        }
    };
    let files: Vec<MimePart<'a>> = message
        .attachments
        .iter()
        .filter(|a| a.content_id.is_none())
        .map(|a| {
            let part = MimePart::new(content_type(a.mime_type), a.data)
                .attachment(one_line(a.filename).into_owned());
            match attached_message(a, message.eight_bit) {
                Some(encoding) => part.transfer_encoding(encoding),
                // mail-builder's choice: base64, which isn't text.
                None => part,
            }
        })
        .collect();
    wrap("multipart/mixed", content, files)
}

/// The Content-Transfer-Encoding of an attached message that goes as it is: `7bit`, or
/// with `eight_bit` `8bit`. RFC 2046 §5.2.1 allows no other for `message/rfc822` (bar
/// `binary`, which SMTP can't carry), so its every header, signature included, reaches
/// the recipient untouched. `None` to leave it to mail-builder, which base64-encodes it:
/// another type, or one that can't go as it is (a NUL, a lone CR or LF, a line over the
/// 998 octets SMTP allows, 8-bit content where it may not go). That breaks the RFC, but
/// Outlook sends attached messages so all the time, and mail clients read them; it beats
/// a message that can't be sent, or one whose lines a server rewraps.
fn attached_message(a: &OutgoingAttachment<'_>, eight_bit: bool) -> Option<&'static str> {
    if !a.mime_type.eq_ignore_ascii_case("message/rfc822") {
        return None;
    }
    let data = a.data;
    let (mut ascii, mut line) = (true, 0usize);
    for (i, &byte) in data.iter().enumerate() {
        match byte {
            0 => return None,
            b'\r' if data.get(i + 1) != Some(&b'\n') => return None,
            b'\n' if i == 0 || data[i - 1] != b'\r' => return None,
            b'\r' => {}
            b'\n' => line = 0,
            _ => {
                line += 1;
                if line > 998 {
                    return None;
                }
                ascii &= byte.is_ascii();
            }
        }
    }
    match (ascii, eight_bit) {
        (true, _) => Some("7bit"),
        (false, true) => Some("8bit"),
        (false, false) => None,
    }
}

/// `first` alone, or with `rest` in a multipart of `kind`.
fn wrap<'a>(kind: &'static str, first: MimePart<'a>, rest: Vec<MimePart<'a>>) -> MimePart<'a> {
    if rest.is_empty() {
        return first;
    }
    let mut parts = vec![first];
    parts.extend(rest);
    MimePart::new(kind, parts)
}

/// What the user wrote is in an element of this class, laid out as the editor shows it
/// ([`html_document`]); so is the text of a message quoted that has no HTML.
const TEXT_CLASS: &str = "catmail-text";

/// The HTML of a message quoted is in an element of this class, which its style sheet is
/// scoped to: left as it was, it would restyle the whole message.
const ORIGINAL_CLASS: &str = "catmail-original";

/// Around the element of [`ORIGINAL_CLASS`]: what the message quoted draws is cut off at
/// this box, so it can't cover what is outside it (what the user wrote, a forward's
/// headers) by moving its content (`position`, negative margins, `transform`).
/// `contain: paint` makes it what `position: fixed` is placed by, and clips there;
/// `position: relative` and `overflow` do the same for everything else in clients that
/// don't know `contain`. `auto`, not `hidden`, so a message wider than the recipient's
/// window can still be scrolled. This element, not the one of [`ORIGINAL_CLASS`]: the
/// message's sheet, scoped, reaches that one (as its `body`) and could move it whole or
/// undo its styles, never one around it.
const CONTAINED: &str = r#"<div style="position:relative;overflow:auto;contain:paint">"#;

/// The attributes of a reply's quote: set off by a line on its left, as Gmail draws it.
const CITE: &str =
    r#" type="cite" style="margin:0 0 0 0.8ex;border-left:1px solid #ccc;padding-left:1ex""#;

/// A forward's headers: a table, their names bold in a column of their own, as Gmail and
/// Apple Mail lay them out.
const HEADERS_TABLE: &str =
    r#"<table cellpadding="0" cellspacing="0" style="border-collapse:collapse">"#;
const HEADER_NAME: &str =
    r#" style="padding:0 1ex 0 0;font-weight:bold;vertical-align:top;white-space:nowrap""#;

/// The message as an HTML document: the body's HTML (or its text as paragraphs), then
/// the quote. `None` for a body of text with no quote of HTML, which the text says all
/// of.
fn html(message: &Outgoing<'_>) -> Option<String> {
    let quote = message.quote;
    if message.html.is_none() && quote.is_none_or(|q| q.html.is_none()) {
        return None;
    }
    let mut body = format!(r#"<div class="{TEXT_CLASS}">"#);
    match message.html {
        Some(html) => body.push_str(&fill_empty_paragraphs(html)),
        None => paragraphs(message.text, &mut body),
    }
    body.push_str("</div>");
    let sheet = quote
        .map(|quote| {
            body.push_str("<br>");
            quote_html(quote, &mut body)
        })
        .unwrap_or_default();
    Some(html_document(&sheet, &body))
}

/// Writes the quote as HTML to `out`: its head, then the message quoted, in a
/// `<blockquote type="cite">` for a reply (what mail clients recognize as a quote). The
/// message's HTML if it has any, its style sheet returned to go in the document's head;
/// else its text, as paragraphs.
fn quote_html(quote: &Quote, out: &mut String) -> String {
    out.push_str(r#"<div class="catmail-quote">"#);
    let close = match &quote.head {
        QuoteHead::Reply(attribution) => {
            let attribution = escape(&attribution.to_string());
            let _ = write!(out, "<div>{attribution}</div><blockquote{CITE}>");
            "</blockquote>"
        }
        QuoteHead::Forward(headers) => {
            let _ = write!(out, "<div>{}</div>", escape(FORWARD_SEPARATOR));
            if !headers.is_empty() {
                out.push_str(HEADERS_TABLE);
                for (field, value) in headers {
                    let _ = write!(
                        out,
                        "<tr><td{HEADER_NAME}>{}:</td><td>{}</td></tr>",
                        field.label(),
                        escape(value)
                    );
                }
                out.push_str("</table>");
            }
            out.push_str("<br>");
            ""
        }
    };
    let sheet = match &quote.html {
        Some(html) => {
            let (sheet, rest) = catmail_html::nested(html, ORIGINAL_CLASS);
            let _ = write!(
                out,
                r#"{CONTAINED}<div class="{ORIGINAL_CLASS}">{rest}</div></div>"#
            );
            sheet
        }
        None => {
            let _ = write!(out, r#"<div class="{TEXT_CLASS}">"#);
            out.push_str(&catmail_html::quoted_text_html(
                &quote.text,
                CITE,
                EMPTY_LINE,
            ));
            out.push_str("</div>");
            String::new()
        }
    };
    out.push_str(close);
    out.push_str("</div>");
    sheet
}

/// `text` as paragraphs, one a line, as the editor writes them.
fn paragraphs(text: &str, out: &mut String) {
    for line in text.trim_end().lines() {
        paragraph(line, out);
    }
}

/// An empty paragraph has no height: a line break in it keeps the empty line.
const EMPTY_LINE: &str = "<p><br></p>";

fn paragraph(line: &str, out: &mut String) {
    if line.is_empty() {
        out.push_str(EMPTY_LINE);
    } else {
        let _ = write!(out, "<p>{}</p>", escape(line));
    }
}

/// The body as a document of its own, with `sheet` (a quoted message's) in its head.
/// What the user wrote is shown as the editor showed it: a paragraph is a line, with no
/// room around it, and an empty one an empty line; spaces as written (runs of them,
/// indentation; the editor writes no whitespace between its tags). Images are no wider
/// than the reader's window.
fn html_document(sheet: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\
         <style>.{TEXT_CLASS} {{ white-space: pre-wrap; }} .{TEXT_CLASS} p {{ margin: 0; }} \
         img {{ max-width: 100%; height: auto; }}{sheet}</style></head>\n\
         <body>{body}</body></html>\n"
    )
}

/// Puts a line break in each empty paragraph (`<p></p>`, `<p style="…"></p>`): with
/// nothing in it a paragraph has no height, and the empty line it is in the editor
/// would be gone.
fn fill_empty_paragraphs(html: &str) -> Cow<'_, str> {
    if !html.contains("></p>") {
        return Cow::Borrowed(html);
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(end) = rest.find("></p>") {
        let (before, after) = rest.split_at(end + 1);
        out.push_str(before);
        let tag = &before[before.rfind('<').unwrap_or(0)..];
        let empty_p = tag
            .strip_prefix("<p")
            .is_some_and(|attrs| attrs.starts_with(['>', ' ']));
        if empty_p {
            out.push_str("<br>");
        }
        rest = after;
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn address(a: &Address) -> Result<BuilderAddress<'_>, InvalidHeader> {
    Ok(BuilderAddress::new_address(
        a.name.as_deref().map(|n| one_line(n).into_owned()),
        bracketed("address", &a.email)?,
    ))
}

fn address_list(list: &[Address]) -> Result<BuilderAddress<'_>, InvalidHeader> {
    let list = list.iter().map(address).collect::<Result<_, _>>()?;
    Ok(BuilderAddress::new_list(list))
}

/// mail-builder writes the Message-ID between angle brackets as it is, and a bare CR in
/// it becomes a line break on the way out, starting a header of the sender's choosing.
/// From an address it drops control characters, so the message would go to another
/// address than the draft's. Unlike threading IDs, neither can just be left out.
fn bracketed<'a>(field: &'static str, value: &'a str) -> Result<&'a str, InvalidHeader> {
    if fits_in_angle_brackets(value) {
        Ok(value)
    } else {
        Err(InvalidHeader {
            field,
            value: value.to_owned(),
        })
    }
}

/// Header text can't contain line breaks: they would end the header, and what follows
/// would be read as another one.
fn one_line(s: &str) -> Cow<'_, str> {
    if s.contains(['\r', '\n']) {
        Cow::Owned(
            s.split(['\r', '\n'])
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        )
    } else {
        Cow::Borrowed(s)
    }
}

fn content_type(mime_type: &str) -> &str {
    let token = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
    };
    match mime_type.split_once('/') {
        Some((t, sub)) if token(t) && token(sub) => mime_type,
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use catmail_types::{Attribution, ForwardField};
    use mail_parser::MessageParser;

    use super::*;

    fn addr(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(String::from),
            email: email.into(),
        }
    }

    #[test]
    fn round_trips_through_the_parser() {
        let from = addr(Some("张三"), "zhang@example.com");
        let to = [
            addr(Some("O'Brien, \"Bob\""), "bob@example.com"),
            addr(None, "c@example.com"),
        ];
        let references = ["root@x".to_owned(), "parent@x".to_owned()];
        let raw = build_message(&Outgoing {
            from: &from,
            to: &to,
            cc: &[addr(None, "d@example.com")],
            bcc: &[],
            subject: "季度报告\r\nBcc: injected@evil.example",
            text: "第一行\n.\nlast line with trailing space ",
            html: None,
            quote: None,
            message_id: "abc.123@example.com",
            in_reply_to: Some("parent@x"),
            references: &references,
            date: 1_792_200_600,
            eight_bit: false,
            attachments: vec![OutgoingAttachment {
                filename: "报告 \"final\".pdf",
                mime_type: "application/pdf",
                content_id: None,
                data: b"%PDF-1.4\x00\xff",
            }],
        })
        .unwrap();
        assert!(raw.is_ascii(), "7-bit clean");
        assert!(
            !String::from_utf8_lossy(&raw).contains("\nBcc:"),
            "no header injection"
        );
        assert!(
            raw.windows(2)
                .enumerate()
                .all(|(i, w)| w[1] != b'\n' || w[0] == b'\r' || i == 0),
            "CRLF line endings"
        );

        let m = MessageParser::new().parse(&raw).unwrap();
        assert_eq!(m.subject(), Some("季度报告 Bcc: injected@evil.example"));
        let from = m.from().unwrap().first().unwrap();
        assert_eq!(
            (from.name(), from.address()),
            (Some("张三"), Some("zhang@example.com"))
        );
        let to: Vec<_> = m
            .to()
            .unwrap()
            .mailboxes()
            .map(|a| (a.name(), a.address()))
            .collect();
        assert_eq!(
            to,
            [
                (Some("O'Brien, \"Bob\""), Some("bob@example.com")),
                (None, Some("c@example.com"))
            ]
        );
        assert_eq!(
            m.cc().unwrap().first().unwrap().address(),
            Some("d@example.com")
        );
        assert!(m.bcc().is_none());
        assert_eq!(m.message_id(), Some("abc.123@example.com"));
        assert_eq!(m.in_reply_to().unwrap().first(), Some("parent@x"));
        assert_eq!(
            m.references().unwrap().iter().collect::<Vec<_>>(),
            ["root@x", "parent@x"]
        );
        assert_eq!(m.date().unwrap().to_timestamp(), 1_792_200_600);
        assert_eq!(
            m.body_text(0).unwrap().replace("\r\n", "\n"),
            "第一行\n.\nlast line with trailing space "
        );
        let attachment = m.attachments().next().unwrap();
        assert_eq!(attachment.attachment_name(), Some("报告 \"final\".pdf"));
        assert_eq!(attachment.content_type().unwrap().ctype(), "application");
        assert_eq!(&*attachment.decoded(), b"%PDF-1.4\x00\xff");
    }

    #[test]
    fn threading_ids_cant_inject_headers() {
        let from = addr(None, "a@example.com");
        let references = ["ok@x".to_owned(), "r1\rX-R: 1@x".to_owned()];
        let raw = build_message(&Outgoing {
            from: &from,
            to: &[addr(None, "b@example.com")],
            cc: &[],
            bcc: &[],
            subject: "Re: hi",
            text: "hi",
            html: None,
            quote: None,
            message_id: "m@example.com",
            in_reply_to: Some("evil\rX-Injected: yes@x"),
            references: &references,
            date: 0,
            eight_bit: false,
            attachments: Vec::new(),
        })
        .unwrap();
        assert!(
            raw.windows(2).all(|w| w[0] != b'\r' || w[1] == b'\n'),
            "no bare CR"
        );
        let m = MessageParser::new().parse(&raw).unwrap();
        assert!(m.headers().get("X-Injected").is_none() && m.headers().get("X-R").is_none());
        assert!(m.in_reply_to().is_none_or(|ids| ids.is_empty()));
        assert_eq!(m.references().unwrap().iter().collect::<Vec<_>>(), ["ok@x"]);
    }

    #[test]
    fn addresses_and_message_id_cant_inject_headers() {
        let from = addr(None, "a@example.com");
        let evil = addr(None, "a\rBcc: x@evil.example");
        let to = [from.clone()];
        let bad_to = [from.clone(), evil.clone()];
        let outgoing = |from, to, message_id| Outgoing {
            from,
            to,
            cc: &[],
            bcc: &[],
            subject: "hi",
            text: "hi",
            html: None,
            quote: None,
            message_id,
            in_reply_to: None,
            references: &[],
            date: 0,
            eight_bit: false,
            attachments: Vec::new(),
        };
        let cases: [(&Address, &[Address], &str, &str); 4] = [
            (&evil, &to, "m@x", "address"),
            (&from, &bad_to, "m@x", "address"),
            (&from, &to, "id\rX-Inj: 1@x", "Message-ID"),
            (&from, &to, "a>b@x", "Message-ID"),
        ];
        for (from, to, message_id, field) in cases {
            let err = build_message(&outgoing(from, to, message_id)).unwrap_err();
            assert_eq!(err.field, field, "{err}");
        }
        assert!(build_message(&outgoing(&from, &to, "m@x")).is_ok());
    }

    #[test]
    fn drafts_keep_bcc() {
        let from = addr(None, "a@example.com");
        let raw = build_message(&Outgoing {
            from: &from,
            to: &[],
            cc: &[],
            bcc: &[addr(Some("Dave\r\nX-Evil: 1"), "d@example.com")],
            subject: "",
            text: "",
            html: None,
            quote: None,
            message_id: "m@example.com",
            in_reply_to: None,
            references: &[],
            date: 0,
            eight_bit: false,
            attachments: Vec::new(),
        })
        .unwrap();
        let m = MessageParser::new().parse(&raw).unwrap();
        let bcc = m.bcc().unwrap().first().unwrap();
        assert_eq!(
            (bcc.name(), bcc.address()),
            (Some("Dave X-Evil: 1"), Some("d@example.com"))
        );
        assert!(m.to().is_none() && m.headers().get("X-Evil").is_none());
    }

    #[test]
    fn html_goes_with_its_text_and_the_images_it_shows() {
        let from = addr(None, "a@example.com");
        let image = |content_id| OutgoingAttachment {
            filename: "shot.png",
            mime_type: "image/png",
            content_id: Some(content_id),
            data: b"\x89PNG",
        };
        let message = |html| Outgoing {
            from: &from,
            to: &[],
            cc: &[],
            bcc: &[],
            subject: "",
            text: "Look\n",
            html,
            quote: None,
            message_id: "m@example.com",
            in_reply_to: None,
            references: &[],
            date: 0,
            eight_bit: false,
            attachments: vec![
                image("shown@catmail"),
                // Taken out of the text since.
                image("gone@catmail"),
                OutgoingAttachment {
                    filename: "a.pdf",
                    mime_type: "application/pdf",
                    content_id: None,
                    data: b"%PDF",
                },
            ],
        };
        let raw = build_message(&message(Some(
            r#"<p>Look</p><p><img src="cid:shown@catmail" alt="shot.png"></p>"#,
        )))
        .unwrap();
        let wire = String::from_utf8_lossy(&raw);
        let at = |s: &str| wire.find(s).unwrap_or_else(|| panic!("{s} in {wire}"));
        assert!(
            at("multipart/mixed") < at("multipart/related")
                && at("multipart/related") < at("multipart/alternative")
                && at("multipart/alternative") < at("text/plain")
                && at("text/plain") < at("text/html")
                && at("text/html") < at("Content-ID: <shown@catmail>")
                && at("Content-ID: <shown@catmail>") < at("a.pdf"),
            "{wire}"
        );
        assert!(!wire.contains("gone@catmail"), "{wire}");
        let m = MessageParser::new().parse(&raw).unwrap();
        assert_eq!(m.body_text(0).unwrap().replace("\r\n", "\n"), "Look\n");
        assert!(
            m.body_html(0)
                .unwrap()
                .contains(r#"<img src="cid:shown@catmail""#)
        );
        assert_eq!(m.attachments().count(), 2);

        // Without HTML, the text has no images.
        let raw = build_message(&message(None)).unwrap();
        let wire = String::from_utf8_lossy(&raw);
        assert!(
            !wire.contains("Content-ID") && !wire.contains("text/html"),
            "{wire}"
        );
        assert_eq!(
            MessageParser::new()
                .parse(&raw)
                .unwrap()
                .attachments()
                .count(),
            1
        );
    }

    fn quoting<'a>(
        from: &'a Address,
        text: &'a str,
        html: Option<&'a str>,
        quote: &'a Quote,
        attachments: Vec<OutgoingAttachment<'a>>,
    ) -> Outgoing<'a> {
        Outgoing {
            from,
            to: &[],
            cc: &[],
            bcc: &[],
            subject: "Re: hi",
            text,
            html,
            quote: Some(quote),
            message_id: "m@example.com",
            in_reply_to: None,
            references: &[],
            date: 0,
            eight_bit: false,
            attachments,
        }
    }

    fn reply_quote(text: &str, html: Option<&str>) -> Quote {
        Quote {
            head: QuoteHead::Reply(Attribution {
                from: Some("A <a@example.com>".into()),
                date: Some("2026-11-02 09:30".into()),
            }),
            text: text.into(),
            html: html.map(String::from),
        }
    }

    /// The text, and the HTML (which the parser makes from the text if there is none).
    fn parts(raw: &[u8]) -> (String, Option<String>) {
        let m = MessageParser::new().parse(raw).unwrap();
        let text = m.body_text(0).unwrap().replace("\r\n", "\n");
        (text, m.body_html(0).map(|h| h.into_owned()))
    }

    #[test]
    fn a_reply_to_html_quotes_it_whole_with_its_styles_kept_to_it() {
        let from = addr(None, "b@example.com");
        let original = catmail_html::sanitize_html(
            r#"<style>body{background:#eee}p{color:red}</style><p>Hi <b>there</b></p><img src="cid:logo@catmail">"#,
        );
        let quote = reply_quote("Hi there", Some(&original));
        let logo = OutgoingAttachment {
            filename: "logo.png",
            mime_type: "image/png",
            content_id: Some("logo@catmail"),
            data: b"\x89PNG",
        };
        // A body without HTML of its own is written as paragraphs.
        let raw = build_message(&quoting(
            &from,
            "Thanks <3\n\nB\n",
            None,
            &quote,
            vec![logo],
        ))
        .unwrap();
        let (text, html) = parts(&raw);
        assert_eq!(
            text,
            "Thanks <3\n\nB\n\nOn 2026-11-02 09:30, A <a@example.com> wrote:\n> Hi there\n"
        );
        let html = html.unwrap();
        for expected in [
            ".catmail-original{background:#eee;}.catmail-original p{color:red;}</style></head>",
            r#"<div class="catmail-text"><p>Thanks &lt;3</p><p><br></p><p>B</p></div>"#,
            "<div>On 2026-11-02 09:30, A &lt;a@example.com&gt; wrote:</div><blockquote type=\"cite\"",
            &format!(
                r#"{CONTAINED}<div class="catmail-original"><p>Hi <b>there</b></p><img src="cid:logo@catmail"></div></div></blockquote>"#
            ),
        ] {
            assert!(html.contains(expected), "{expected} in {html}");
        }
        assert!(
            String::from_utf8_lossy(&raw).contains("Content-ID: <logo@catmail>"),
            "the quote's image goes with it"
        );
    }

    #[test]
    fn a_reply_to_text_stays_text() {
        let from = addr(None, "b@example.com");
        let quote = reply_quote("Hi\n> earlier\n>> first\nback", None);
        let raw = build_message(&quoting(&from, "Thanks", None, &quote, vec![])).unwrap();
        assert_eq!(
            parts(&raw).0,
            "Thanks\n\nOn 2026-11-02 09:30, A <a@example.com> wrote:\n> Hi\n> > earlier\n> >> first\n> back\n"
        );
        assert!(!String::from_utf8_lossy(&raw).contains("text/html"));
        // Under HTML, its quotes are quotes.
        let raw = build_message(&quoting(
            &from,
            "Thanks",
            Some("<p><b>Thanks</b></p>"),
            &quote,
            vec![],
        ))
        .unwrap();
        let html = parts(&raw).1.unwrap();
        let cite = format!("<blockquote{CITE}>");
        let expected = format!(
            r#"{cite}<div class="catmail-text"><p>Hi</p>{cite}<p>earlier</p>{cite}<p>first</p></blockquote></blockquote><p>back</p></div></blockquote>"#
        );
        assert!(html.contains(&expected), "{expected} in {html}");
        assert!(html.contains(r#"<div class="catmail-text"><p><b>Thanks</b></p></div>"#));
    }

    #[test]
    fn a_forward_is_not_quoted() {
        let from = addr(None, "b@example.com");
        let quote = Quote {
            head: QuoteHead::Forward(vec![
                (ForwardField::From, "A <a@example.com>".into()),
                (ForwardField::Subject, "Hi".into()),
            ]),
            text: "> not a quote of ours".into(),
            html: Some("<p>Original</p>".into()),
        };
        let raw = build_message(&quoting(&from, "", None, &quote, vec![])).unwrap();
        let (text, html) = parts(&raw);
        assert_eq!(
            text,
            "---------- Forwarded message ---------\nFrom: A <a@example.com>\nSubject: Hi\n\n\
             > not a quote of ours\n"
        );
        let html = html.unwrap();
        assert!(!html.contains("<blockquote"), "{html}");
        let expected = format!(
            r#"<div>---------- Forwarded message ---------</div>{HEADERS_TABLE}<tr><td{HEADER_NAME}>From:</td><td>A &lt;a@example.com&gt;</td></tr><tr><td{HEADER_NAME}>Subject:</td><td>Hi</td></tr></table><br>{CONTAINED}<div class="catmail-original"><p>Original</p></div></div>"#
        );
        assert!(html.contains(&expected), "{expected} in {html}");
    }

    #[test]
    fn a_quote_is_kept_from_covering_what_is_around_it() {
        let from = addr(None, "b@example.com");
        // Its own content moved over the forward's headers, and its `body` (the element
        // the sheet is scoped to) moved whole.
        let original = catmail_html::sanitize_html(
            r#"<style>body{margin-top:-400px!important;transform:translateY(-400px)}</style><div style="position:fixed;inset:0;background:#fff;z-index:9999">fake</div>"#,
        );
        let quote = Quote {
            head: QuoteHead::Forward(vec![(ForwardField::From, "A <a@example.com>".into())]),
            text: "fake".into(),
            html: Some(original),
        };
        let raw = build_message(&quoting(&from, "", None, &quote, vec![])).unwrap();
        let html = parts(&raw).1.unwrap();
        // The sheet reaches the element of its class, never the one clipping it.
        assert!(
            html.contains(".catmail-original{margin-top:-400px!important;transform:"),
            "{html}"
        );
        let expected = format!(
            r#"</table><br>{CONTAINED}<div class="catmail-original"><div style="position:fixed;"#
        );
        assert!(html.contains(&expected), "{expected} in {html}");
        assert!(CONTAINED.contains("contain:paint") && CONTAINED.contains("overflow:auto"));
        assert!(!CONTAINED.contains("class="));
    }

    #[test]
    fn empty_paragraphs_keep_their_line() {
        assert_eq!(
            fill_empty_paragraphs(
                r#"<p>a</p><p></p><p style="text-align: center"></p><blockquote><p></p></blockquote><p><b>x</b></p><pre></pre>"#
            ),
            r#"<p>a</p><p><br></p><p style="text-align: center"><br></p><blockquote><p><br></p></blockquote><p><b>x</b></p><pre></pre>"#
        );
        assert!(matches!(
            fill_empty_paragraphs("<p>a</p>"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn odd_mime_types_become_octet_stream() {
        assert_eq!(content_type("image/png"), "image/png");
        assert_eq!(
            content_type("text/plain; charset=x"),
            "application/octet-stream"
        );
        assert_eq!(content_type("bogus"), "application/octet-stream");
        assert_eq!(content_type("a/b\r\nX-Evil: 1"), "application/octet-stream");
    }

    #[test]
    fn a_message_attached_whole_goes_as_it_is() {
        const ORIGINAL: &[u8] = b"From: a@example.com\r\n\
            DKIM-Signature: v=1; b=abc\r\n\
            Subject: Original\r\n\
            Content-Type: text/plain; charset=utf-8\r\n\
            Content-Transfer-Encoding: 8bit\r\n\r\n\
            \xe4\xbd\xa0\xe5\xa5\xbd\r\n";
        let from = addr(None, "b@example.com");
        let build = |data: &'static [u8], eight_bit| {
            let raw = build_message(&Outgoing {
                from: &from,
                to: &[],
                cc: &[],
                bcc: &[],
                subject: "Fwd: Original",
                text: "See below.",
                html: None,
                quote: None,
                message_id: "m@example.com",
                in_reply_to: None,
                references: &[],
                date: 0,
                attachments: vec![OutgoingAttachment {
                    filename: "Original.eml",
                    mime_type: "message/rfc822",
                    content_id: None,
                    data,
                }],
                eight_bit,
            })
            .unwrap();
            let encoding = String::from_utf8_lossy(&raw)
                .split("Content-Type: message/rfc822")
                .nth(1)
                .and_then(|part| part.split("Content-Transfer-Encoding: ").nth(1))
                .and_then(|rest| rest.split("\r\n").next())
                .map(String::from);
            (raw, encoding)
        };
        let contains = |raw: &[u8], data: &[u8]| raw.windows(data.len()).any(|w| w == data);

        // Where 8-bit content may go: untouched, signature and all.
        let (raw, encoding) = build(ORIGINAL, true);
        assert_eq!(encoding.as_deref(), Some("8bit"));
        assert!(contains(&raw, ORIGINAL));
        let parsed = MessageParser::new().parse(&raw).unwrap();
        let attached = parsed.attachments().next().unwrap();
        assert_eq!(attached.attachment_name(), Some("Original.eml"));
        let mail_parser::PartKind::Message(inner) = attached.kind() else {
            panic!("not read as a message: {:?}", attached.kind());
        };
        assert_eq!(inner.subject(), Some("Original"));
        assert_eq!(inner.body_text(0).unwrap().trim_end(), "你好");

        // 7-bit clean goes as it is anywhere.
        let ascii: &[u8] = b"Subject: Plain\r\n\r\nHello\r\n";
        let (raw, encoding) = build(ascii, false);
        assert_eq!(encoding.as_deref(), Some("7bit"));
        assert!(contains(&raw, ascii) && raw.is_ascii());

        // Where it may not, or what no SMTP server would carry as it is: base64.
        let long: &'static [u8] = Box::leak(
            [&b"Subject: Long\r\n\r\n"[..], &[b'a'; 999], b"\r\n"]
                .concat()
                .into_boxed_slice(),
        );
        for data in [
            ORIGINAL,
            b"Subject: LF\n\nHello\n",
            b"Subject: \0\r\n",
            long,
        ] {
            let (raw, encoding) = build(data, data != ORIGINAL);
            assert_eq!(encoding.as_deref(), Some("base64"), "{data:?}");
            assert!(raw.is_ascii());
        }
    }
}
