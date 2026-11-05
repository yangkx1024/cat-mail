//! The commands on messages, the same wherever they show (over the message open, by the
//! list's heading for the messages checked, on a row while hovered): what they do to the
//! messages, what they are called, which changes ask first, and what to say when the
//! server refuses one.

use catmail_core::{
    AccountId, DeleteMode, DeleteOutcome, Flags, FolderId, MessageId, MessageSummary, OpKind,
    OpStatus, PendingOp,
};

use crate::folders::AccountFolders;
use crate::format;

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
    /// `folders` gives an account's folders.
    pub fn of<'a, I>(messages: I, folders: impl Fn(AccountId) -> Option<&'a AccountFolders>) -> Self
    where
        I: IntoIterator<Item = &'a MessageSummary>,
        I::IntoIter: Clone,
    {
        let messages = messages.into_iter();
        let archive = messages
            .clone()
            .any(|m| folders(m.account_id).is_some_and(|f| f.can_archive(m)));
        let move_to = one_account(messages.clone()).is_some_and(|a| folders(a).is_some());
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

/// What toggling `flag` on the messages does: (add, remove). It comes off if they all
/// have it, or else goes on them all.
pub fn toggled(messages: &[MessageSummary], flag: Flags) -> (Flags, Flags) {
    if common_flags(messages).contains(flag) {
        (Flags::empty(), flag)
    } else {
        (flag, Flags::empty())
    }
}

/// What a command does, for its failure: "Couldn't {what}".
pub fn what(messages: usize, one: &'static str, many: &'static str) -> &'static str {
    if messages == 1 { one } else { many }
}

/// Those of the messages Archive does anything to: their account has an Archive folder
/// they aren't in, or on Gmail they are in INBOX and All Mail.
pub fn archivable<'a>(
    messages: &[MessageSummary],
    folders: impl Fn(AccountId) -> Option<&'a AccountFolders>,
) -> Vec<MessageId> {
    messages
        .iter()
        .filter(|m| folders(m.account_id).is_some_and(|f| f.can_archive(m)))
        .map(|m| m.id)
        .collect()
}

/// Those of the messages, all of the account with `folders` and looked at in
/// `shown_in`, that moving to `to` changes.
pub fn movable(
    folders: &AccountFolders,
    messages: &[MessageSummary],
    shown_in: Option<FolderId>,
    to: FolderId,
) -> Vec<MessageId> {
    messages
        .iter()
        .filter(|m| folders.can_move(m.folder_id, shown_in, to))
        .map(|m| m.id)
        .collect()
}

/// The folders "Move to" offers for the messages, all of the account with `folders` and
/// looked at in `shown_in`: those moving any of them to changes, by
/// [`AccountFolders::paths`].
pub fn move_targets(
    folders: &AccountFolders,
    messages: &[MessageSummary],
    shown_in: Option<FolderId>,
) -> Vec<(FolderId, String)> {
    folders
        .paths()
        .into_iter()
        .filter(|(folder, _)| {
            messages
                .iter()
                .any(|m| folders.can_move(m.folder_id, shown_in, folder.id))
        })
        .map(|(folder, path)| (folder.id, path))
        .collect()
}

/// What picking `label` in "Labels" does to the message: (add, remove), as it has the
/// label now.
pub fn label_change(message: &MessageSummary, label: FolderId) -> (Vec<FolderId>, Vec<FolderId>) {
    if message.labels.contains(&label) {
        (Vec::new(), vec![label])
    } else {
        (vec![label], Vec::new())
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
    pub ids: Vec<MessageId>,
    pub mode: DeleteMode,
    /// Some go for good and can't be brought back: the user is asked first, once for them
    /// all.
    pub confirm: Option<Confirm>,
}

/// What asking about a change that can't be undone says.
#[derive(Debug, PartialEq)]
pub struct Confirm {
    pub title: String,
    pub description: String,
}

/// How deleting the messages goes; `folders` gives an account's folders. Messages of an
/// account it doesn't know are left out.
pub fn deletion<'a>(
    messages: &[MessageSummary],
    folders: impl Fn(AccountId) -> Option<&'a AccountFolders>,
) -> Deletion {
    let (mut ids, mut for_good) = (Vec::new(), Vec::new());
    // For good without Trash only where the user is asked about it: if an account's
    // Trash went away since its folders were listed here, the engine refuses.
    let mut mode = DeleteMode::ToTrash;
    for message in messages {
        let Some(folders) = folders(message.account_id) else {
            continue;
        };
        ids.push(message.id);
        let outcome = folders.delete_outcome(message.folder_id);
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
        }),
        _ => Some(Confirm {
            title: format!("Delete {} messages for good?", for_good.len()),
            description: "They are removed from the server and can't be brought back.".into(),
        }),
    };
    Deletion { ids, mode, confirm }
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
    use super::*;

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
