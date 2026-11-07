//! The right pane of an outbox entry, without its widgets: what it is, what it says
//! happened to it, and the choices it leaves (which the engine allows:
//! `OutboxStatus::can_*`).

use catmail_core::{AccountId, Identity, MailClient, OutboxEntry, OutboxId, OutboxStatus};

use crate::commands::Confirm;
use crate::composer::senders;
use crate::format;
use crate::list::outbox_status;

/// The lines over the entry's state, as (name, value). From is the identity it goes out
/// as, named among `identities` (every account's) as a new message's From names it. It
/// is among them once they are read: an identity can't be removed while the outbox has
/// messages to send through it.
pub fn facts(
    entry: &OutboxEntry,
    identities: &[Identity],
    clock: format::Clock,
) -> [(&'static str, String); 3] {
    let from = senders(identities)
        .into_iter()
        .find(|s| s.identity == entry.identity)
        .map(|s| s.title)
        .unwrap_or_default();
    [
        ("To", entry.recipients.join(", ")),
        ("From", from),
        ("Queued", format::date(Some(entry.created_at), clock)),
    ]
}

/// What happened to the entry, as the pane says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// It may have arrived: the user has to say.
    Unknown {
        title: &'static str,
        text: &'static str,
    },
    /// The server turned it down, for `reason`.
    Rejected {
        title: &'static str,
        reason: String,
    },
    Sending,
    /// Where it is, and why it is there (the last error), if it says.
    Note {
        text: &'static str,
        reason: Option<String>,
    },
}

impl State {
    pub fn of(entry: &OutboxEntry) -> State {
        let reason = entry.last_error.clone().filter(|r| !r.is_empty());
        match entry.status {
            OutboxStatus::Unknown => State::Unknown {
                title: "Delivery not confirmed",
                text: "The connection dropped after the message went to the server, before it \
                       confirmed it, so it may have arrived. It won't be sent again on its own: \
                       check with a recipient, then choose.",
            },
            OutboxStatus::Failed => State::Rejected {
                title: "The server rejected this message",
                reason: reason.unwrap_or_default(),
            },
            OutboxStatus::Sending => State::Sending,
            OutboxStatus::Accepted => State::Note {
                text: "Sent. A copy is still to be saved to the Sent folder.",
                reason,
            },
            OutboxStatus::Queued | OutboxStatus::Done => State::Note {
                text: outbox_status(entry).0,
                reason,
            },
        }
    }
}

/// What the user can do about an entry, as its status allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    SendNow(AccountId),
    MarkSent(OutboxId),
    Retry(OutboxId),
    Cancel(OutboxId),
}

impl Choice {
    /// A name for the button, unique in the pane.
    pub fn id(self) -> &'static str {
        match self {
            Self::SendNow(_) => "send-now",
            Self::MarkSent(_) => "mark-sent",
            Self::Retry(_) => "retry",
            Self::Cancel(_) => "cancel",
        }
    }

    pub fn label(self, status: OutboxStatus) -> &'static str {
        match self {
            Self::SendNow(_) => "Send now",
            Self::MarkSent(_) => "Mark as sent",
            Self::Retry(_) if status.went_out() => "Save the copy now",
            Self::Retry(_) if status.retry_may_duplicate() => "Send again",
            Self::Retry(_) => "Try again",
            Self::Cancel(_) if status.went_out() => "Don't keep a copy",
            Self::Cancel(_) => "Back to drafts",
        }
    }

    /// What to ask first, and the button that goes on, if anything: sending again what
    /// may have arrived.
    pub fn confirm(self, status: OutboxStatus) -> Option<Confirm> {
        (matches!(self, Self::Retry(_)) && status.retry_may_duplicate()).then(|| Confirm {
            title: "Send it again?".into(),
            description: "If the first one did arrive, the recipients get it twice.".into(),
            action: "Send again",
        })
    }

    /// What it does, for "Couldn't …".
    pub fn what(self) -> &'static str {
        match self {
            Self::Retry(_) => "try again",
            Self::SendNow(_) => "send the message",
            Self::MarkSent(_) => "mark the message as sent",
            Self::Cancel(_) => "take the message out of the outbox",
        }
    }

    pub async fn run(self, client: &MailClient) -> catmail_core::Result<()> {
        match self {
            Self::Retry(id) => client.retry_outbox(id).await.map(drop),
            Self::MarkSent(id) => client.mark_outbox_sent(id).await.map(drop),
            Self::Cancel(id) => client.cancel_outbox(id).await.map(drop),
            Self::SendNow(account) => client.flush_outbox(account).await.map(drop),
        }
    }
}

