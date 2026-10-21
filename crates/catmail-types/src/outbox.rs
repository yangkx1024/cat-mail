use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{AccountId, DraftId, OutboxId};

/// Where a message handed to "send" is. See [`OutboxEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutboxStatus {
    /// Waiting for an attempt; retried with backoff after temporary failures.
    Queued,
    /// The SMTP transaction is running.
    Sending,
    /// The server accepted the message; saving a copy to the Sent folder is pending.
    /// Only that is retried from here, never the send itself.
    Accepted,
    /// The connection was lost after the whole message was handed over, before the
    /// server confirmed it. It may or may not have been delivered, so it is never
    /// resent automatically: retry, mark it sent, or cancel.
    Unknown,
    /// Sent, and saved to Sent where there is one. The entry and its draft are gone.
    Done,
    /// The server refused the message for good (`last_error` says why). Retry once
    /// the cause is fixed, or cancel to edit the draft again.
    Failed,
}

impl OutboxStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sending => "sending",
            Self::Accepted => "accepted",
            Self::Unknown => "unknown",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// The message went out; at most saving it to Sent is left.
    pub fn went_out(self) -> bool {
        matches!(self, Self::Accepted | Self::Done)
    }

    /// Nothing more happens on its own: the user has to choose what to do.
    pub fn needs_user(self) -> bool {
        matches!(self, Self::Unknown | Self::Failed)
    }

    /// What `MailClient::retry_outbox` makes of an entry in this state, if it applies:
    /// another attempt now, of the send if it never went out (or may not have), of saving
    /// to Sent if it did.
    pub fn retry_to(self) -> Option<Self> {
        match self {
            Self::Failed | Self::Unknown => Some(Self::Queued),
            Self::Queued | Self::Accepted => Some(self),
            Self::Sending | Self::Done => None,
        }
    }

    /// Whether [`retry_to`](Self::retry_to) applies.
    pub fn can_retry(self) -> bool {
        self.retry_to().is_some()
    }

    /// Whether retrying may deliver the message twice: the first attempt may have
    /// arrived. Worth asking the user first.
    pub fn retry_may_duplicate(self) -> bool {
        self == Self::Unknown
    }

    /// Whether `MailClient::mark_outbox_sent` applies: the user found it was delivered.
    pub fn can_mark_sent(self) -> bool {
        self == Self::Unknown
    }

    /// Whether `MailClient::cancel_outbox` applies: one that never went out becomes a draft
    /// again; for one that did ([`went_out`](Self::went_out)), this gives up saving it
    /// to Sent.
    pub fn can_cancel(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Failed | Self::Unknown | Self::Accepted
        )
    }
}

impl fmt::Display for OutboxStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OutboxStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "queued" => Self::Queued,
            "sending" => Self::Sending,
            "accepted" => Self::Accepted,
            "unknown" => Self::Unknown,
            "done" => Self::Done,
            "failed" => Self::Failed,
            other => return Err(format!("unknown outbox status `{other}`")),
        })
    }
}

/// A message handed to "send": a snapshot of its draft as it was then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub id: OutboxId,
    pub account_id: AccountId,
    /// Returns to the drafts when the send is cancelled before it went out.
    pub draft_id: Option<DraftId>,
    pub subject: String,
    /// Every envelope recipient, Bcc included.
    pub recipients: Vec<String>,
    /// Without angle brackets.
    pub message_id: String,
    pub status: OutboxStatus,
    /// Attempts at the current step: the send while queued, saving to Sent once
    /// accepted.
    pub attempts: u32,
    /// Unix timestamp, seconds, of the next automatic attempt.
    pub next_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    /// Unix timestamp, seconds.
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrying_sends_again_unless_it_went_out() {
        use OutboxStatus::*;
        assert_eq!(Failed.retry_to(), Some(Queued));
        assert_eq!(Unknown.retry_to(), Some(Queued));
        assert_eq!(Queued.retry_to(), Some(Queued));
        // Sent already: only saving to Sent is tried again.
        assert_eq!(Accepted.retry_to(), Some(Accepted));
        assert_eq!(Sending.retry_to(), None);
        assert_eq!(Done.retry_to(), None);
    }
}
