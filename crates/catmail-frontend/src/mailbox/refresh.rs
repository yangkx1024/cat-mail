//! Which engine events make the mailbox read what again, and how often.

use catmail_core::{Event, FolderId};

use crate::mailbox::Source;

/// What the mailbox reads again after an engine event.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Refresh {
    /// The accounts, their folders and counts, drafts and outbox.
    pub sidebar: bool,
    pub list: bool,
}

impl Refresh {
    pub const ALL: Self = Self {
        sidebar: true,
        list: true,
    };

    /// For `event`, while the list shows `shown`, the messages of `folders` (unless it
    /// shows search results); `None` when events were missed, which could have changed
    /// anything.
    pub fn after(event: Option<&Event>, shown: Option<&Source>, folders: &[FolderId]) -> Self {
        let Some(event) = event else {
            return Self::ALL;
        };
        let sidebar = |list| Self {
            sidebar: true,
            list,
        };
        // Search results come from every folder of every account.
        let search = matches!(shown, Some(Source::Search(_)));
        match *event {
            // Search results may come from the account or folder that went away; All
            // Inboxes may have an INBOX more or less.
            Event::AccountAdded { .. }
            | Event::AccountRemoved { .. }
            | Event::FoldersChanged { .. } => {
                sidebar(search || shown == Some(&Source::Inboxes))
            }
            // Read with the accounts, for Settings.
            Event::AccountChanged { .. } => sidebar(false),
            // Unread and total counts may have changed too.
            Event::MessagesChanged { folder, .. } => sidebar(search || folders.contains(&folder)),
            // Only the count, which is read on its own (`Mailbox::count_drafts`): every pause
            // in typing saves a draft.
            Event::DraftsChanged { account } => Self {
                sidebar: false,
                list: shown == Some(&Source::Drafts(account)),
            },
            Event::OutboxChanged { account } => sidebar(shown == Some(&Source::Outbox(account))),
            Event::AccountStatusChanged { .. }
            | Event::PendingOpsChanged { .. }
            | Event::ChangesRefused { .. }
            | Event::MessageSent { .. }
            // Comes with `MessagesChanged`.
            | Event::NewMail { .. } => Self::default(),
        }
    }

    pub fn is_empty(self) -> bool {
        self == Self::default()
    }

    fn and(self, other: Self) -> Self {
        Self {
            sidebar: self.sidebar || other.sidebar,
            list: self.list || other.list,
        }
    }
}

/// Holds back what events ask to read again within an interval of the last read, and
/// reads it together when the interval is over: a sync's burst of events costs a read
/// per interval, while a lone change still shows at once.
#[derive(Debug, Default)]
pub struct Throttle {
    /// Asked for since the last read.
    held: Refresh,
    /// The last read was less than an interval ago.
    waiting: bool,
}

impl Throttle {
    /// What to read now. When it isn't empty, an interval starts; [`elapsed`] ends it.
    ///
    /// [`elapsed`]: Self::elapsed
    pub fn ask(&mut self, refresh: Refresh) -> Refresh {
        if self.waiting {
            self.held = self.held.and(refresh);
            return Refresh::default();
        }
        self.waiting = !refresh.is_empty();
        refresh
    }

    /// The interval is over: what was held back, to read now, which starts another
    /// interval unless it is empty.
    pub fn elapsed(&mut self) -> Refresh {
        let held = std::mem::take(&mut self.held);
        self.waiting = !held.is_empty();
        held
    }
}

#[cfg(test)]
mod tests {
    use catmail_core::{AccountId, FolderId};

    use super::*;

    const NOTHING: Refresh = Refresh {
        sidebar: false,
        list: false,
    };
    const SIDEBAR: Refresh = Refresh {
        sidebar: true,
        list: false,
    };
    const LIST: Refresh = Refresh {
        sidebar: false,
        list: true,
    };

