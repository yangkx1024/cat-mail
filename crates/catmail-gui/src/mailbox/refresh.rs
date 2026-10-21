//! Which engine events make the window read what again, and how often.

use catmail_core::Event;

use super::message_list::Source;

/// What the window reads again after an engine event.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Refresh {
    /// The accounts, their folders and counts, drafts and outbox.
    pub sidebar: bool,
    /// The rows the list shows.
    pub list: bool,
}

impl Refresh {
    pub const ALL: Self = Self {
        sidebar: true,
        list: true,
    };

    /// For `event`, while the list shows `shown`; `None` when events were missed, which
    /// could have changed anything.
    pub fn after(event: Option<&Event>, shown: Option<&Source>) -> Self {
        let Some(event) = event else {
            return Self::ALL;
        };
        let sidebar = |list| Self {
            sidebar: true,
            list,
        };
        match *event {
            Event::AccountAdded { .. }
            | Event::AccountRemoved { .. }
            | Event::FoldersChanged { .. } => sidebar(false),
            // Unread and total counts may have changed too. A search looks in every
            // folder of every account.
            Event::MessagesChanged { folder, .. } => sidebar(
                matches!(shown, Some(Source::Search(_))) || shown == Some(&Source::Folder(folder)),
            ),
            Event::DraftsChanged { account } => sidebar(shown == Some(&Source::Drafts(account))),
            Event::OutboxChanged { account } => sidebar(shown == Some(&Source::Outbox(account))),
            Event::AccountStatusChanged { .. }
            | Event::PendingOpsChanged { .. }
            | Event::MessageSent { .. } => Self::default(),
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

/// One read at a time. Asking while one runs asks for one more after it, so the last
/// read always starts after the last change; only replacing the GPUI task wouldn't do,
/// as the engine finishes a read it was handed anyway.
#[derive(Debug, Default)]
pub struct Coalesce {
    running: bool,
    again: bool,
}

impl Coalesce {
    /// Whether to start reading now; if not, another read follows the one running.
    pub fn start(&mut self) -> bool {
        if self.running {
            self.again = true;
            return false;
        }
        self.running = true;
        true
    }

    /// The read is done: whether to start another.
    pub fn finish(&mut self) -> bool {
        self.running = false;
        std::mem::take(&mut self.again)
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

    #[test]
    fn events_read_again_what_they_change() {
        let (account, other) = (AccountId(1), AccountId(2));
        let (folder, elsewhere) = (FolderId(1), FolderId(2));
        let messages = Event::MessagesChanged { account, folder };
        let search = Source::Search("lunch".into());
        #[rustfmt::skip]
        let cases = [
            (None, None, Refresh::ALL),
            (None, Some(Source::Folder(folder)), Refresh::ALL),
            (Some(Event::AccountAdded { account }), Some(Source::Folder(folder)), SIDEBAR),
            (Some(Event::AccountRemoved { account }), None, SIDEBAR),
            (Some(Event::FoldersChanged { account }), Some(Source::Folder(folder)), SIDEBAR),
            (Some(messages.clone()), Some(Source::Folder(folder)), Refresh::ALL),
            (Some(messages.clone()), Some(Source::Folder(elsewhere)), SIDEBAR),
            (Some(messages.clone()), Some(Source::Drafts(account)), SIDEBAR),
            (Some(messages.clone()), None, SIDEBAR),
            // Results come from every account.
            (Some(Event::MessagesChanged { account: other, folder: elsewhere }), Some(search.clone()), Refresh::ALL),
            (Some(Event::DraftsChanged { account }), Some(Source::Drafts(account)), Refresh::ALL),
            (Some(Event::DraftsChanged { account }), Some(Source::Drafts(other)), SIDEBAR),
            (Some(Event::DraftsChanged { account }), Some(search.clone()), SIDEBAR),
            (Some(Event::OutboxChanged { account }), Some(Source::Outbox(account)), Refresh::ALL),
            (Some(Event::OutboxChanged { account }), Some(Source::Outbox(other)), SIDEBAR),
            (Some(Event::AccountStatusChanged { account }), Some(Source::Folder(folder)), NOTHING),
            (Some(Event::PendingOpsChanged { account }), Some(search), NOTHING),
        ];
        for (event, shown, expected) in cases {
            assert_eq!(
                Refresh::after(event.as_ref(), shown.as_ref()),
                expected,
                "{event:?} {shown:?}"
            );
        }
    }

    #[test]
    fn asking_during_a_read_reads_once_more() {
        let mut reads = Coalesce::default();
        assert!(reads.start());
        assert!(!reads.start());
        assert!(!reads.start());
        assert!(reads.finish());
        assert!(reads.start());
        assert!(!reads.finish());
        assert!(reads.start());
    }

    #[test]
    fn a_burst_is_read_at_once_then_once_per_interval() {
        let list = Refresh {
            sidebar: false,
            list: true,
        };
        let mut throttle = Throttle::default();
        // Nothing to read starts no interval.
        assert_eq!(throttle.ask(NOTHING), NOTHING);
        assert_eq!(throttle.ask(SIDEBAR), SIDEBAR);
        assert_eq!(throttle.ask(SIDEBAR), NOTHING);
        assert_eq!(throttle.ask(list), NOTHING);
        assert_eq!(throttle.ask(NOTHING), NOTHING);
        // Everything held back, together.
        assert_eq!(throttle.elapsed(), Refresh::ALL);
        assert_eq!(throttle.ask(SIDEBAR), NOTHING);
        assert_eq!(throttle.elapsed(), SIDEBAR);
        // A quiet interval ends the waiting: the next change is read at once.
        assert_eq!(throttle.elapsed(), NOTHING);
        assert_eq!(throttle.ask(list), list);
        assert_eq!(throttle.elapsed(), NOTHING);
        assert_eq!(throttle.ask(SIDEBAR), SIDEBAR);
    }
}
