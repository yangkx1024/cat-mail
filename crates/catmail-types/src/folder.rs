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

impl Folder {
    /// Whether it is a Gmail label a message can be given. Starred is a label's mailbox
    /// too, but it shows `\Flagged` rather than a label.
    pub fn is_label(&self) -> bool {
        self.kind == FolderKind::Label && self.role != Some(FolderRole::Flagged)
    }

    /// Whether it is a Gmail label of the user's that frontends show, on its messages and
    /// apart from the folders. The system labels (INBOX, Sent, Drafts, Important) have a
    /// role: they are where a message is listed, not what it is about. One hidden from
    /// IMAP in Gmail's settings isn't shown anywhere.
    pub fn is_user_label(&self) -> bool {
        self.kind == FolderKind::Label && self.role.is_none() && self.listed
    }
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

/// What moving or archiving a message does (see `docs/architecture/labels.md`). On a
/// standard account every mailbox is a location, and a move changes it. On Gmail most
/// mailboxes are labels: a move between them changes labels, and only one into or out
/// of Trash or Spam changes the location.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MovePlan {
    /// The location it moves to.
    pub to: Option<FolderId>,
    /// The labels it loses, once in `to` if it moves: the one it was looked at in, or
    /// those Gmail takes off in Trash and Spam (Gmail does that itself; they are taken
    /// off here so that the change is known before the server makes it).
    pub remove: Vec<FolderId>,
    /// The label it gets, once in `to` if it moves.
    pub add: Option<FolderId>,
}

/// Why a message can't be moved to a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unmovable {
    /// Not one of the account's folders.
    NotFound,
    /// A hierarchy node (`\Noselect`).
    HoldsNoMessages,
    /// Starred, which is `\Flagged`: star the message instead.
    Starred,
}

impl MovePlan {
    /// For a message in location `from` that is looked at in `shown_in` (a label, on
    /// Gmail; otherwise, or with `None`, where it is), moved to `to`; all of them among
    /// the account's `folders`. From a label the move takes that label off, as Gmail
    /// does. Empty if it is there already.
    pub fn of(
        folders: &[Folder],
        from: FolderId,
        shown_in: Option<FolderId>,
        to: FolderId,
    ) -> Result<Self, Unmovable> {
        let target = folders
            .iter()
            .find(|f| f.id == to)
            .ok_or(Unmovable::NotFound)?;
        if !target.selectable {
            return Err(Unmovable::HoldsNoMessages);
        }
        // Starred stays: it is a flag, not a label the message was moved out of.
        let shown = shown_in
            .and_then(|id| folders.iter().find(|f| f.id == id))
            .filter(|f| f.is_label())
            .map(|f| f.id);
        Ok(match target.kind {
            // Into the location it is in: out of the label only (to All Mail from INBOX
            // is archiving it).
            FolderKind::Location if to == from => Self {
                remove: shown.into_iter().collect(),
                ..Self::default()
            },
            FolderKind::Location => Self {
                to: Some(to),
                remove: taken_off_in(folders, target),
                ..Self::default()
            },
            FolderKind::Label if !target.is_label() => return Err(Unmovable::Starred),
            FolderKind::Label if shown_in == Some(to) => Self::default(),
            FolderKind::Label => {
                // A label shows the messages of All Mail: one in Trash or Spam goes there.
                let all = all_mail(folders);
                Self {
                    to: all.filter(|&all| all != from),
                    remove: shown.filter(|_| all == Some(from)).into_iter().collect(),
                    add: Some(to),
                }
            }
        })
    }

    /// Archiving a message in location `from`, one of the account's `folders`. On a
    /// standard account it moves to the Archive folder ([`special_folder`]), unless it is
    /// in an Archive folder already. On Gmail it loses `\Inbox`, if it is in All Mail.
    /// `None` if there is nothing archiving can do.
    pub fn archive(folders: &[Folder], from: FolderId) -> Option<Self> {
        if let Some(inbox) = inbox_label(folders) {
            return (all_mail(folders)? == from).then(|| Self {
                remove: vec![inbox],
                ..Self::default()
            });
        }
        if role_of(folders, from) == Some(FolderRole::Archive) {
            return None;
        }
        Some(Self {
            to: Some(special_folder(folders, FolderRole::Archive)?.id),
            ..Self::default()
        })
    }

