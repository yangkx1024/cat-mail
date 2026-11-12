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
    /// Plain text.
    pub text: String,
}

impl DraftContent {
    /// To, Cc and Bcc, in that order.
    pub fn recipients(&self) -> impl Iterator<Item = &Address> {
        self.to.iter().chain(&self.cc).chain(&self.bcc)
    }
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
    pub text: Option<String>,
}

impl DraftChanges {
    /// Everything replaced, as an editor saves what it shows.
    pub fn all(from: IdentityId, content: DraftContent) -> Self {
        let DraftContent {
            subject,
            to,
            cc,
            bcc,
            text,
        } = content;
        Self {
            from: Some(from),
            subject: Some(subject),
            to: Some(to),
            cc: Some(cc),
            bcc: Some(bcc),
            text: Some(text),
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
            text,
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
        replace(&mut content.text, text);
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
}

impl Forwarding {
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

/// A file attached to a draft. Its content was copied in when it was added, so the
/// original file may change or go away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftAttachment {
    pub id: DraftAttachmentId,
    pub filename: String,
    /// Lower-case `type/subtype`.
    pub mime_type: String,
    pub size: u64,
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
