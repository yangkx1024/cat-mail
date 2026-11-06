//! The commands on messages, the same wherever they show (over the message open, by the
//! list's heading for the messages checked, on a row while hovered): what they do to the
//! messages, what they are called, which changes ask first, what to say when one fails
//! and when the server refuses one.

use std::collections::HashSet;

use catmail_core::{
    AccountId, DeleteMode, DeleteOutcome, Flags, FolderId, MessageId, MessageSummary, OpKind,
    OpStatus, PendingOp,
};

use crate::format;
use crate::mailbox::AccountEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageCommand {
    Archive,
    Delete,
    Move,
    ToggleRead,
    ToggleFlag,
}

impl MessageCommand {
    /// Those about where the messages are, in a toolbar's order.
    pub const PLACE: [MessageCommand; 3] = [Self::Archive, Self::Delete, Self::Move];
    /// Those about their marks.
    pub const MARKS: [MessageCommand; 2] = [Self::ToggleRead, Self::ToggleFlag];

    /// Its name, for messages that all have `flags`: a toggle says what it does to them.
    pub fn label(self, flags: Flags) -> &'static str {
        match self {
            Self::Archive => "Archive",
            Self::Delete => "Delete",
            Self::Move => "Move to",
            Self::ToggleRead if flags.contains(Flags::SEEN) => "Mark as unread",
            Self::ToggleRead => "Mark as read",
            Self::ToggleFlag if flags.contains(Flags::FLAGGED) => "Remove flag",
            Self::ToggleFlag => "Flag",
        }
    }
}

/// The flags the messages all have: what a toggle takes off them all; one any of them
/// lacks it sets on all.
pub fn common_flags<'a>(messages: impl IntoIterator<Item = &'a MessageSummary>) -> Flags {
    messages
        .into_iter()
        .map(|m| m.flags)
        .reduce(Flags::intersection)
        .unwrap_or_default()
}

/// The account the messages are all of: "Move to" lists its folders. None if there are
/// none, or they are of more than one.
pub fn one_account<'a>(
    messages: impl IntoIterator<Item = &'a MessageSummary>,
) -> Option<AccountId> {
    let mut messages = messages.into_iter();
    let account = messages.next()?.account_id;
    messages.all(|m| m.account_id == account).then_some(account)
}

/// What the commands do to some messages, which their buttons show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applies {
    /// The flags they all have: the toggles take those off.
    flags: Flags,
    /// Archive changes one of them at least.
    archive: bool,
    /// They are of one account, whose folders "Move to" lists.
    move_to: bool,
}

impl Applies {
    pub fn of<'a, I>(messages: I, accounts: &[AccountEntry]) -> Self
    where
        I: IntoIterator<Item = &'a MessageSummary>,
        I::IntoIter: Clone,
    {
        let messages = messages.into_iter();
        let archive = messages.clone().any(|m| can_archive(accounts, m));
        let move_to = one_account(messages.clone()).is_some_and(|a| account(accounts, a).is_some());
        Self {
            flags: common_flags(messages),
            archive,
            move_to,
        }
    }

    /// The flags the messages all have, which the toggles' names and icons follow.
    pub fn flags(&self) -> Flags {
        self.flags
    }

    /// Whether `command` does anything to the messages: its button is disabled if not.
    pub fn enables(&self, command: MessageCommand) -> bool {
        match command {
            MessageCommand::Archive => self.archive,
            MessageCommand::Move => self.move_to,
            MessageCommand::Delete | MessageCommand::ToggleRead | MessageCommand::ToggleFlag => {
                true
            }
        }
    }
}

fn account(accounts: &[AccountEntry], id: AccountId) -> Option<&AccountEntry> {
    accounts.iter().find(|e| e.account.id == id)
}

fn can_archive(accounts: &[AccountEntry], message: &MessageSummary) -> bool {
    account(accounts, message.account_id).is_some_and(|e| e.folders.can_archive(message))
}

/// A change to some messages: which, and what it does, for its failure ("Couldn't
/// {what}").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub ids: Vec<MessageId>,
    pub what: &'static str,
}

impl Change {
    fn new(ids: Vec<MessageId>, one: &'static str, many: &'static str) -> Self {
        let what = if ids.len() == 1 { one } else { many };
        Self { ids, what }
    }

    /// Whether it changes no message: nothing to do.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// Setting and clearing flags on messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagChange {
    pub change: Change,
    pub add: Flags,
    pub remove: Flags,
}

