//! What the system's notifications say of the new mail the engine announces
//! (`Event::NewMail`), and where clicking one goes: the arrivals gathered for a moment,
//! a notification for each of a few or one for them all, and the tag each carries, which
//! comes back with its click. The frontend shows them and follows the click. See
//! docs/architecture/background.md.

use std::time::Duration;

use catmail_core::{AccountId, MessageId, MessageSummary};

use crate::commands::one_account;
use crate::format;
use crate::mailbox::Destination;

/// How long announcements are gathered before telling: a sync stores new mail in
/// batches, and after a wake every account syncs at once, each announcing its own.
pub const GATHER: Duration = Duration::from_secs(1);

/// Up to this many arrivals get a notification each; more get one for them all.
const ONE_EACH: usize = 3;

/// What a notification is about, and so what clicking it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Message(AccountId, MessageId),
    /// Several messages, in the account's INBOX.
    Inbox(AccountId),
    /// Several messages in more than one account: the window as it is.
    Window,
}

const TAG: &str = "new-mail";

impl Target {
    /// Carried by the notification and back in its response. One for several messages
    /// replaces the one before where the system can.
    pub fn tag(self) -> String {
        match self {
            Target::Message(account, message) => format!("{TAG}/{account}/{message}"),
            Target::Inbox(account) => format!("{TAG}/{account}"),
            Target::Window => TAG.into(),
        }
    }

    /// Where clicking takes the main window; for several accounts, nowhere new.
    pub fn destination(self) -> Option<Destination> {
        match self {
            Target::Message(account, message) => Some(Destination::Inbox {
                account,
                message: Some(message),
            }),
            Target::Inbox(account) => Some(Destination::Inbox {
                account,
                message: None,
            }),
            Target::Window => None,
        }
    }

    /// The target of a notification's `tag`; `None` if it isn't one of ours.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let mut parts = tag.split('/');
        if parts.next() != Some(TAG) {
            return None;
        }
        let mut id = || parts.next().map(str::parse::<i64>).transpose().ok();
        let target = match (id()?, id()?) {
            (None, _) => Target::Window,
            (Some(account), None) => Target::Inbox(AccountId(account)),
            (Some(account), Some(message)) => {
                Target::Message(AccountId(account), MessageId(message))
            }
        };
        parts.next().is_none().then_some(target)
    }
}

/// One notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// [`Target::tag`].
    pub tag: String,
    /// Plain text.
    pub title: String,
    /// Markup on Linux ([`plain`]), plain text elsewhere.
    pub body: String,
}

/// What to show for `arrived`, newest first: a notification for each, the newest
/// posted last so it ends up on top, or one for them all.
pub fn notifications(arrived: &[MessageSummary]) -> Vec<Notice> {
    if arrived.len() <= ONE_EACH {
        return arrived
            .iter()
            .rev()
            .map(|m| Notice {
                tag: Target::Message(m.account_id, m.id).tag(),
                title: match format::sender(&m.from) {
                    "" => "(no sender)".into(),
                    sender => sender.to_owned(),
                },
                body: plain(format::subject(m.subject.as_deref())),
            })
            .collect();
    }
    let target = one_account(arrived).map_or(Target::Window, Target::Inbox);
    let mut senders: Vec<&str> = Vec::new();
    for sender in arrived.iter().map(|m| format::sender(&m.from)) {
        if !sender.is_empty() && !senders.contains(&sender) {
            senders.push(sender);
        }
    }
    vec![Notice {
        tag: target.tag(),
        title: format!("{} new messages", arrived.len()),
        body: plain(&senders.join(", ")),
    }]
}

/// `text` as a notification's body, which is markup on Linux where the notification
/// server takes it (the XDG `body-markup` capability, which most do): passed on as it
/// is, a subject's `<b>` would be bold, and its `<a href>` a link. The title is plain
/// text everywhere.
fn plain(text: &str) -> String {
    if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    } else {
        text.to_owned()
    }
}

/// The arrivals announced and not yet told about, gathered for [`GATHER`].
#[derive(Debug, Default)]
pub struct Arrivals {
    arrived: Vec<MessageId>,
    gathering: bool,
}

