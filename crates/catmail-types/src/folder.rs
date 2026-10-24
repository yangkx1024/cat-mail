use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{AccountId, FolderId};

/// Well-known folder purpose, detected from IMAP SPECIAL-USE attributes or names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    All,
    Flagged,
    /// Gmail's Important (RFC 8457 `\Important`).
    Important,
}

impl FolderRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Sent => "sent",
            Self::Drafts => "drafts",
            Self::Trash => "trash",
            Self::Junk => "junk",
            Self::Archive => "archive",
            Self::All => "all",
            Self::Flagged => "flagged",
            Self::Important => "important",
        }
    }

    /// The name frontends give a folder with this role, whatever the server calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Sent => "Sent",
            Self::Drafts => "Drafts",
            Self::Trash => "Trash",
            Self::Junk => "Junk",
            Self::Archive => "Archive",
            Self::All => "All Mail",
            Self::Flagged => "Flagged",
            Self::Important => "Important",
        }
    }

    /// Whether unread mail here is news worth a count: not in Sent, Junk or Trash.
    pub fn counts_unread(self) -> bool {
        !matches!(self, Self::Sent | Self::Junk | Self::Trash)
    }
}

impl FromStr for FolderRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "inbox" => Self::Inbox,
            "sent" => Self::Sent,
            "drafts" => Self::Drafts,
            "trash" => Self::Trash,
            "junk" => Self::Junk,
            "archive" => Self::Archive,
            "all" => Self::All,
            "flagged" => Self::Flagged,
            "important" => Self::Important,
            other => return Err(format!("unknown folder role `{other}`")),
        })
    }
}

/// What a mailbox is to an account (see `docs/architecture/labels.md`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderKind {
    /// Where messages are: each message is in exactly one, under its own UID. Every
    /// mailbox of a standard IMAP account; All Mail, Trash and Spam on Gmail.
    #[default]
    Location,
    /// A label on messages that are in a location: the other mailboxes on Gmail. Not
    /// synced itself; its messages are those with the label.
    Label,
}

impl FolderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Location => "location",
            Self::Label => "label",
        }
    }
}

impl FromStr for FolderKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "location" => Self::Location,
            "label" => Self::Label,
            other => return Err(format!("unknown folder kind `{other}`")),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    pub account_id: AccountId,
    /// Full mailbox name for display, e.g. `INBOX` or `Archive/2026`; hierarchy levels
    /// are separated by `delimiter`.
    pub name: String,
    pub delimiter: Option<char>,
    pub role: Option<FolderRole>,
    pub kind: FolderKind,
    /// False for a label the server doesn't list (hidden in Gmail's settings) that
    /// messages still carry: kept, not shown.
    pub listed: bool,
    /// False for hierarchy-only nodes (`\Noselect`) that cannot hold messages.
    pub selectable: bool,
    pub unread_count: u32,
    pub total_count: u32,
    /// Unix timestamp, seconds, of the last completed sync.
    pub last_synced_at: Option<i64>,
}

/// The folder the engine uses for `role` among an account's folders: where Delete moves
/// to, Archive moves to, sent mail is saved. An account may list several with a role;
/// it is the selectable one with the lowest id. The store's `Folders::by_role` applies
/// this too.
pub fn special_folder(folders: &[Folder], role: FolderRole) -> Option<&Folder> {
    folders
        .iter()
        .filter(|f| f.selectable && f.role == Some(role))
        .min_by_key(|f| f.id)
}

/// What deleting a message does, unless permanent deletion is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// It moves to the account's Trash, and can be brought back from there.
    ToTrash(FolderId),
    /// It is in a Trash folder already, so it is deleted for good.
    Permanent,
    /// The account has no Trash folder: it can only be deleted for good.
    NoTrash,
}

impl DeleteOutcome {
    /// For a message in `from`, one of the account's `folders`. Any Trash folder counts
    /// as Trash, not only the one deletion moves to ([`special_folder`]).
    pub fn of(folders: &[Folder], from: FolderId) -> Self {
        if role_of(folders, from) == Some(FolderRole::Trash) {
            return Self::Permanent;
        }
        match special_folder(folders, FolderRole::Trash) {
            Some(trash) => Self::ToTrash(trash.id),
            None => Self::NoTrash,
        }
    }

    /// Whether the message can't be brought back afterwards.
    pub fn is_permanent(self) -> bool {
        !matches!(self, Self::ToTrash(_))
    }
}

/// Where archiving moves a message in `from`, one of the account's `folders`: its Archive
/// folder ([`special_folder`]), unless the message is in an Archive folder already.
pub fn archive_folder(folders: &[Folder], from: FolderId) -> Option<FolderId> {
    if role_of(folders, from) == Some(FolderRole::Archive) {
        return None;
    }
    special_folder(folders, FolderRole::Archive).map(|f| f.id)
}

fn role_of(folders: &[Folder], id: FolderId) -> Option<FolderRole> {
    folders.iter().find(|f| f.id == id).and_then(|f| f.role)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, role: Option<FolderRole>, selectable: bool) -> Folder {
        Folder {
            id: FolderId(id),
            account_id: AccountId(1),
            name: format!("f{id}"),
            delimiter: Some('/'),
            role,
            kind: FolderKind::Location,
            listed: true,
            selectable,
            unread_count: 0,
            total_count: 0,
            last_synced_at: None,
        }
    }

    #[test]
    fn the_special_folder_is_the_selectable_one_with_the_lowest_id() {
        let folders = [
            folder(1, Some(FolderRole::Inbox), true),
            folder(5, Some(FolderRole::Trash), true),
            folder(3, Some(FolderRole::Trash), true),
            folder(2, Some(FolderRole::Trash), false),
        ];
        assert_eq!(
            special_folder(&folders, FolderRole::Trash).map(|f| f.id),
            Some(FolderId(3))
        );
        assert!(special_folder(&folders, FolderRole::Archive).is_none());
    }

    #[test]
    fn deleting_from_any_trash_folder_is_for_good() {
        let folders = [
            folder(1, Some(FolderRole::Inbox), true),
            folder(3, Some(FolderRole::Trash), true),
            folder(5, Some(FolderRole::Trash), true),
        ];
        assert_eq!(
            DeleteOutcome::of(&folders, FolderId(1)),
            DeleteOutcome::ToTrash(FolderId(3))
        );
        assert_eq!(
            DeleteOutcome::of(&folders, FolderId(3)),
            DeleteOutcome::Permanent
        );
        // Not the Trash the engine moves to, but a Trash all the same.
        assert_eq!(
            DeleteOutcome::of(&folders, FolderId(5)),
            DeleteOutcome::Permanent
        );
        assert_eq!(
            DeleteOutcome::of(&folders[..1], FolderId(1)),
            DeleteOutcome::NoTrash
        );
        assert!(!DeleteOutcome::ToTrash(FolderId(3)).is_permanent());
        assert!(DeleteOutcome::NoTrash.is_permanent());
    }

    #[test]
    fn archiving_skips_what_is_archived_already() {
        let folders = [
            folder(1, Some(FolderRole::Inbox), true),
            folder(4, Some(FolderRole::Archive), true),
            folder(6, Some(FolderRole::Archive), true),
        ];
        assert_eq!(archive_folder(&folders, FolderId(1)), Some(FolderId(4)));
        assert_eq!(archive_folder(&folders, FolderId(4)), None);
        assert_eq!(archive_folder(&folders, FolderId(6)), None);
        assert_eq!(archive_folder(&folders[..1], FolderId(1)), None);
    }
}