/// What toggling `flag` on the messages does: it comes off if they all have it, or else
/// goes on them all.
pub fn toggled(messages: &[MessageSummary], flag: Flags) -> FlagChange {
    let (add, remove) = if common_flags(messages).contains(flag) {
        (Flags::empty(), flag)
    } else {
        (flag, Flags::empty())
    };
    let ids = messages.iter().map(|m| m.id).collect();
    FlagChange {
        change: Change::new(ids, "change the message", "change the messages"),
        add,
        remove,
    }
}

/// Marking a message read, as opening it does.
pub fn mark_read(id: MessageId) -> FlagChange {
    FlagChange {
        change: Change {
            ids: vec![id],
            what: "mark the message as read",
        },
        add: Flags::SEEN,
        remove: Flags::empty(),
    }
}

/// Those of the messages Archive does anything to: their account has an Archive folder
/// they aren't in, or on Gmail they are in INBOX and All Mail.
pub fn archivable(messages: &[MessageSummary], accounts: &[AccountEntry]) -> Change {
    let ids = messages
        .iter()
        .filter(|m| can_archive(accounts, m))
        .map(|m| m.id)
        .collect();
    Change::new(ids, "archive the message", "archive the messages")
}

/// Those of the messages, all of one account and looked at in `shown_in`
/// ([`Source::shown_in`](crate::mailbox::Source::shown_in)), that moving to `to` changes;
/// none if they are of more than one.
pub fn movable(
    messages: &[MessageSummary],
    accounts: &[AccountEntry],
    shown_in: Option<FolderId>,
    to: FolderId,
) -> Change {
    let folders = one_account(messages).and_then(|a| account(accounts, a));
    let ids = folders.map_or_else(Vec::new, |entry| {
        messages
            .iter()
            .filter(|m| entry.folders.can_move(m.folder_id, shown_in, to))
            .map(|m| m.id)
            .collect()
    });
    Change::new(ids, "move the message", "move the messages")
}

/// The folders "Move to" offers for the messages, all of one account and looked at in
/// `shown_in`: those moving any of them to changes, by
/// [`AccountFolders::paths`](crate::folders::AccountFolders::paths). None if the messages
/// are of more than one account, or one not among `accounts`.
pub fn move_targets(
    messages: &[MessageSummary],
    accounts: &[AccountEntry],
    shown_in: Option<FolderId>,
) -> Option<Vec<(FolderId, String)>> {
    let folders = &account(accounts, one_account(messages)?)?.folders;
    // Whether a move changes a message depends on its folder only: each once, not each
    // message once ("Select all").
    let from: HashSet<FolderId> = messages.iter().map(|m| m.folder_id).collect();
    let targets = folders
        .paths()
        .into_iter()
        .filter(|(folder, _)| {
            from.iter()
                .any(|&from| folders.can_move(from, shown_in, folder.id))
        })
        .map(|(folder, path)| (folder.id, path))
        .collect();
    Some(targets)
}

/// Adding labels to a message and taking them off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelChange {
    pub change: Change,
    pub add: Vec<FolderId>,
    pub remove: Vec<FolderId>,
}

/// What picking `label` in "Labels" does to the message, as it has the label now.
pub fn label_change(message: &MessageSummary, label: FolderId) -> LabelChange {
    let (add, remove) = if message.labels.contains(&label) {
        (Vec::new(), vec![label])
    } else {
        (vec![label], Vec::new())
    };
    LabelChange {
        change: Change {
            ids: vec![message.id],
            what: "change the labels",
        },
        add,
        remove,
    }
}

/// Which of a folder picker's entries, by name, show for what is typed.
pub fn picker_filter(query: &str) -> impl Fn(&str) -> bool + use<> {
    let query = query.trim().to_lowercase();
    move |name| name.to_lowercase().contains(&query)
}

/// What a folder picker says when it lists nothing: "Labels" (`labels`) or "Move to",
/// with `any` entries before filtering.
pub fn picker_empty_text(labels: bool, any: bool) -> &'static str {
    match (labels, any) {
        (true, false) => "No labels",
        (true, true) => "No label by that name",
        (false, _) => "No folder by that name",
    }
}

/// Deleting some messages: each to Trash, or for good from Trash or without one, as the
/// engine will ([`DeleteOutcome`]), in one change.
#[derive(Debug, PartialEq)]
pub struct Deletion {
    pub change: Change,
    pub mode: DeleteMode,
    /// Some go for good and can't be brought back: the user is asked first, once for them
    /// all.
    pub confirm: Option<Confirm>,
}

/// What asking about a change that can't be undone says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub title: String,
    pub description: String,
    /// The button that goes on: "Delete", "Remove".
    pub action: &'static str,
}