    /// Whether the account archives at all: it has an Archive folder, or is Gmail's.
    pub fn can_archive(folders: &[Folder]) -> bool {
        inbox_label(folders).is_some() || special_folder(folders, FolderRole::Archive).is_some()
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Whether it changes a message in location `from` with `labels`: archiving one that
    /// isn't in INBOX doesn't.
    pub fn changes(&self, from: FolderId, labels: &[FolderId]) -> bool {
        self.to.is_some_and(|to| to != from)
            || self.remove.iter().any(|l| labels.contains(l))
            || self.add.is_some_and(|l| !labels.contains(&l))
    }
}

/// The labels Gmail takes off a message moved into `location`: `\Inbox` in Trash, that
/// and `\Important` in Spam. None on a standard account.
fn taken_off_in(folders: &[Folder], location: &Folder) -> Vec<FolderId> {
    let roles: &[FolderRole] = match location.role {
        Some(FolderRole::Trash) => &[FolderRole::Inbox],
        Some(FolderRole::Junk) => &[FolderRole::Inbox, FolderRole::Important],
        _ => &[],
    };
    roles
        .iter()
        .filter_map(|&role| special_folder(folders, role))
        .filter(|f| f.is_label())
        .map(|f| f.id)
        .collect()
}

/// Gmail's INBOX, a label.
fn inbox_label(folders: &[Folder]) -> Option<FolderId> {
    special_folder(folders, FolderRole::Inbox)
        .filter(|f| f.kind == FolderKind::Label)
        .map(|f| f.id)
}

/// Gmail's All Mail, the location of every message not in Trash or Spam.
fn all_mail(folders: &[Folder]) -> Option<FolderId> {
    special_folder(folders, FolderRole::All)
        .filter(|f| f.kind == FolderKind::Location)
        .map(|f| f.id)
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

    fn moves_to(to: i64) -> MovePlan {
        MovePlan {
            to: Some(FolderId(to)),
            ..MovePlan::default()
        }
    }

    #[test]
    fn archiving_skips_what_is_archived_already() {
        let folders = [
            folder(1, Some(FolderRole::Inbox), true),
            folder(4, Some(FolderRole::Archive), true),
            folder(6, Some(FolderRole::Archive), true),
        ];
        let archive = |from| MovePlan::archive(&folders, FolderId(from));
        assert_eq!(archive(1), Some(moves_to(4)));
        assert_eq!(archive(4), None);
        assert_eq!(archive(6), None);
        assert_eq!(MovePlan::archive(&folders[..1], FolderId(1)), None);
    }

    /// INBOX (1), Work (2), Starred (3) and Important (4) are labels; All Mail (5),
    /// Trash (6) and Spam (7) locations.
    fn gmail() -> Vec<Folder> {
        let label = |id, role| Folder {
            kind: FolderKind::Label,
            ..folder(id, role, true)
        };
        vec![
            label(1, Some(FolderRole::Inbox)),
            label(2, None),
            label(3, Some(FolderRole::Flagged)),
            label(4, Some(FolderRole::Important)),
            folder(5, Some(FolderRole::All), true),
            folder(6, Some(FolderRole::Trash), true),
            folder(7, Some(FolderRole::Junk), true),
        ]
    }

    #[test]
    fn moves_on_gmail_change_labels_unless_into_or_out_of_trash_or_spam() {
        let folders = gmail();
        let plan = |from, shown_in: Option<i64>, to| {
            MovePlan::of(
                &folders,
                FolderId(from),
                shown_in.map(FolderId),
                FolderId(to),
            )
        };
        let labels = |add: Option<i64>, remove: Option<i64>| MovePlan {
            to: None,
            add: add.map(FolderId),
            remove: remove.map(FolderId).into_iter().collect(),
        };
        let moves = |to: i64, remove: &[i64]| MovePlan {
            to: Some(FolderId(to)),
            remove: remove.iter().copied().map(FolderId).collect(),
            add: None,
        };
        // From INBOX to Work; from All Mail or a search, Work is only added.
        assert_eq!(plan(5, Some(1), 2), Ok(labels(Some(2), Some(1))));
        assert_eq!(plan(5, Some(5), 2), Ok(labels(Some(2), None)));
        assert_eq!(plan(5, None, 2), Ok(labels(Some(2), None)));
        // Starred keeps the star; it is no target.
        assert_eq!(plan(5, Some(3), 2), Ok(labels(Some(2), None)));
        assert_eq!(plan(5, Some(1), 3), Err(Unmovable::Starred));
        // To All Mail from INBOX archives it; to where it is changes nothing.
        assert_eq!(plan(5, Some(1), 5), Ok(labels(None, Some(1))));
        assert!(plan(5, Some(2), 2).unwrap().is_empty());
        assert!(plan(5, None, 5).unwrap().is_empty());
        // Trash and Spam are locations, which take INBOX (and Important) off; out of
        // them, into All Mail first.
        assert_eq!(plan(5, Some(2), 6), Ok(moves(6, &[1])));
        assert_eq!(plan(6, Some(6), 7), Ok(moves(7, &[1, 4])));
        assert_eq!(plan(6, Some(6), 5), Ok(moves(5, &[])));
        assert_eq!(
            plan(6, Some(6), 1),
            Ok(MovePlan {
                to: Some(FolderId(5)),
                add: Some(FolderId(1)),
                remove: Vec::new()
            })
        );
        assert_eq!(plan(5, None, 9), Err(Unmovable::NotFound));
    }

    #[test]
    fn archiving_on_gmail_takes_inbox_off() {
        let folders = gmail();
        let inbox = MovePlan {
            remove: vec![FolderId(1)],
            ..MovePlan::default()
        };
        assert_eq!(MovePlan::archive(&folders, FolderId(5)), Some(inbox));
        assert_eq!(MovePlan::archive(&folders, FolderId(6)), None);
        assert_eq!(MovePlan::archive(&folders, FolderId(7)), None);
        assert!(MovePlan::can_archive(&folders));
    }

    #[test]
    fn a_plan_changes_a_message_only_if_it_differs() {
        let folders = gmail();
        let ids = |ids: &[i64]| ids.iter().copied().map(FolderId).collect::<Vec<_>>();
        let all_mail = FolderId(5);
        // Archiving takes INBOX off what has it, and does nothing to the rest.
        let archive = MovePlan::archive(&folders, all_mail).unwrap();
        assert!(archive.changes(all_mail, &ids(&[1, 2])));
        assert!(!archive.changes(all_mail, &ids(&[2])));
        // A label added that it has, a move to where it is: nothing.
        let work = MovePlan::of(&folders, all_mail, None, FolderId(2)).unwrap();
        assert!(!work.changes(all_mail, &ids(&[2])));
        assert!(work.changes(all_mail, &ids(&[1])));
        assert!(!moves_to(5).changes(all_mail, &[]));
        assert!(moves_to(6).changes(all_mail, &[]));
    }

    #[test]
    fn moves_on_a_standard_account_change_the_folder() {
        let folders = [
            folder(1, Some(FolderRole::Inbox), true),
            folder(2, None, true),
            folder(3, None, false),
        ];
        let plan =
            |from, to| MovePlan::of(&folders, FolderId(from), Some(FolderId(1)), FolderId(to));
        assert_eq!(plan(1, 2), Ok(moves_to(2)));
        assert!(plan(1, 1).unwrap().is_empty());
        assert_eq!(plan(1, 3), Err(Unmovable::HoldsNoMessages));
        assert!(!MovePlan::can_archive(&folders));
    }
}
