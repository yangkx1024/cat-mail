//! What a reply or forward starts with: its recipients, subject and quote, and the name
//! of a message forwarded whole.

use std::collections::HashSet;

use catmail_types::{
    Address, Attribution, DraftBody, DraftContent, ForwardField, IdentityId, Message, Quote,
    QuoteHead,
};

use crate::display;
use crate::filename::safe_filename;

/// An address of the account: an identity's, or its own (`None`, written as the default).
pub(crate) type Own = (Option<IdentityId>, String);

/// Recipients, subject and quote of a reply to `message`, written by the account
/// whose addresses are `own` (each with the identity that sends as it), and the identity
/// it is sent as: that of the address the message came from or went to (`None`, the
/// default, for none).
pub(crate) fn reply_content(
    message: &Message,
    own: &[Own],
    all: bool,
) -> (DraftContent, Option<IdentityId>) {
    let ReplyRecipients { to, cc, written_as } = reply_recipients(message, own, all);
    let content = DraftContent {
        subject: reply_subject(message.summary.subject.as_deref().unwrap_or_default()),
        to,
        cc,
        bcc: Vec::new(),
        body: DraftBody::default(),
        quote: Some(quote(message, QuoteHead::Reply(attribution(message)))),
    };
    (content, written_as)
}

pub(crate) struct ReplyRecipients {
    to: Vec<Address>,
    /// Empty unless replying to all.
    pub(crate) cc: Vec<Address>,
    written_as: Option<IdentityId>,
}