/// How deleting the messages goes. Messages of an account not among `accounts` are left
/// out.
pub fn deletion(messages: &[MessageSummary], accounts: &[AccountEntry]) -> Deletion {
    let (mut ids, mut for_good) = (Vec::new(), Vec::new());
    // For good without Trash only where the user is asked about it: if an account's
    // Trash went away since its folders were listed here, the engine refuses.
    let mut mode = DeleteMode::ToTrash;
    for message in messages {
        let Some(entry) = account(accounts, message.account_id) else {
            continue;
        };
        ids.push(message.id);
        let outcome = entry.folders.delete_outcome(message.folder_id);
        if outcome == DeleteOutcome::NoTrash {
            mode = DeleteMode::ByOutcome;
        }
        if outcome.is_permanent() {
            for_good.push(message);
        }
    }
    let confirm = match for_good[..] {
        [] => None,
        [message] => Some(Confirm {
            title: format!(
                "Delete “{}” for good?",
                format::subject(message.subject.as_deref())
            ),
            description: "It is removed from the server and can't be brought back.".into(),
            action: "Delete",
        }),
        _ => Some(Confirm {
            title: format!("Delete {} messages for good?", for_good.len()),
            description: "They are removed from the server and can't be brought back.".into(),
            action: "Delete",
        }),
    };
    Deletion {
        change: Change::new(ids, "delete the message", "delete the messages"),
        mode,
        confirm,
    }
}

