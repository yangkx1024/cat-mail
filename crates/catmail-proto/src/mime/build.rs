//! Building outgoing messages with `mail-builder`.

use std::borrow::Cow;

use catmail_types::Address;
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
    /// Without angle brackets.
    pub message_id: &'a str,
    pub in_reply_to: Option<&'a str>,
    pub references: &'a [String],
    /// Unix seconds.
    pub date: i64,
    pub attachments: Vec<OutgoingAttachment<'a>>,
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
/// encoded words, bodies as quoted-printable or base64 where needed).
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
    let text = MimePart::new("text/plain", message.text);
    let content = match message.html {
        None => text,
        Some(html) => {
            let alternative = MimePart::new(
                "multipart/alternative",
                vec![text, MimePart::new("text/html", html_document(html))],
            );
            let shown = catmail_html::cid_image_ids(html);
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
            MimePart::new(content_type(a.mime_type), a.data)
                .attachment(one_line(a.filename).into_owned())
        })
        .collect();
    wrap("multipart/mixed", content, files)
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

/// The body's content as a document of its own, shown as the editor showed it: a
/// paragraph is a line, with no room around it, and an empty one an empty line; spaces
/// as written (runs of them, indentation; the content has no whitespace between its
/// tags); images no wider than the reader's window.
fn html_document(content: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\
         <style>body {{ white-space: pre-wrap; }} p {{ margin: 0; }} \
         img {{ max-width: 100%; height: auto; }}</style></head>\n\
         <body>{}</body></html>\n",
        fill_empty_paragraphs(content)
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

/// mail-builder writes addresses and the Message-ID between angle brackets as they are,
/// and a bare CR in one becomes a line break on the way out, starting a header of the
/// sender's choosing. Unlike threading IDs, they can't just be left out.
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
            message_id: "abc.123@example.com",
            in_reply_to: Some("parent@x"),
            references: &references,
            date: 1_792_200_600,
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
            message_id: "m@example.com",
            in_reply_to: Some("evil\rX-Injected: yes@x"),
            references: &references,
            date: 0,
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
            message_id,
            in_reply_to: None,
            references: &[],
            date: 0,
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
            message_id: "m@example.com",
            in_reply_to: None,
            references: &[],
            date: 0,
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
            message_id: "m@example.com",
            in_reply_to: None,
            references: &[],
            date: 0,
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
}
