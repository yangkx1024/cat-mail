use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{AccountId, FolderId};

/// Well-known folder purpose, detected from IMAP SPECIAL-USE attributes or names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    All,
    Flagged,
}

impl FolderRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Sent => "sent",
            Self::Drafts => "drafts",
            Self::Trash => "trash",
            Self::Junk => "junk",
            Self::Archive => "archive",
            Self::All => "all",
            Self::Flagged => "flagged",
        }
    }
}

impl FromStr for FolderRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "inbox" => Self::Inbox,
            "sent" => Self::Sent,
            "drafts" => Self::Drafts,
            "trash" => Self::Trash,
            "junk" => Self::Junk,
            "archive" => Self::Archive,
            "all" => Self::All,
            "flagged" => Self::Flagged,
            other => return Err(format!("unknown folder role `{other}`")),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub account_id: AccountId,
    /// Full mailbox name for display, e.g. `INBOX` or `Archive/2026`; hierarchy levels
    /// are separated by `delimiter`.
    pub name: String,
    pub delimiter: Option<char>,
    pub role: Option<FolderRole>,
    /// False for hierarchy-only nodes (`\Noselect`) that cannot hold messages.
    pub selectable: bool,
    pub unread_count: u32,
    pub total_count: u32,
    /// Unix timestamp, milliseconds, of the last completed sync.
    pub last_synced_at: Option<i64>,
}