/// What to tell the user about a change the server didn't make.
pub fn refused_change(op: &PendingOp) -> String {
    let subject = op.subject.as_deref().unwrap_or("a message");
    match op.status {
        OpStatus::Conflict => {
            format!("A change to “{subject}” was dropped: the folder changed on the server first.")
        }
        _ => {
            let verb = match op.kind {
                OpKind::Flags { .. } | OpKind::Label { .. } => "change",
                OpKind::Move { .. } => "move",
                OpKind::Delete => "delete",
            };
            format!(
                "The server didn't {verb} “{subject}”, so it is as it was. {}",
                op.last_error.as_deref().unwrap_or_default()
            )
            .trim_end()
            .to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use catmail_core::FolderRole;

    use super::*;
    use crate::fixtures::{account, folder, gmail, gmail_folder, message, standard};
    use crate::mailbox::Source;

    /// A standard account with Trash (`standard(1)`), one without (2), and Gmail (3).
    fn accounts() -> Vec<AccountEntry> {
        let gmail = gmail(
            3,
            vec![
                gmail_folder(31, 3, "INBOX"),
                gmail_folder(32, 3, "All Mail"),
                gmail_folder(33, 3, "Trash"),
                gmail_folder(34, 3, "Work"),
            ],
        );
        vec![standard(1), account(2, vec![folder(21, 2, "INBOX")]), gmail]
    }

    fn ids(change: &Change) -> Vec<i64> {
        change.ids.iter().map(|id| id.0).collect()
    }

    #[test]
    fn archive_takes_what_it_changes_and_says_what_failed() {
        let accounts = accounts();
        let mut in_inbox = message(3, 3, 32);
        in_inbox.labels = vec![FolderId(31)];
        let messages = [
            message(1, 1, 11), // Into Archive.
            message(2, 1, 13), // In Archive already.
            message(4, 2, 21), // No Archive.
            in_inbox,          // Out of Gmail's INBOX.
            message(5, 3, 32), // In All Mail only.
            message(6, 9, 91), // An account not shown.
        ];
        let change = archivable(&messages, &accounts);
        assert_eq!(ids(&change), [1, 3]);
        assert_eq!(change.what, "archive the messages");
        assert_eq!(
            archivable(&messages[..1], &accounts).what,
            "archive the message"
        );
        assert!(archivable(&messages[1..2], &accounts).is_empty());
        let applies = Applies::of(&messages[..2], &accounts);
        assert!(applies.enables(MessageCommand::Archive) && applies.enables(MessageCommand::Move));
        let applies = Applies::of(&messages[1..3], &accounts);
        assert!(!applies.enables(MessageCommand::Archive), "neither changes");
        assert!(!applies.enables(MessageCommand::Move), "of two accounts");
    }

    #[test]
    fn moving_on_gmail_takes_off_the_label_it_is_looked_at_in() {
        let accounts = accounts();
        let mut work = message(1, 3, 32);
        work.labels = vec![FolderId(34)];
        let messages = [work];
        let (inbox, work) = (FolderId(31), FolderId(34));
        // Looked at in Work, moved to INBOX: Work comes off. In All Mail it only gets INBOX.
        let shown_in = Source::Folder(work).shown_in(AccountId(3), &accounts);
        assert_eq!(shown_in, Some(work));
        let change = movable(&messages, &accounts, shown_in, inbox);
        assert_eq!((ids(&change), change.what), (vec![1], "move the message"));
        let targets = move_targets(&messages, &accounts, shown_in).unwrap();
        assert!(targets.iter().any(|(id, _)| *id == inbox));
        assert!(!targets.iter().any(|(id, _)| *id == work), "it is there");
        // Of two accounts: nowhere to move to.
        let two = [message(1, 1, 11), message(2, 2, 21)];
        assert_eq!(move_targets(&two, &accounts, None), None);
        assert!(movable(&two, &accounts, None, FolderId(12)).is_empty());
    }

    #[test]
    fn a_message_is_looked_at_where_the_list_shows_it() {
        let accounts = accounts();
        let (one, gmail) = (AccountId(1), AccountId(3));
        #[rustfmt::skip]
        let cases = [
            (Source::Inboxes, one, Some(11)),
            (Source::Inboxes, gmail, Some(31)),
            (Source::Folder(FolderId(12)), one, Some(12)),
            // Not a folder of the message's account.
            (Source::Folder(FolderId(12)), gmail, None),
            (Source::Search("lunch".into()), one, None),
            (Source::Drafts(one), one, None),
            (Source::Inboxes, AccountId(9), None),
        ];
        for (source, account, expected) in cases {
            let shown_in = source.shown_in(account, &accounts);
            assert_eq!(shown_in, expected.map(FolderId), "{source:?} {account:?}");
        }
        let inbox = accounts[2].folders.with_role(FolderRole::Inbox).unwrap();
        assert_eq!(Source::Inboxes.shown_in(gmail, &accounts), Some(inbox.id));
    }

    #[test]
    fn deleting_asks_first_for_what_goes_for_good() {
        let accounts = accounts();
        let delete = |messages: &[MessageSummary]| deletion(messages, &accounts);
        let to_trash = message(1, 1, 11);
        let mut from_trash = message(2, 1, 14);
        from_trash.subject = Some("Lunch".into());
        let no_trash = message(3, 2, 21);
        let one = delete(std::slice::from_ref(&to_trash));
        assert_eq!((one.mode, one.confirm), (DeleteMode::ToTrash, None));
        assert_eq!(one.change.what, "delete the message");

        let two = delete(&[to_trash, from_trash.clone()]);
        assert_eq!(two.mode, DeleteMode::ToTrash);
        assert_eq!(two.confirm.unwrap().title, "Delete “Lunch” for good?");
        assert_eq!(two.change.what, "delete the messages");

        let for_good = delete(&[from_trash, no_trash, message(4, 9, 91)]);
        assert_eq!(ids(&for_good.change), [2, 3], "not of an account not shown");
        assert_eq!(for_good.mode, DeleteMode::ByOutcome);
        assert_eq!(
            for_good.confirm.unwrap().title,
            "Delete 2 messages for good?"
        );
    }

    #[test]
    fn toggles_and_labels_say_what_they_change() {
        let mut read = message(1, 1, 11);
        read.flags = Flags::SEEN;
        let unread = message(2, 1, 11);
        let toggle = toggled(&[read.clone(), unread.clone()], Flags::SEEN);
        assert_eq!((toggle.add, toggle.remove), (Flags::SEEN, Flags::empty()));
        assert_eq!(toggle.change.what, "change the messages");
        let toggle = toggled(std::slice::from_ref(&read), Flags::SEEN);
        assert_eq!((toggle.add, toggle.remove), (Flags::empty(), Flags::SEEN));
        assert_eq!(toggle.change.what, "change the message");
        assert_eq!(mark_read(MessageId(2)).add, Flags::SEEN);

        read.labels = vec![FolderId(34)];
        let off = label_change(&read, FolderId(34));
        assert_eq!((off.add, off.remove), (vec![], vec![FolderId(34)]));
        let on = label_change(&read, FolderId(35));
        assert_eq!((on.add, on.remove), (vec![FolderId(35)], vec![]));
        assert_eq!(on.change.what, "change the labels");
    }

    #[test]
    fn toggles_name_what_they_do() {
        let both = Flags::SEEN | Flags::FLAGGED;
        assert_eq!(MessageCommand::ToggleRead.label(both), "Mark as unread");
        assert_eq!(
            MessageCommand::ToggleRead.label(Flags::empty()),
            "Mark as read"
        );
        assert_eq!(MessageCommand::ToggleFlag.label(both), "Remove flag");
        assert_eq!(MessageCommand::ToggleFlag.label(Flags::SEEN), "Flag");
    }

    #[test]
    fn the_picker_filters_by_name() {
        assert!(picker_filter(" invo ")("Clients / Invoices"));
        assert!(picker_filter("")("Inbox"));
        assert!(!picker_filter("sent")("Inbox"));
    }
}
