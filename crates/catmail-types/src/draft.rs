use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::{AccountId, Address, DraftAttachmentId, DraftId, IdentityId, MessageId, OutboxId};

/// What the user writes; with the identity it is sent as, everything a draft update
/// replaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftContent {
    pub subject: String,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Sent to, but left out of the headers.
    pub bcc: Vec<Address>,
    pub body: DraftBody,
    /// The message a reply or forward carries, sent below the body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<Quote>,
}

impl DraftContent {
    /// To, Cc and Bcc, in that order.
    pub fn recipients(&self) -> impl Iterator<Item = &Address> {
        self.to.iter().chain(&self.cc).chain(&self.bcc)
    }

    /// The plain text as sent: the body's, then the quote's.
    pub fn text_as_sent(&self) -> String {
        match &self.quote {
            Some(quote) => quote.below(&self.body.text),
            None => self.body.text.clone(),
        }
    }
}

/// The message's text, plain or formatted. One value: the plain text of a formatted
/// message is its other form, written with the HTML, so the two are only ever replaced
/// together.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftBody {
    /// Plain text. With `html`, the same message for readers that show only text: it
    /// is sent as the HTML's alternative.
    pub text: String,
    /// The message formatted, as the composer writes it, or `None` for plain text. Its
    /// images are inline attachments of the draft, by `cid:` URL
    /// ([`DraftAttachment::content_id`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

impl DraftBody {
    /// Text without formatting, as an agent or the CLI writes it.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            html: None,
        }
    }

    pub fn is_formatted(&self) -> bool {
        self.html.is_some()
    }
}

/// What a reply or forward carries of the message it answers. Kept apart from the body:
/// the editor can't hold a message's HTML as it is, so the quote is sent as it was taken,
/// below what the user wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    /// What introduces it: who wrote the message and when, or a forward's headers.
    pub head: QuoteHead,
    /// The message's text.
    pub text: String,
    /// The message's HTML as it was sanitized when stored (a style sheet in front, if
    /// any), its images inline attachments of the draft by `cid:` URL
    /// ([`DraftAttachment::content_id`]). `None` for a message of text only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

/// What introduces a quote, as data, and how the message follows it: quoted under a
/// reply's attribution (`> ` in the text, a `<blockquote type="cite">` in the HTML), as it
/// was under a forward's headers. Worded here ([`Attribution`]'s line,
/// [`ForwardField::label`], [`FORWARD_SEPARATOR`]); each place that shows it lays it out
/// (a table in the HTML sent, names and values apart in the composer). The values are the
/// text shown, as they read when the draft was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteHead {
    Reply(Attribution),
    /// The message's headers, those it has, in the order shown.
    Forward(Vec<(ForwardField, String)>),
}

/// Who wrote the message a reply quotes, and when.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribution {
    /// The sender, `Name <address>`; `None` for a message without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// When it was sent, `2026-10-19 14:05` in the time zone the draft was made in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

/// A header a forward shows of the message it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardField {
    From,
    Date,
    Subject,
    To,
    Cc,
}

/// The line a forward's headers follow, wherever it is shown as text.
pub const FORWARD_SEPARATOR: &str = "---------- Forwarded message ---------";

impl Quote {
    /// `text`, then the quote, as the plain text of a message shows them.
    pub fn below(&self, text: &str) -> String {
        with_quote(text, &self.as_text())
    }

    /// The quote as plain text: a reply's attribution, then the message's text
    /// `> `-quoted; or a forward's separator, a `Name: value` line for each header, and
    /// the message's text as it was.
    pub fn as_text(&self) -> String {
        let text = self.text.trim_end();
        let mut out = String::new();
        match &self.head {
            QuoteHead::Reply(attribution) => {
                let _ = writeln!(out, "{attribution}");
                for line in text.lines() {
                    out.push('>');
                    if !line.is_empty() {
                        out.push(' ');
                        out.push_str(line);
                    }
                    out.push('\n');
                }
            }
            QuoteHead::Forward(headers) => {
                out.push_str(FORWARD_SEPARATOR);
                out.push('\n');
                for (field, value) in headers {
                    let _ = writeln!(out, "{}: {value}", field.label());
                }
                out.push('\n');
                if !text.is_empty() {
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
        out
    }
}

impl QuoteHead {
    /// Every value it shows, to change in place.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut String> {
        let (attribution, headers) = match self {
            QuoteHead::Reply(Attribution { from, date }) => {
                (Some(from.iter_mut().chain(date)), None)
            }
            QuoteHead::Forward(headers) => (None, Some(headers.iter_mut().map(|(_, v)| v))),
        };
        attribution
            .into_iter()
            .flatten()
            .chain(headers.into_iter().flatten())
    }
}

/// `On 2026-10-19 14:05, A <a@example.com> wrote:`.
impl std::fmt::Display for Attribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(date) = &self.date {
            write!(f, "On {date}, ")?;
        }
        let from = self.from.as_deref().unwrap_or("someone");
        write!(f, "{from} wrote:")
    }
}

