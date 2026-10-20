use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{AccountId, Flags, FolderId, MessageId, PendingOpId};

/// What a queued change does on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpKind {
    /// Sets and clears flags; only the ones that changed locally.
    Flags {
        add: Flags,
        remove: Flags,
    },
    Move {
        to: FolderId,
    },
    /// Deletes the message for good (`\Deleted` and expunge).
    Delete,
}

/// Where a queued change is. Applied changes are gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpStatus {
    /// Waiting to be made on the server, at the next sync or flush.
    Pending,
    /// The server refused it (`last_error` says why); the local change was undone.
    Failed,
    /// The folder was rebuilt on the server (new UIDVALIDITY) or deleted before the
    /// change was made, so it no longer applies; the folder was refetched instead.
    Conflict,
}

impl OpStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Failed => "failed",
            Self::Conflict => "conflict",
        }
    }
}

impl fmt::Display for OpStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OpStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "pending" => Self::Pending,
            "failed" => Self::Failed,
            "conflict" => Self::Conflict,
            other => return Err(format!("unknown operation status `{other}`")),
        })
    }
}

/// A change made locally to one message (marked read, moved, deleted) that is still to
/// be made on the server, or that couldn't be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOp {
    pub id: PendingOpId,
    pub account_id: AccountId,
    /// Where the message is on the server.
    pub folder_id: FolderId,
    /// The local message showing the change, while there is one.
    pub message_id: Option<MessageId>,
    /// That message's subject.
    pub subject: Option<String>,
    #[serde(flatten)]
    pub kind: OpKind,
    pub status: OpStatus,
    /// Tries that ended in a lost connection.
    pub attempts: u32,
    pub last_error: Option<String>,
    /// Unix timestamp, seconds.
    pub created_at: i64,
}

/// What replaying an account's queued changes did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayReport {
    /// Made on the server.
    pub applied: u32,
    /// Refused by the server, and undone locally.
    pub failed: u32,
    /// Still pending: their folder couldn't be opened, or was rebuilt on the server and
    /// needs a sync first.
    pub pending: u32,
    /// Drafts saved to the server's Drafts folder.
    pub drafts_uploaded: u32,
    /// Drafts the server refused to store; [`Draft::upload_error`](crate::Draft::upload_error) says why.
    pub drafts_failed: u32,
}