pub(crate) fn reply_recipients(message: &Message, own: &[Own], all: bool) -> ReplyRecipients {
    let mine = |a: &Address| {
        own.iter()
            .find(|(_, email)| a.email.eq_ignore_ascii_case(email))
            .map(|(identity, _)| *identity)
    };
    let is_me = |a: &Address| mine(a).is_some();
    let sender = &message.summary.from;
    // Replying to one's own message continues the conversation with its recipients.
    let from_me = sender.iter().any(is_me);
    let written_as = if from_me {
        sender.iter().find_map(mine)
    } else {
        message
            .summary
            .to
            .iter()
            .chain(&message.summary.cc)
            .find_map(mine)
    };
    let to = if from_me {
        message.summary.to.clone()
    } else if !message.reply_to.is_empty() {
        message.reply_to.clone()
    } else {
        sender.clone()
    };
    let mut seen = HashSet::new();
    let mut unique = |a: &Address| seen.insert(a.email.to_ascii_lowercase());
    let to: Vec<Address> = to.into_iter().filter(|a| unique(a)).collect();
    let cc = if all {
        // Those it went to, but for one's own message they are already in `to`.
        let to_others: &[Address] = if from_me { &[] } else { &message.summary.to };
        to_others
            .iter()
            .chain(&message.summary.cc)
            .filter(|a| !is_me(a) && unique(a))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    ReplyRecipients {
        to,
        cc,
        written_as: written_as.flatten(),
    }
}

fn reply_subject(subject: &str) -> String {
    prefixed(
        subject,
        "Re",
        &["re:", "re：", "回复:", "回复：", "答复:", "答复："],
    )
}

pub(crate) fn forward_subject(subject: &str) -> String {
    prefixed(
        subject,
        "Fwd",
        &["fwd:", "fw:", "fwd：", "fw：", "转发:", "转发："],
    )
}

/// `subject` with `prefix: ` in front, unless it starts with one of the `known` ways
/// (lowercase) of writing it already.
fn prefixed(subject: &str, prefix: &str, known: &[&str]) -> String {
    let subject = subject.trim();
    let lower = subject.to_lowercase();
    if known.iter().any(|known| lower.starts_with(known)) {
        subject.to_owned()
    } else {
        format!("{prefix}: {subject}")
    }
}

/// `message` quoted, introduced by `head`: its text, and its HTML if it has any.
fn quote(message: &Message, head: QuoteHead) -> Quote {
    let body = message.body.as_ref();
    Quote {
        head,
        text: body.and_then(|b| b.text.clone()).unwrap_or_default(),
        html: body.and_then(|b| b.html.clone()),
    }
}

fn attribution(message: &Message) -> Attribution {
    Attribution {
        from: message.summary.from.first().map(Address::to_string),
        date: display::local_date(message.summary.date),
    }
}

/// Subject and quote of a forward of `message`: its headers, then the message as it was.
pub(crate) fn forward_content(message: &Message) -> DraftContent {
    let subject = message.summary.subject.as_deref().unwrap_or_default();
    let headers = [
        (
            ForwardField::From,
            display::addresses(&message.summary.from),
        ),
        (ForwardField::Date, display::date(message.summary.date)),
        (ForwardField::Subject, subject.trim().to_owned()),
        (ForwardField::To, display::addresses(&message.summary.to)),
        (ForwardField::Cc, display::addresses(&message.summary.cc)),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .collect();
    DraftContent {
        subject: forward_subject(subject),
        quote: Some(quote(message, QuoteHead::Forward(headers))),
        ..Default::default()
    }
}

/// What a message attached whole is named: its subject, as Thunderbird and Outlook name
/// it, with `.eml`, made safe to save (see [`safe_filename`]).
pub(crate) fn eml_name(subject: &str) -> String {
    // A slash is part of the subject, not a directory.
    let subject = subject.replace(['/', '\\'], "_");
    let subject = subject.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if subject.is_empty() {
        return "message.eml".into();
    }
    safe_filename(Some(&format!("{subject}.eml")), "message.eml")
}

#[cfg(test)]
mod tests {
    use catmail_types::{AccountId, Flags, FolderId, MessageBody, MessageId, MessageSummary};

    use super::*;

    fn addr(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(String::from),
            email: email.into(),
        }
    }

    fn message(from: Address, to: Vec<Address>, cc: Vec<Address>) -> Message {
        Message {
            summary: MessageSummary {
                id: MessageId(1),
                account_id: AccountId(1),
                folder_id: FolderId(1),
                subject: Some("季度报告".into()),
                from: vec![from],
                to,
                cc,
                date: None,
                snippet: None,
                flags: Flags::empty(),
                has_attachments: false,
                labels: Vec::new(),
            },
            reply_to: Vec::new(),
            message_id: Some("m1@example.com".into()),
            references: Vec::new(),
            size: None,
            body: Some(MessageBody {
                text: Some("第一行\n\nsecond\n".into()),
                html: None,
                attachments: Vec::new(),
            }),
        }
    }

    fn me() -> Vec<Own> {
        vec![(None, "me@example.com".into())]
    }

    fn emails(list: &[Address]) -> Vec<&str> {
        list.iter().map(|a| a.email.as_str()).collect()
    }

    #[test]
    fn reply_goes_to_the_sender_and_reply_all_to_everyone_else() {
        let m = message(
            addr(Some("张三"), "zhang@example.com"),
            vec![addr(None, "ME@example.com"), addr(None, "li@example.com")],
            vec![
                addr(None, "wang@example.com"),
                addr(None, "Zhang@example.com"),
            ],
        );
        let (reply, _) = reply_content(&m, &me(), false);
        assert_eq!(emails(&reply.to), ["zhang@example.com"]);
        assert!(reply.cc.is_empty());
        assert_eq!(reply.subject, "Re: 季度报告");
        assert_eq!(reply.body, DraftBody::default(), "the user's to write");
        assert_eq!(
            reply.text_as_sent(),
            "张三 <zhang@example.com> wrote:\n> 第一行\n>\n> second\n"
        );

        let (all, _) = reply_content(&m, &me(), true);
        assert_eq!(emails(&all.to), ["zhang@example.com"]);
        assert_eq!(
            emails(&all.cc),
            ["li@example.com", "wang@example.com"],
            "not me, not twice"
        );
    }

    #[test]
    fn reply_to_and_own_messages() {
        let mut m = message(
            addr(None, "zhang@example.com"),
            vec![addr(None, "list@example.com")],
            vec![],
        );
        m.reply_to = vec![addr(None, "list@example.com")];
        let (reply, _) = reply_content(&m, &me(), true);
        assert_eq!(emails(&reply.to), ["list@example.com"]);
        assert!(reply.cc.is_empty());

        let mine = message(
            addr(None, "me@example.com"),
            vec![addr(None, "zhang@example.com")],
            vec![addr(None, "li@example.com")],
        );
        let (reply, _) = reply_content(&mine, &me(), true);
        assert_eq!(emails(&reply.to), ["zhang@example.com"]);
        assert_eq!(emails(&reply.cc), ["li@example.com"]);
    }

    #[test]
    fn replies_are_written_as_the_address_written_to() {
        // The default, another, and the account's own address no identity sends as.
        let own = [
            (Some(IdentityId(1)), "me@example.com".into()),
            (Some(IdentityId(2)), "news@example.com".into()),
            (None, "account@example.com".into()),
        ];
        let to = |to: &str, cc: &str| {
            message(
                addr(None, "zhang@example.com"),
                vec![addr(None, to)],
                vec![addr(None, cc)],
            )
        };
        let as_ = |m: &Message| reply_content(m, &own, true).1;
        assert_eq!(
            as_(&to("News@example.com", "me@example.com")),
            Some(IdentityId(2))
        );
        assert_eq!(
            as_(&to("li@example.com", "news@example.com")),
            Some(IdentityId(2))
        );
        assert_eq!(
            as_(&to("me@example.com", "li@example.com")),
            Some(IdentityId(1))
        );
        assert_eq!(as_(&to("account@example.com", "li@example.com")), None);
        assert_eq!(as_(&to("li@example.com", "wang@example.com")), None);
        let (all, _) = reply_content(&to("li@example.com", "account@example.com"), &own, true);
        assert_eq!(emails(&all.cc), ["li@example.com"], "the account is me");
        let (all, _) = reply_content(&to("news@example.com", "li@example.com"), &own, true);
        assert_eq!(emails(&all.cc), ["li@example.com"], "an identity is me too");

        let mine = message(
            addr(None, "news@example.com"),
            vec![addr(None, "zhang@example.com")],
            vec![],
        );
        let (reply, from) = reply_content(&mine, &own, true);
        assert_eq!(
            (emails(&reply.to), from),
            (vec!["zhang@example.com"], Some(IdentityId(2)))
        );
    }

    #[test]
    fn reply_subjects() {
        assert_eq!(reply_subject("hello"), "Re: hello");
        assert_eq!(reply_subject("RE: hello"), "RE: hello");
        assert_eq!(reply_subject("回复：你好"), "回复：你好");
        assert_eq!(reply_subject(""), "Re: ");
    }

    #[test]
    fn a_message_attached_whole_is_named_after_its_subject() {
        assert_eq!(eml_name("Report"), "Report.eml");
        assert_eq!(eml_name("Re: a/b\\c?*x*  "), "Re_ a_b_c__x_.eml");
        assert_eq!(eml_name("Sent..."), "Sent.eml");
        assert_eq!(eml_name(" ... "), "message.eml");
        assert_eq!(eml_name("CON"), "_CON.eml");
        let long = eml_name(&"长".repeat(300));
        assert!(long.len() <= 200 && long.ends_with("长.eml"), "{long}");
    }

    #[test]
    fn forward_subjects() {
        assert_eq!(forward_subject("hello"), "Fwd: hello");
        assert_eq!(forward_subject(" FW: hello"), "FW: hello");
        assert_eq!(forward_subject("Fwd：报告"), "Fwd：报告");
        assert_eq!(forward_subject("转发：你好"), "转发：你好");
        assert_eq!(forward_subject("Re: hello"), "Fwd: Re: hello");
    }

    #[test]
    fn forward_shows_the_headers_above_the_text_unquoted() {
        let m = message(
            addr(Some("张三"), "zhang@example.com"),
            vec![addr(None, "me@example.com"), addr(None, "li@example.com")],
            vec![],
        );
        let forward = forward_content(&m);
        assert_eq!(forward.subject, "Fwd: 季度报告");
        assert!(forward.recipients().next().is_none());
        assert_eq!(forward.body, DraftBody::default());
        let quote = forward.quote.as_ref().unwrap();
        assert_eq!(
            quote.head,
            QuoteHead::Forward(vec![
                (ForwardField::From, "张三 <zhang@example.com>".into()),
                (ForwardField::Subject, "季度报告".into()),
                (ForwardField::To, "me@example.com, li@example.com".into()),
            ])
        );
        assert_eq!(
            forward.text_as_sent(),
            "---------- Forwarded message ---------\n\
             From: 张三 <zhang@example.com>\n\
             Subject: 季度报告\n\
             To: me@example.com, li@example.com\n\
             \n\
             第一行\n\nsecond\n"
        );
    }
}