impl ForwardField {
    /// The header's name as shown.
    pub fn label(self) -> &'static str {
        match self {
            ForwardField::From => "From",
            ForwardField::Date => "Date",
            ForwardField::Subject => "Subject",
            ForwardField::To => "To",
            ForwardField::Cc => "Cc",
        }
    }
}

/// `text`, then `quote` (a quote as plain text), as a message's plain text shows them.
fn with_quote(text: &str, quote: &str) -> String {
    let mut out = text.trim_end().to_owned();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(quote);
    out
}

/// What a draft update changes; `None` leaves that part as it is. Made to the draft as it
/// is when updated, so what someone else changed meanwhile (an editor saving it) to the
/// other parts stays.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftChanges {
    /// The identity to send it as.
    pub from: Option<IdentityId>,
    pub subject: Option<String>,
    pub to: Option<Vec<Address>>,
    pub cc: Option<Vec<Address>>,
    pub bcc: Option<Vec<Address>>,
    /// The whole body: a plain one ([`DraftBody::plain`]) takes the formatting off.
    pub body: Option<DraftBody>,
    /// `Some(None)` takes the quote out.
    pub quote: Option<Option<Quote>>,
}

impl DraftChanges {
    /// Everything the user writes replaced, as an editor saves what it shows. The quote,
    /// which they don't write, stays as it is (`content.quote` is ignored): an editor
    /// saving as they type needn't hand the whole message quoted back each time.
    pub fn all(from: IdentityId, content: DraftContent) -> Self {
        let DraftContent {
            subject,
            to,
            cc,
            bcc,
            body,
            quote: _,
        } = content;
        Self {
            from: Some(from),
            subject: Some(subject),
            to: Some(to),
            cc: Some(cc),
            bcc: Some(bcc),
            body: Some(body),
            quote: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Makes the changes to `content`; returns the identity, if one is given.
    pub fn apply(self, content: &mut DraftContent) -> Option<IdentityId> {
        let Self {
            from,
            subject,
            to,
            cc,
            bcc,
            body,
            quote,
        } = self;
        fn replace<T>(field: &mut T, value: Option<T>) {
            if let Some(value) = value {
                *field = value;
            }
        }
        replace(&mut content.subject, subject);
        replace(&mut content.to, to);
        replace(&mut content.cc, cc);
        replace(&mut content.bcc, bcc);
        replace(&mut content.body, body);
        replace(&mut content.quote, quote);
        from
    }
}

/// What a forward starts with besides the message: who it goes to, and the user's text,
/// written above the forwarded message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forwarding {
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub text: String,
    /// Attaches the message whole, exactly as the server stores it (`message/rfc822`,
    /// named after its subject with `.eml`), instead of quoting it and copying its
    /// attachments: its every header and its MIME structure go along, for whoever has to
    /// check the original (a suspicious message to an administrator) or process it.
    pub as_attachment: bool,
}

/// A draft made from a message, by what it takes of the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// A reply: the body, quoted, with the images it shows.
    Reply,
    /// A forward: the body, quoted, with the images it shows, and the attachments.
    Forward,
    /// A forward as an attachment ([`Forwarding::as_attachment`]): the raw message.
    ForwardAttached,
}

impl Forwarding {
    /// What forwarding takes of the message.
    pub fn answer(&self) -> Answer {
        if self.as_attachment {
            Answer::ForwardAttached
        } else {
            Answer::Forward
        }
    }

    /// To, Cc and Bcc, in that order.
    pub fn recipients(&self) -> impl Iterator<Item = &Address> {
        self.to.iter().chain(&self.cc).chain(&self.bcc)
    }
}

/// A message being written. Stored locally, with a copy in the server's Drafts folder
/// that follows each change; it is sent through the outbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub id: DraftId,
    pub account_id: AccountId,
    /// Sent as this identity of the account.
    pub identity: IdentityId,
    #[serde(flatten)]
    pub content: DraftContent,
    pub attachments: Vec<DraftAttachment>,
    /// The message this replies to, while it is still stored.
    pub reply_to: Option<MessageId>,
    /// Message-IDs (without angle brackets) of the message replied to and its thread,
    /// for the In-Reply-To and References headers. Kept here so they survive the
    /// original being deleted.
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// Set while the draft is in the outbox: it can't be changed until the send is
    /// cancelled, and is deleted once sent.
    pub outbox: Option<OutboxId>,
    /// The server's Drafts folder has this version of it. Uploads happen at the next
    /// sync or replay of queued changes, and only if the account has a Drafts folder.
    pub on_server: bool,
    /// Why the server refused the last upload, until one succeeds.
    pub upload_error: Option<String>,
    /// Unix timestamp, seconds.
    pub updated_at: i64,
}

