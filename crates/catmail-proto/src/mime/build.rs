//! Building outgoing messages with `mail-builder`.

use std::borrow::Cow;

use catmail_types::Address;
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address as BuilderAddress;
use mail_builder::headers::message_id::MessageId;

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
    /// Plain text; line breaks may be LF or CRLF.
    pub text: &'a str,
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
    pub data: &'a [u8],
}

/// The message in wire form: CRLF line endings, 7-bit clean (headers as RFC 2047
/// encoded words, bodies as quoted-printable or base64 where needed).
pub fn build_message(message: &Outgoing<'_>) -> Vec<u8> {
    let mut builder = MessageBuilder::new()
        .message_id(message.message_id)
        .date(message.date)
        .subject(one_line(message.subject))
        .text_body(message.text)
        .from(address(message.from));
    if !message.to.is_empty() {
        builder = builder.to(address_list(message.to));
    }
    if !message.cc.is_empty() {
        builder = builder.cc(address_list(message.cc));
    }
    if !message.bcc.is_empty() {
        builder = builder.bcc(address_list(message.bcc));
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
    for a in &message.attachments {
        builder = builder.attachment(
            content_type(a.mime_type),
            one_line(a.filename).into_owned(),
            a.data,
        );
    }
    builder
        .write_to_vec()
        .expect("writing into a Vec can't fail")
}

fn address(a: &Address) -> BuilderAddress<'_> {
    BuilderAddress::new_address(
        a.name.as_deref().map(|n| one_line(n).into_owned()),
        a.email.as_str(),
    )
}

fn address_list(list: &[Address]) -> BuilderAddress<'_> {
    BuilderAddress::new_list(list.iter().map(address).collect())
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
    use mail_parser::{MessageParser, MimeHeaders};

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
            message_id: "abc.123@example.com",
            in_reply_to: Some("parent@x"),
            references: &references,
            date: 1_792_200_600,
            attachments: vec![OutgoingAttachment {
                filename: "报告 \"final\".pdf",
                mime_type: "application/pdf",
                data: b"%PDF-1.4\x00\xff",
            }],
        });
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
            .iter()
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
        assert_eq!(m.in_reply_to().as_text(), Some("parent@x"));
        assert_eq!(
            m.references().as_text_list().unwrap().to_vec(),
            ["root@x", "parent@x"]
        );
        assert_eq!(m.date().unwrap().to_timestamp(), 1_792_200_600);
        assert_eq!(
            m.body_text(0).unwrap().replace("\r\n", "\n"),
            "第一行\n.\nlast line with trailing space "
        );
        let attachment = m.attachment(0).unwrap();
        assert_eq!(attachment.attachment_name(), Some("报告 \"final\".pdf"));
        assert_eq!(attachment.content_type().unwrap().ctype(), "application");
        assert_eq!(attachment.contents(), b"%PDF-1.4\x00\xff");
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
            message_id: "m@example.com",
            in_reply_to: Some("evil\rX-Injected: yes@x"),
            references: &references,
            date: 0,
            attachments: Vec::new(),
        });
        assert!(
            raw.windows(2).all(|w| w[0] != b'\r' || w[1] == b'\n'),
            "no bare CR"
        );
        let m = MessageParser::new().parse(&raw).unwrap();
        assert!(m.header("X-Injected").is_none() && m.header("X-R").is_none());
        assert!(m.in_reply_to().is_empty());
        assert_eq!(m.references().as_text(), Some("ok@x"));
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
            message_id: "m@example.com",
            in_reply_to: None,
            references: &[],
            date: 0,
            attachments: Vec::new(),
        });
        let m = MessageParser::new().parse(&raw).unwrap();
        let bcc = m.bcc().unwrap().first().unwrap();
        assert_eq!(
            (bcc.name(), bcc.address()),
            (Some("Dave X-Evil: 1"), Some("d@example.com"))
        );
        assert!(m.to().is_none() && m.header("X-Evil").is_none());
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
