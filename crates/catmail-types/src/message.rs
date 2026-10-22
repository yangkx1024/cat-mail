use std::fmt;
use std::ops::{BitOr, BitOrAssign};

use serde::{Deserialize, Serialize};

use crate::{AccountId, AttachmentId, FolderId, MessageId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

/// `Name <a@example.com>`, or just the address. For showing, not for headers: the name
/// is not quoted.
impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{name} <{}>", self.email),
            None => f.write_str(&self.email),
        }
    }
}

/// IMAP system flags as a bitset; stored as an INTEGER column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Flags(u32);

impl Flags {
    pub const SEEN: Self = Self(1 << 0);
    pub const FLAGGED: Self = Self(1 << 1);
    pub const ANSWERED: Self = Self(1 << 2);
    pub const DRAFT: Self = Self(1 << 3);
    pub const DELETED: Self = Self(1 << 4);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The flags in both.
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// The flags in `self` but not in `other`.
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// These flags with `add` set and then `remove` cleared.
    pub const fn apply(self, add: Self, remove: Self) -> Self {
        Self((self.0 | add.0) & !remove.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl BitOr for Flags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Flags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// The server's current flags for a message in one mailbox.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagState {
    pub uid: u32,
    pub flags: Flags,
    /// Non-system flags such as `$Junk` or `$Forwarded`.
    pub keywords: Vec<String>,
    /// MODSEQ, with CONDSTORE.
    pub modseq: Option<u64>,
}

/// What a message list row needs; bodies and attachments are loaded separately.
/// Describes the message as it is in `folder_id`; `flags` are that copy's flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageSummary {
    pub id: MessageId,
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub subject: Option<String>,
    pub from: Vec<Address>,
    /// Unix timestamp, seconds.
    pub date: Option<i64>,
    pub snippet: Option<String>,
    pub flags: Flags,
    pub has_attachments: bool,
}

impl MessageSummary {
    /// Where a page ending with this message ends: the next page is what comes after it.
    pub fn cursor(&self) -> MessageCursor {
        MessageCursor {
            date: self.date,
            id: self.id,
        }
    }
}

/// A place in a message list or search results, which are newest first, ties (equal or
/// missing dates) by id; messages without a date come last. Unlike a count of rows to
/// skip, it stays put when messages above it come or go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCursor {
    pub date: Option<i64>,
    pub id: MessageId,
}

/// A message for reading: the list summary plus the full headers, and the body once it
/// has been downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    #[serde(flatten)]
    pub summary: MessageSummary,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Where replies should go instead of the sender, e.g. a mailing list.
    pub reply_to: Vec<Address>,
    /// The Message-ID header, without angle brackets.
    pub message_id: Option<String>,
    /// The References header: Message-IDs of the thread so far, oldest first.
    pub references: Vec<String>,
    /// RFC822.SIZE: the raw message in octets.
    pub size: Option<u32>,
    /// `None` until downloaded.
    pub body: Option<MessageBody>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBody {
    /// Plain text; converted from the HTML part if the message has no text part.
    pub text: Option<String>,
    /// Sanitized HTML, if the message has an HTML part. Scripts, forms and event
    /// handlers are gone; image URLs (`cid:`, remote) are left for the renderer to
    /// resolve or block.
    pub html: Option<String>,
    /// Attachments and inline parts (images referenced by `cid:`), in MIME order.
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: AttachmentId,
    pub message_id: MessageId,
    pub filename: Option<String>,
    /// Lower-case `type/subtype`.
    pub mime_type: String,
    /// Decoded size in bytes; estimated from the encoded size until downloaded.
    pub size: Option<u32>,
    /// Without angle brackets; what `cid:` URLs in the HTML refer to.
    pub content_id: Option<String>,
    /// Shown within the body rather than offered as a file.
    pub inline: bool,
    /// Whether the content is stored locally. Large messages are downloaded without
    /// their attachments, which are fetched when first opened.
    pub downloaded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_ops() {
        let mut f = Flags::SEEN | Flags::FLAGGED;
        assert!(f.contains(Flags::SEEN));
        assert!(!f.contains(Flags::DRAFT));
        f.remove(Flags::SEEN);
        assert_eq!(f, Flags::FLAGGED);
        f.insert(Flags::DELETED);
        assert_eq!(Flags::from_bits(f.bits()), f);
        assert_eq!(f.difference(Flags::FLAGGED), Flags::DELETED);
        assert_eq!(f.intersection(Flags::FLAGGED | Flags::SEEN), Flags::FLAGGED);
        assert!(Flags::empty().is_empty() && !f.is_empty());
        assert_eq!(
            f.apply(Flags::SEEN, Flags::FLAGGED),
            Flags::SEEN | Flags::DELETED
        );
    }
}
