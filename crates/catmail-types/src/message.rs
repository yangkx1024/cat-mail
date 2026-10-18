use std::ops::{BitOr, BitOrAssign};

use serde::{Deserialize, Serialize};

use crate::{AccountId, FolderId, MessageId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
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

/// What a message list row needs; bodies and attachments are loaded separately.
/// Describes the message as it is in `folder_id`; `flags` are that copy's flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageSummary {
    pub id: MessageId,
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub thread_id: Option<i64>,
    pub subject: Option<String>,
    pub from: Vec<Address>,
    /// Unix timestamp, seconds.
    pub date: Option<i64>,
    pub snippet: Option<String>,
    pub flags: Flags,
    pub has_attachments: bool,
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
    }
}