impl Arrivals {
    /// `messages` were announced. True if they are the first since the last telling:
    /// the frontend tells about them all [`GATHER`] from now ([`Arrivals::take`]).
    pub fn arrived(&mut self, messages: Vec<MessageId>) -> bool {
        self.arrived.extend(messages);
        !std::mem::replace(&mut self.gathering, true)
    }

    /// What was gathered, to tell about now; what arrives from now on is gathered anew.
    pub fn take(&mut self) -> Vec<MessageId> {
        self.gathering = false;
        std::mem::take(&mut self.arrived)
    }
}

#[cfg(test)]
mod tests {
    use catmail_core::{Address, Flags, FolderId};

    use super::*;

    fn message(account: i64, id: i64, from: &str, subject: &str) -> MessageSummary {
        MessageSummary {
            id: MessageId(id),
            account_id: AccountId(account),
            folder_id: FolderId(1),
            subject: Some(subject.into()),
            from: vec![Address {
                name: Some(from.into()),
                email: format!("{}@example.com", from.to_lowercase()),
            }],
            to: Vec::new(),
            cc: Vec::new(),
            date: Some(id),
            snippet: None,
            flags: Flags::empty(),
            has_attachments: false,
            labels: Vec::new(),
        }
    }

    #[test]
    fn a_few_arrivals_get_a_notification_each_newest_last() {
        let shown = notifications(&[message(1, 8, "Bob", "Lunch"), message(1, 7, "Carol", "")]);
        let shown: Vec<_> = shown
            .iter()
            .map(|n| (n.title.as_str(), n.body.as_str(), Target::from_tag(&n.tag)))
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "Carol",
                    "(no subject)",
                    Some(Target::Message(AccountId(1), MessageId(7)))
                ),
                (
                    "Bob",
                    "Lunch",
                    Some(Target::Message(AccountId(1), MessageId(8)))
                ),
            ]
        );
    }

    #[test]
    fn many_arrivals_get_one_notification_for_them_all() {
        let arrived = [
            message(1, 4, "Bob", "a"),
            message(1, 3, "Carol", "b"),
            message(1, 2, "Bob", "c"),
            message(1, 1, "Dave", "d"),
        ];
        let shown = notifications(&arrived);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].title, "4 new messages");
        assert_eq!(shown[0].body, "Bob, Carol, Dave");
        assert_eq!(
            Target::from_tag(&shown[0].tag),
            Some(Target::Inbox(AccountId(1)))
        );

        let mut two_accounts = arrived.clone();
        two_accounts[3].account_id = AccountId(2);
        let shown = notifications(&two_accounts);
        assert_eq!(Target::from_tag(&shown[0].tag), Some(Target::Window));
    }

    #[test]
    fn what_the_mail_says_shows_as_written() {
        let subject = "<b>Invoice</b> & <a href=\"https://example.com\">pay</a>";
        let shown = notifications(&[message(1, 1, "<i>Bob</i>", subject)]);
        let body = if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            "&lt;b&gt;Invoice&lt;/b&gt; &amp; &lt;a href=\"https://example.com\"&gt;pay&lt;/a&gt;"
        } else {
            subject
        };
        assert_eq!(shown[0].body, body);
        assert_eq!(shown[0].title, "<i>Bob</i>");
    }

    #[test]
    fn tags_that_arent_ours_are_ignored() {
        for tag in ["", "other", "new-mail/x", "new-mail/1/2/3", "new-mailbox/1"] {
            assert_eq!(Target::from_tag(tag), None, "{tag}");
        }
    }

    #[test]
    fn arrivals_are_told_about_together() {
        let mut arrivals = Arrivals::default();
        assert!(arrivals.arrived(vec![MessageId(1)]));
        assert!(!arrivals.arrived(vec![MessageId(2)]), "already gathering");
        assert_eq!(arrivals.take(), [MessageId(1), MessageId(2)]);
        assert!(arrivals.arrived(vec![MessageId(3)]), "gathering anew");
    }
}