    #[test]
    fn events_read_again_what_they_change() {
        let (account, other) = (AccountId(1), AccountId(2));
        let (folder, elsewhere, inbox) = (FolderId(1), FolderId(2), FolderId(3));
        let messages = Event::MessagesChanged { account, folder };
        let search = Source::Search("lunch".into());
        #[rustfmt::skip]
        let cases = [
            (None, None, Refresh::ALL),
            (None, Some(Source::Folder(folder)), Refresh::ALL),
            (Some(Event::AccountAdded { account }), Some(Source::Folder(folder)), SIDEBAR),
            (Some(Event::AccountRemoved { account }), None, SIDEBAR),
            (Some(Event::FoldersChanged { account }), Some(Source::Folder(folder)), SIDEBAR),
            // The results may hold messages of an account or a folder that is gone.
            (Some(Event::AccountRemoved { account }), Some(search.clone()), Refresh::ALL),
            (Some(Event::FoldersChanged { account }), Some(search.clone()), Refresh::ALL),
            (Some(messages.clone()), Some(Source::Folder(folder)), Refresh::ALL),
            (Some(messages.clone()), Some(Source::Folder(elsewhere)), SIDEBAR),
            (Some(messages.clone()), Some(Source::Drafts(account)), SIDEBAR),
            (Some(messages.clone()), None, SIDEBAR),
            // Results come from every account.
            (Some(Event::MessagesChanged { account: other, folder: elsewhere }), Some(search.clone()), Refresh::ALL),
            // All Inboxes, from each INBOX; which they are may change with the accounts.
            (Some(Event::MessagesChanged { account: other, folder: inbox }), Some(Source::Inboxes), Refresh::ALL),
            (Some(Event::MessagesChanged { account: other, folder: elsewhere }), Some(Source::Inboxes), SIDEBAR),
            (Some(Event::AccountAdded { account: other }), Some(Source::Inboxes), Refresh::ALL),
            (Some(Event::FoldersChanged { account }), Some(Source::Inboxes), Refresh::ALL),
            (Some(Event::DraftsChanged { account }), Some(Source::Inboxes), NOTHING),
            // The draft count is read on its own.
            (Some(Event::DraftsChanged { account }), Some(Source::Drafts(account)), LIST),
            (Some(Event::DraftsChanged { account }), Some(Source::Drafts(other)), NOTHING),
            (Some(Event::DraftsChanged { account }), Some(search.clone()), NOTHING),
            (Some(Event::OutboxChanged { account }), Some(Source::Outbox(account)), Refresh::ALL),
            (Some(Event::OutboxChanged { account }), Some(Source::Outbox(other)), SIDEBAR),
            (Some(Event::AccountStatusChanged { account }), Some(Source::Folder(folder)), NOTHING),
            (Some(Event::PendingOpsChanged { account }), Some(search.clone()), NOTHING),
            (Some(Event::ChangesRefused { account }), Some(search), NOTHING),
        ];
        for (event, shown, expected) in cases {
            let folders = match shown {
                Some(Source::Folder(id)) => vec![id],
                Some(Source::Inboxes) => vec![folder, inbox],
                _ => Vec::new(),
            };
            assert_eq!(
                Refresh::after(event.as_ref(), shown.as_ref(), &folders),
                expected,
                "{event:?} {shown:?}"
            );
        }
    }

    #[test]
    fn a_burst_is_read_at_once_then_once_per_interval() {
        let mut throttle = Throttle::default();
        // Nothing to read starts no interval.
        assert_eq!(throttle.ask(NOTHING), NOTHING);
        assert_eq!(throttle.ask(SIDEBAR), SIDEBAR);
        assert_eq!(throttle.ask(SIDEBAR), NOTHING);
        assert_eq!(throttle.ask(LIST), NOTHING);
        assert_eq!(throttle.ask(NOTHING), NOTHING);
        // Everything held back, together.
        assert_eq!(throttle.elapsed(), Refresh::ALL);
        assert_eq!(throttle.ask(SIDEBAR), NOTHING);
        assert_eq!(throttle.elapsed(), SIDEBAR);
        // A quiet interval ends the waiting: the next change is read at once.
        assert_eq!(throttle.elapsed(), NOTHING);
        assert_eq!(throttle.ask(LIST), LIST);
        assert_eq!(throttle.elapsed(), NOTHING);
        assert_eq!(throttle.ask(SIDEBAR), SIDEBAR);
    }
}
