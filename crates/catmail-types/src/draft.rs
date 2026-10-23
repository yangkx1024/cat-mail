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
