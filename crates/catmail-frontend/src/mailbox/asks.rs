//! What the tray and the notifications ask of the main window's mailbox, which there may
//! be none of (running with no window): kept for the next to open, with where the last
//! one was.

use futures_channel::mpsc;

use super::{Destination, Place};

/// What the tray or a notification asks of the main window's mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// Go there, once the folders are read.
    GoTo(Destination),
    /// A new message, from the account shown.
    Compose,
}

/// The asks on their way to the main window's mailbox, or waiting for one. The app keeps
/// one for the process: windows come and go.
#[derive(Debug)]
pub struct Asks {
    /// The main window's mailbox, while it follows them.
    mailbox: Option<mpsc::UnboundedSender<Ask>>,
    /// Asked while there was none: the next takes them.
    waiting: Vec<Ask>,
    /// Where the last one was, for the next to open there.
    left_at: Option<Place>,
}

impl Default for Asks {
    fn default() -> Self {
        Self::new()
    }
}

impl Asks {
    pub const fn new() -> Self {
        Self {
            mailbox: None,
            waiting: Vec::new(),
            left_at: None,
        }
    }

    /// Hands `ask` to the mailbox, or keeps it for the next if there is none (or it has
    /// stopped following).
    pub fn ask(&mut self, ask: Ask) {
        let sent = self
            .mailbox
            .as_ref()
            .is_some_and(|mailbox| mailbox.unbounded_send(ask).is_ok());
        if !sent {
            self.mailbox = None;
            self.waiting.push(ask);
        }
    }

    /// For the main window's mailbox, as it opens: what was asked of it while there was
    /// none, starting with where the last one was (unless a notification asked for
    /// elsewhere), and what is asked from now on.
    pub fn follow(&mut self) -> (Vec<Ask>, mpsc::UnboundedReceiver<Ask>) {
        let (mailbox, asks) = mpsc::unbounded();
        self.mailbox = Some(mailbox);
        let mut waiting = std::mem::take(&mut self.waiting);
        if !waiting.iter().any(|ask| matches!(ask, Ask::GoTo(_)))
            && let Some(place) = self.left_at
        {
            waiting.insert(0, Ask::GoTo(Destination::Place(place)));
        }
        (waiting, asks)
    }

    /// The main window's mailbox shows `place`: the next opens there.
    pub fn left_at(&mut self, place: Option<Place>) {
        self.left_at = place;
    }

    /// The main window's mailbox is gone: what is asked from now on waits for the next.
    pub fn closed(&mut self) {
        self.mailbox = None;
    }
}

#[cfg(test)]
mod tests {
    use catmail_core::{AccountId, FolderId, MessageId};

    use super::*;

    const INBOX: Destination = Destination::Inbox {
        account: AccountId(1),
        message: Some(MessageId(7)),
    };

    #[test]
    fn asks_wait_for_a_mailbox_which_starts_where_the_last_was() {
        let mut asks = Asks::new();
        // None asked, none left: nothing to do.
        assert_eq!(asks.follow().0, []);
        asks.closed();

        asks.left_at(Some(Place::Folder(FolderId(3))));
        asks.ask(Ask::Compose);
        asks.ask(Ask::Compose);
        let (asked, mut later) = asks.follow();
        assert_eq!(
            asked,
            [
                Ask::GoTo(Destination::Place(Place::Folder(FolderId(3)))),
                Ask::Compose,
                Ask::Compose,
            ]
        );
        // Taken: the mailbox following gets the next as they come.
        asks.ask(Ask::GoTo(INBOX));
        assert_eq!(later.try_recv(), Ok(Ask::GoTo(INBOX)));
        assert!(later.try_recv().is_err(), "nothing more");

        // Where it was is forgotten with nothing shown.
        asks.left_at(None);
        asks.closed();
        asks.ask(Ask::Compose);
        assert_eq!(asks.follow().0, [Ask::Compose]);
    }

    #[test]
    fn where_a_notification_asks_to_go_comes_before_where_the_last_was() {
        let mut asks = Asks::new();
        asks.left_at(Some(Place::Inboxes));
        asks.ask(Ask::Compose);
        asks.ask(Ask::GoTo(INBOX));
        assert_eq!(asks.follow().0, [Ask::Compose, Ask::GoTo(INBOX)]);
    }

    #[test]
    fn asks_wait_once_the_mailbox_has_stopped_following() {
        let mut asks = Asks::new();
        let (_, later) = asks.follow();
        // Its view gone before the window said so.
        drop(later);
        asks.ask(Ask::Compose);
        let (asked, _later) = asks.follow();
        assert_eq!(asked, [Ask::Compose]);

        // Closed: the next mailbox takes what is asked meanwhile, not this one.
        asks.closed();
        asks.ask(Ask::GoTo(INBOX));
        assert_eq!(asks.follow().0, [Ask::GoTo(INBOX)]);
    }
}
