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
    pub kind: QuoteKind,
    /// What introduces it, as plain text: who wrote it and when (`On …, … wrote:`), or a
    /// forward's header lines.
    pub intro: String,
    /// The message's text.
    pub text: String,
    /// The message's HTML as it was sanitized when stored (a style sheet in front, if
    /// any), its images inline attachments of the draft by `cid:` URL
    /// ([`DraftAttachment::content_id`]). `None` for a message of text only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteKind {
    /// Quoted: `> ` in the text, a `<blockquote type="cite">` in the HTML.
    Reply,
    /// As it was, below its headers.
    Forward,
}

/// The line a forward's intro starts with, its headers (`From: …`) after it.
pub const FORWARD_SEPARATOR: &str = "---------- Forwarded message ---------";

impl Quote {
    /// A forward's header lines (`From: …`, `Subject: …`): its intro after
    /// [`FORWARD_SEPARATOR`]. `None` for a reply, or a forward without them.
    pub fn forward_headers(&self) -> Option<&str> {
        if self.kind != QuoteKind::Forward {
            return None;
        }
        let intro = self.intro.trim();
        let headers = intro.strip_prefix(FORWARD_SEPARATOR).unwrap_or(intro);
        Some(headers.trim_start()).filter(|h| !h.is_empty())
    }

    /// `text`, then the quote, as the plain text of a message shows them.
    pub fn below(&self, text: &str) -> String {
        with_quote(text, &self.as_text())
    }

    /// The quote as plain text: its intro, then the message's text, `> `-quoted in a
    /// reply.
    pub fn as_text(&self) -> String {
        self.kind.as_text(&self.intro, &self.text)
    }
}

impl QuoteKind {
    /// A quote of this kind as plain text ([`Quote::as_text`]), from its intro and the
    /// message's text.
    pub fn as_text(self, intro: &str, text: &str) -> String {
        let mut out = intro.trim_end().to_owned();
        out.push('\n');
        let text = text.trim_end();
        match self {
            QuoteKind::Reply => {
                for line in text.lines() {
                    out.push('>');
                    if !line.is_empty() {
                        out.push(' ');
                        out.push_str(line);
                    }
                    out.push('\n');
                }
            }
            QuoteKind::Forward => {
                out.push('\n');
                if !text.is_empty() {
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
        out
    }

    pub fn as_str(self) -> &'static str {
        match self {
            QuoteKind::Reply => "reply",
            QuoteKind::Forward => "forward",
        }
    }
}

impl std::str::FromStr for QuoteKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "reply" => Ok(Self::Reply),
            "forward" => Ok(Self::Forward),
            other => Err(format!("unknown quote kind `{other}`")),
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

    fn quote(kind: QuoteKind) -> Quote {
        Quote {
            kind,
            intro: "On Monday, A wrote:".into(),
            text: "hi\n\n> earlier\n\n".into(),
            html: None,
        }
    }

    #[test]
    fn a_reply_quotes_the_text_below_the_body() {
        let mut content = DraftContent {
            body: DraftBody::plain("Thanks\n\n"),
            quote: Some(quote(QuoteKind::Reply)),
            ..DraftContent::default()
        };
        assert_eq!(
            content.text_as_sent(),
            "Thanks\n\nOn Monday, A wrote:\n> hi\n>\n> > earlier\n"
        );
        content.body = DraftBody::default();
        assert_eq!(
            content.text_as_sent(),
            "On Monday, A wrote:\n> hi\n>\n> > earlier\n"
        );
    }

    #[test]
    fn a_forwards_headers_follow_its_separator() {
        let mut quote = Quote {
            kind: QuoteKind::Forward,
            intro: format!("{FORWARD_SEPARATOR}\nFrom: A\nSubject: Hi\n"),
            text: String::new(),
            html: None,
        };
        assert_eq!(quote.forward_headers(), Some("From: A\nSubject: Hi"));
        quote.intro = FORWARD_SEPARATOR.into();
        assert_eq!(quote.forward_headers(), None);
        quote.intro = "From: A".into();
        assert_eq!(quote.forward_headers(), Some("From: A"));
        quote.kind = QuoteKind::Reply;
        assert_eq!(quote.forward_headers(), None);
    }

    #[test]
    fn a_forward_carries_the_text_as_it_was() {
        let content = DraftContent {
            body: DraftBody::plain("FYI"),
            quote: Some(Quote {
                intro: "---------- Forwarded message ---------\nFrom: A".into(),
                ..quote(QuoteKind::Forward)
            }),
            ..DraftContent::default()
        };
        assert_eq!(
            content.text_as_sent(),
            "FYI\n\n---------- Forwarded message ---------\nFrom: A\n\nhi\n\n> earlier\n"
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
                    ..quote(QuoteKind::Reply)
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
            quote: Some(quote(QuoteKind::Reply)),
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