/// A draft as a list shows it: all of it but the HTML of its text and of its quote,
/// which a list doesn't show, and which can be a whole message for each draft listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSummary {
    pub id: DraftId,
    pub account_id: AccountId,
    pub identity: IdentityId,
    pub subject: String,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    /// What the user wrote, as plain text ([`DraftBody::text`]).
    pub text: String,
    /// The text is formatted too ([`DraftBody::html`]).
    pub formatted: bool,
    /// The quote, as plain text ([`Quote::as_text`]).
    pub quote: Option<String>,
    pub attachments: Vec<DraftAttachment>,
    pub reply_to: Option<MessageId>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub outbox: Option<OutboxId>,
    pub on_server: bool,
    pub upload_error: Option<String>,
    pub updated_at: i64,
}

impl DraftSummary {
    /// The plain text as sent: what the user wrote, then the quote.
    pub fn text_as_sent(&self) -> String {
        match &self.quote {
            Some(quote) => with_quote(&self.text, quote),
            None => self.text.clone(),
        }
    }

    /// To, Cc and Bcc, in that order.
    pub fn recipients(&self) -> impl Iterator<Item = &Address> {
        self.to.iter().chain(&self.cc).chain(&self.bcc)
    }
}

impl From<&Draft> for DraftSummary {
    fn from(d: &Draft) -> Self {
        let c = &d.content;
        Self {
            id: d.id,
            account_id: d.account_id,
            identity: d.identity,
            subject: c.subject.clone(),
            to: c.to.clone(),
            cc: c.cc.clone(),
            bcc: c.bcc.clone(),
            text: c.body.text.clone(),
            formatted: c.body.is_formatted(),
            quote: c.quote.as_ref().map(Quote::as_text),
            attachments: d.attachments.clone(),
            reply_to: d.reply_to,
            in_reply_to: d.in_reply_to.clone(),
            references: d.references.clone(),
            outbox: d.outbox,
            on_server: d.on_server,
            upload_error: d.upload_error.clone(),
            updated_at: d.updated_at,
        }
    }
}

/// A file attached to a draft. Its content was copied in when it was added, so the
/// original file may change or go away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftAttachment {
    pub id: DraftAttachmentId,
    pub filename: String,
    /// Lower-case `type/subtype`.
    pub mime_type: String,
    pub size: u64,
    /// An image in the draft's HTML (`cid:` this), not a file attached to it. Sent only
    /// while the HTML shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
}

impl DraftAttachment {
    /// An image of the draft's text rather than a file attached.
    pub fn in_text(&self) -> bool {
        self.content_id.is_some()
    }
}

/// A file to attach to a draft: one on disk, copied in as it is when attached, or the
/// content itself, as an agent hands it over (it has no access to the files here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewFile {
    Path(std::path::PathBuf),
    Content {
        /// The attachment's file name: a name, not a path.
        name: String,
        /// Shared: cloning it doesn't copy the file, so a caller can keep it as well (an
        /// upload, for a call that fails).
        content: bytes::Bytes,
    },
}

impl NewFile {
    /// Its file name, as the attachment will be called.
    pub fn name(&self) -> String {
        match self {
            NewFile::Path(path) => path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            NewFile::Content { name, .. } => name.clone(),
        }
    }

    /// What an error about it calls it: the path, or the name.
    pub fn describe(&self) -> String {
        match self {
            NewFile::Path(path) => path.display().to_string(),
            NewFile::Content { name, .. } => name.clone(),
        }
    }
}

impl From<std::path::PathBuf> for NewFile {
    fn from(path: std::path::PathBuf) -> Self {
        NewFile::Path(path)
    }
}

impl From<&std::path::PathBuf> for NewFile {
    fn from(path: &std::path::PathBuf) -> Self {
        NewFile::Path(path.clone())
    }
}

