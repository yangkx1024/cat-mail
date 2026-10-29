//! The system's notifications for new mail, which the engine announces
//! (`Event::NewMail`), while the main window isn't in front; clicking one shows the
//! window on the message. Lives with the app, not a window: it tells about mail that
//! arrives while none is open. See docs/architecture/background.md.

use std::time::Duration;

use catmail_core::{AccountId, Event, MailClient, MessageId, MessageSummary, RecvError};
use gpui_kit::{App, Global, SharedString, SystemNotification, Task};

use crate::format;
use crate::mailbox::Destination;
use crate::resident::Resident;
use crate::settings::Preferences;

/// What the system knows the app by: on Windows its AppUserModelID; elsewhere only the
/// name shows. The qualifier of the data and config directories (`Config`).
pub const APP_ID: &str = "dev.CatMail.CatMail";
pub const APP_NAME: &str = "cat-mail";

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
    /// replaces the one before where the system can (not on Linux).
    fn tag(self) -> SharedString {
        match self {
            Target::Message(account, message) => format!("{TAG}/{account}/{message}").into(),
            Target::Inbox(account) => format!("{TAG}/{account}").into(),
            Target::Window => TAG.into(),
        }
    }

    /// Where clicking takes the main window; for several accounts, nowhere new.
    fn destination(self) -> Option<Destination> {
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

    fn from_tag(tag: &str) -> Option<Self> {
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

/// What to show for `arrived`, newest first: a notification for each, the newest
/// posted last so it ends up on top, or one for them all.
fn notifications(arrived: &[MessageSummary]) -> Vec<SystemNotification> {
    if arrived.len() <= ONE_EACH {
        return arrived
            .iter()
            .rev()
            .map(|m| SystemNotification {
                tag: Target::Message(m.account_id, m.id).tag(),
                title: match format::sender(&m.from) {
                    "" => "(no sender)".into(),
                    sender => sender.to_owned().into(),
                },
                body: plain(format::subject(m.subject.as_deref())),
                actions: Vec::new(),
            })
            .collect();
    }
    let account = arrived[0].account_id;
    let target = if arrived.iter().all(|m| m.account_id == account) {
        Target::Inbox(account)
    } else {
        Target::Window
    };
    let mut senders: Vec<&str> = Vec::new();
    for sender in arrived.iter().map(|m| format::sender(&m.from)) {
        if !sender.is_empty() && !senders.contains(&sender) {
            senders.push(sender);
        }
    }
    vec![SystemNotification {
        tag: target.tag(),
        title: format!("{} new messages", arrived.len()).into(),
        body: plain(&senders.join(", ")),
        actions: Vec::new(),
    }]
}

/// `text` as a notification's body, which is markup on Linux where the notification
/// server takes it (the XDG `body-markup` capability, which most do): GPUI passes it on
/// as it is, so a subject's `<b>` would be bold, and its `<a href>` a link. The title
/// is plain text everywhere.
fn plain(text: &str) -> SharedString {
    if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .into()
    } else {
        text.to_owned().into()
    }
}

pub struct Notifier {
    client: MailClient,
    /// Announced and not yet told about.
    arrived: Vec<MessageId>,
    /// Waiting out [`GATHER`] since something arrived, to tell then. The task isn't
    /// kept: it would be dropped, and so cancelled, as it takes what arrived.
    gathering: bool,
    _events: Task<()>,
}

impl Global for Notifier {}

impl Notifier {
    /// Tells about the new mail `client` announces from now on.
    pub fn start(client: MailClient, cx: &mut App) {
        let mut events = client.subscribe();
        let events = cx.spawn(async move |cx| {
            loop {
                match events.recv().await {
                    Ok(Event::NewMail { messages, .. }) => {
                        cx.update(|cx| Self::arrived(messages, cx));
                    }
                    Ok(_) => {}
                    // Missed announcements can't be had again; the mail is in the list.
                    Err(RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "missed events, maybe of new mail");
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
        cx.set_global(Self {
            client,
            arrived: Vec::new(),
            gathering: false,
            _events: events,
        });
        cx.on_system_notification_response(|response, cx| {
            if let Some(target) = Target::from_tag(&response.tag) {
                Resident::show_at(target.destination(), cx);
            }
        });
    }

    fn arrived(messages: Vec<MessageId>, cx: &mut App) {
        let this = cx.global_mut::<Self>();
        this.arrived.extend(messages);
        if std::mem::replace(&mut this.gathering, true) {
            return;
        }
        cx.spawn(async move |cx| {
            cx.background_executor().timer(GATHER).await;
            let told = cx.update(|cx| {
                // Looking at the window, the user sees it arrive.
                let tell = Preferences::get(cx).notify_new_mail && !Resident::main_is_active(cx);
                let this = cx.global_mut::<Self>();
                this.gathering = false;
                let arrived = std::mem::take(&mut this.arrived);
                tell.then(|| (this.client.clone(), arrived))
            });
            let Some((client, arrived)) = told else {
                return;
            };
            // Read or moved meanwhile, here or elsewhere: not new any more.
            let still_new = match client.unread_in_inbox(arrived).await {
                Ok(still_new) => still_new,
                Err(e) => return tracing::warn!(error = %e, "couldn't read new mail"),
            };
            cx.update(|cx| {
                for notification in notifications(&still_new) {
                    cx.show_system_notification(notification);
                }
            });
        })
        .detach();
    }

    /// Whether announcements are being gathered.
    #[cfg(test)]
    pub fn is_gathering(cx: &App) -> bool {
        cx.global::<Self>().gathering
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
            .map(|n| (n.title.as_ref(), n.body.as_ref(), Target::from_tag(&n.tag)))
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
        assert_eq!(shown[0].title.as_ref(), "4 new messages");
        assert_eq!(shown[0].body.as_ref(), "Bob, Carol, Dave");
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
        assert_eq!(shown[0].body.as_ref(), body);
        assert_eq!(shown[0].title.as_ref(), "<i>Bob</i>");
    }

    #[test]
    fn tags_that_arent_ours_are_ignored() {
        for tag in ["", "other", "new-mail/x", "new-mail/1/2/3", "new-mailbox/1"] {
            assert_eq!(Target::from_tag(tag), None, "{tag}");
        }
    }
}