/// The entry's choices, the likeliest first. A queued entry is sent with the rest of the
/// account's outbox; one the server rejected most likely needs editing.
pub fn choices(entry: &OutboxEntry) -> Vec<Choice> {
    let status = entry.status;
    let mut choices = Vec::new();
    if status.can_mark_sent() {
        choices.push(Choice::MarkSent(entry.id));
    }
    if status == OutboxStatus::Queued {
        choices.push(Choice::SendNow(entry.account_id));
    } else if status.can_retry() {
        choices.push(Choice::Retry(entry.id));
    }
    if status.can_cancel() {
        let cancel = Choice::Cancel(entry.id);
        match status {
            OutboxStatus::Failed => choices.insert(0, cancel),
            _ => choices.push(cancel),
        }
    }
    choices
}

#[cfg(test)]
mod tests {
    use catmail_core::{IdentityId, Security, ServerConfig};

    use super::*;
    use crate::fixtures;

    /// Account 2's, sent as identity 3.
    fn entry(status: OutboxStatus) -> OutboxEntry {
        OutboxEntry {
            identity: IdentityId(3),
            ..fixtures::outbox_entry(1, 2, status)
        }
    }

    fn identity(id: i64, name: Option<&str>, email: &str, host: &str) -> Identity {
        Identity {
            id: IdentityId(id),
            account_id: AccountId(2),
            email: email.into(),
            display_name: name.map(Into::into),
            username: email.into(),
            smtp: ServerConfig {
                host: host.into(),
                port: 465,
                security: Security::Tls,
            },
            is_default: id == 1,
            saves_sent: None,
            created_at: 0,
        }
    }

    #[test]
    fn from_is_the_identity_it_goes_out_as() {
        let from = |identities: &[Identity]| {
            facts(
                &entry(OutboxStatus::Queued),
                identities,
                format::Clock::TwentyFourHour,
            )[1]
            .1
            .clone()
        };
        let default = identity(1, None, "me@example.com", "smtp.example.com");
        let other = identity(3, Some("Me"), "me@work.example", "smtp.work.example");
        assert_eq!(from(&[default.clone(), other]), "Me <me@work.example>");
        // The same address through another server is told apart by it, as in the composer.
        let relay = identity(3, None, "me@example.com", "relay.example.net");
        assert_eq!(
            from(&[default, relay]),
            "me@example.com · relay.example.net"
        );
        // Not read yet.
        assert_eq!(from(&[]), "");
    }

    #[test]
    fn choices_follow_what_the_engine_allows() {
        let labels = |status| {
            choices(&entry(status))
                .into_iter()
                .map(|c| c.label(status))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(OutboxStatus::Unknown),
            ["Mark as sent", "Send again", "Back to drafts"]
        );
        assert_eq!(
            labels(OutboxStatus::Failed),
            ["Back to drafts", "Try again"]
        );
        assert_eq!(
            labels(OutboxStatus::Accepted),
            ["Save the copy now", "Don't keep a copy"]
        );
        assert_eq!(labels(OutboxStatus::Queued), ["Send now", "Back to drafts"]);
        assert!(labels(OutboxStatus::Sending).is_empty());
        assert!(labels(OutboxStatus::Done).is_empty());
    }

    #[test]
    fn only_sending_again_what_may_have_arrived_asks_first() {
        let retry = Choice::Retry(OutboxId(1));
        assert!(retry.confirm(OutboxStatus::Unknown).is_some());
        assert!(retry.confirm(OutboxStatus::Failed).is_none());
        assert!(retry.confirm(OutboxStatus::Accepted).is_none());
        assert!(
            Choice::Cancel(OutboxId(1))
                .confirm(OutboxStatus::Unknown)
                .is_none()
        );
    }

    #[test]
    fn the_state_says_why() {
        let mut failed = entry(OutboxStatus::Failed);
        failed.last_error = Some("550 no such user".into());
        assert!(matches!(
            State::of(&failed),
            State::Rejected { reason, .. } if reason == "550 no such user"
        ));
        let queued = entry(OutboxStatus::Queued);
        assert_eq!(
            State::of(&queued),
            State::Note {
                text: "Waiting to send",
                reason: None
            }
        );
    }
}
