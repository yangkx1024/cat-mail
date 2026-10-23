//! Which engine events make the window read what again, and how often.

use std::collections::HashMap;

use catmail_core::{AccountId, Event};

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
            // Search results may come from the account or folder that went away.
            Event::AccountAdded { .. }
            | Event::AccountRemoved { .. }
            | Event::FoldersChanged { .. } => sidebar(matches!(shown, Some(Source::Search(_)))),
            // Read with the accounts, for Settings.
            Event::IdentitiesChanged { .. } => sidebar(false),
            // Unread and total counts may have changed too. A search looks in every
            // folder of every account.
            Event::MessagesChanged { folder, .. } => sidebar(
                matches!(shown, Some(Source::Search(_))) || shown == Some(&Source::Folder(folder)),
            ),
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

    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.running
    }
}

/// Which draft count to show. Every `DraftsChanged` counts its account's drafts again,
/// and a read of the sidebar counts them too: only a count started after the account's
/// latest `DraftsChanged` is shown, so an older one landing late can't put back what
/// the drafts were.
#[derive(Debug, Default, Clone)]
pub struct DraftCounts {
    /// `DraftsChanged` events seen, by account, plus one for each time events were missed.
    changes: HashMap<AccountId, u64>,
}

impl DraftCounts {
    /// `account`'s drafts changed: the change a count started now answers.
    pub fn changed(&mut self, account: AccountId) -> u64 {
        let change = self.changes.entry(account).or_default();
        *change += 1;
        *change
    }

    /// Events were missed, a `DraftsChanged` among them maybe: the counts on their way
    /// may be older than the read of the sidebar that follows. Call it before starting
    /// that read, or the read is taken for an old one too. Only accounts with a count
    /// started have one on its way.
    pub fn missed(&mut self) {
        for change in self.changes.values_mut() {
            *change += 1;
        }
    }

    /// Whether the count started for `change` is still the latest.
    pub fn is_latest(&self, account: AccountId, change: u64) -> bool {
        self.changes.get(&account).copied().unwrap_or_default() == change
    }

    /// Whether a read of the sidebar started when the counts were `then` has the latest
    /// count of `account`'s drafts.
    pub fn read_is_latest(&self, then: &Self, account: AccountId) -> bool {
        self.changes.get(&account) == then.changes.get(&account)
    }
}

#[cfg(test)]
mod tests {
    use catmail_core::FolderId;

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
            // The results may hold messages of an account or a folder that is gone.
            (Some(Event::AccountRemoved { account }), Some(search.clone()), Refresh::ALL),
            (Some(Event::FoldersChanged { account }), Some(search.clone()), Refresh::ALL),
            (Some(messages.clone()), Some(Source::Folder(folder)), Refresh::ALL),
            (Some(messages.clone()), Some(Source::Folder(elsewhere)), SIDEBAR),
            (Some(messages.clone()), Some(Source::Drafts(account)), SIDEBAR),
            (Some(messages.clone()), None, SIDEBAR),
            // Results come from every account.
            (Some(Event::MessagesChanged { account: other, folder: elsewhere }), Some(search.clone()), Refresh::ALL),
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

    #[test]
    fn only_a_count_started_after_the_latest_change_is_shown() {
        let (account, other) = (AccountId(1), AccountId(2));
        let mut counts = DraftCounts::default();
        let read = counts.clone();
        assert!(counts.read_is_latest(&read, account));
        let first = counts.changed(account);
        let second = counts.changed(account);
        assert!(!counts.is_latest(account, first));
        assert!(counts.is_latest(account, second));
        // A read started before the change doesn't count; another account's change
        // doesn't make it old.
        assert!(!counts.read_is_latest(&read, account));
        let read = counts.clone();
        counts.changed(other);
        assert!(counts.read_is_latest(&read, account));
        assert!(!counts.read_is_latest(&read, other));
    }

    #[test]
    fn missing_events_outdates_the_counts_on_their_way() {
        let account = AccountId(1);
        let mut counts = DraftCounts::default();
        let change = counts.changed(account);
        // Missed, then the sidebar is read again: it has the latest count, while the
        // count started before the events were missed lands late and is dropped.
        counts.missed();
        let read = counts.clone();
        assert!(counts.read_is_latest(&read, account));
        assert!(!counts.is_latest(account, change));
        // A change seen afterwards is counted as usual.
        let change = counts.changed(account);
        assert!(counts.is_latest(account, change));
        assert!(!counts.read_is_latest(&read, account));
    }
}