impl From<&std::path::Path> for NewFile {
    fn from(path: &std::path::Path) -> Self {
        NewFile::Path(path.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_body_takes_the_formatting_off() {
        let formatted = DraftBody {
            text: "Hi\n".into(),
            html: Some("<p><b>Hi</b></p>".into()),
        };
        let mut content = DraftContent {
            body: formatted.clone(),
            ..DraftContent::default()
        };
        DraftChanges {
            subject: Some("Re: x".into()),
            ..DraftChanges::default()
        }
        .apply(&mut content);
        assert_eq!(content.body, formatted);
        DraftChanges {
            body: Some(DraftBody::plain("Bye\n")),
            ..DraftChanges::default()
        }
        .apply(&mut content);
        assert_eq!(content.body, DraftBody::plain("Bye\n"));
    }

    fn quote(head: QuoteHead) -> Quote {
        Quote {
            head,
            text: "hi\n\n> earlier\n\n".into(),
            html: None,
        }
    }

    fn reply() -> Quote {
        quote(QuoteHead::Reply(Attribution {
            from: Some("A".into()),
            date: Some("2026-11-02 09:30".into()),
        }))
    }

    #[test]
    fn a_reply_quotes_the_text_below_the_body() {
        let mut content = DraftContent {
            body: DraftBody::plain("Thanks\n\n"),
            quote: Some(reply()),
            ..DraftContent::default()
        };
        assert_eq!(
            content.text_as_sent(),
            "Thanks\n\nOn 2026-11-02 09:30, A wrote:\n> hi\n>\n> > earlier\n"
        );
        content.body = DraftBody::default();
        assert_eq!(
            content.text_as_sent(),
            "On 2026-11-02 09:30, A wrote:\n> hi\n>\n> > earlier\n"
        );
    }

    #[test]
    fn an_attribution_says_what_it_knows() {
        let mut attribution = Attribution {
            from: Some("A <a@example.com>".into()),
            date: Some("2026-11-02 09:30".into()),
        };
        assert_eq!(
            attribution.to_string(),
            "On 2026-11-02 09:30, A <a@example.com> wrote:"
        );
        attribution.date = None;
        assert_eq!(attribution.to_string(), "A <a@example.com> wrote:");
        assert_eq!(Attribution::default().to_string(), "someone wrote:");
    }

    #[test]
    fn a_forward_carries_the_text_as_it_was_below_its_headers() {
        let mut content = DraftContent {
            body: DraftBody::plain("FYI"),
            quote: Some(quote(QuoteHead::Forward(vec![
                (ForwardField::From, "A".into()),
                (ForwardField::Subject, "Hi".into()),
            ]))),
            ..DraftContent::default()
        };
        assert_eq!(
            content.text_as_sent(),
            "FYI\n\n---------- Forwarded message ---------\nFrom: A\nSubject: Hi\n\n\
             hi\n\n> earlier\n"
        );
        content.quote.as_mut().unwrap().head = QuoteHead::Forward(Vec::new());
        assert_eq!(
            content.text_as_sent(),
            "FYI\n\n---------- Forwarded message ---------\n\nhi\n\n> earlier\n"
        );
    }

    #[test]
    fn a_quote_reads_back_as_it_was_written() {
        let forward = quote(QuoteHead::Forward(vec![(ForwardField::Cc, "B".into())]));
        for quote in [reply(), forward] {
            let json = serde_json::to_string(&quote).unwrap();
            assert_eq!(
                serde_json::from_str::<Quote>(&json).unwrap(),
                quote,
                "{json}"
            );
        }
    }

    #[test]
    fn every_value_of_a_head_can_be_changed() {
        let mut head = reply().head;
        head.values_mut().for_each(|v| v.push('!'));
        assert_eq!(
            head,
            QuoteHead::Reply(Attribution {
                from: Some("A!".into()),
                date: Some("2026-11-02 09:30!".into()),
            })
        );
        let mut head = QuoteHead::Forward(vec![(ForwardField::To, "B".into())]);
        head.values_mut().for_each(|v| v.push('!'));
        assert_eq!(
            head,
            QuoteHead::Forward(vec![(ForwardField::To, "B!".into())])
        );
    }

    #[test]
    fn a_summary_says_what_is_sent() {
        let draft = Draft {
            id: DraftId(1),
            account_id: AccountId(1),
            identity: IdentityId(1),
            content: DraftContent {
                body: DraftBody {
                    text: "Thanks".into(),
                    html: Some("<p><b>Thanks</b></p>".into()),
                },
                quote: Some(Quote {
                    html: Some("<p>hi</p>".into()),
                    ..reply()
                }),
                ..DraftContent::default()
            },
            attachments: Vec::new(),
            reply_to: None,
            in_reply_to: None,
            references: Vec::new(),
            outbox: None,
            on_server: false,
            upload_error: None,
            updated_at: 0,
        };
        let summary = DraftSummary::from(&draft);
        assert!(summary.formatted);
        assert_eq!(summary.text_as_sent(), draft.content.text_as_sent());
    }

    #[test]
    fn the_quote_changes_only_when_given() {
        let mut content = DraftContent {
            quote: Some(reply()),
            ..DraftContent::default()
        };
        DraftChanges {
            body: Some(DraftBody::plain("new")),
            ..DraftChanges::default()
        }
        .apply(&mut content);
        assert!(content.quote.is_some());
        // An editor saves what it writes, which the quote isn't.
        DraftChanges::all(IdentityId(1), DraftContent::default()).apply(&mut content);
        assert!(content.quote.is_some());
        DraftChanges {
            quote: Some(None),
            ..DraftChanges::default()
        }
        .apply(&mut content);
        assert_eq!(content.quote, None);
    }
}
