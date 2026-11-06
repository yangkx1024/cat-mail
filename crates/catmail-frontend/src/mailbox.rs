//! What the mailbox window shows, apart from how: the places the sidebar selects, what
//! the message list lists, and each account as the sidebar shows it.

pub mod sidebar;

use std::time::Duration;

use catmail_core::{
    Account, AccountId, AccountMode, AccountOverview, AccountStatus, ErrorKind, Folder, FolderId,
    FolderRole, Identity, MessageId, OutboxEntry,
};

use crate::commands::Confirm;
use crate::folders::{self, AccountFolders, FolderIcon};

/// Opening a folder synced longer ago than this syncs it. Background sync keeps only
/// INBOX current between full syncs; the server can't tell us about the others.
pub const SYNC_ON_OPEN_AFTER: Duration = Duration::from_secs(60);

/// After reading the sidebar or the list again for the engine's events, how long the
/// next events wait to be read together. A sync sends one per batch of messages.
pub const REFRESH_INTERVAL: Duration = Duration::from_millis(200);

/// Why a Gmail account's labels are folders, and how to have labels again.
pub const ALL_MAIL_HIDDEN: &str = "Labels show as folders because All Mail is hidden from \
    IMAP. Turn on “Show in IMAP” for it in Gmail’s settings.";

/// Where the sidebar's selection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Every account's INBOX, as one list.
    Inboxes,
    Folder(FolderId),
    /// The drafts kept here (the server's Drafts folder holds copies of them).
    Drafts(AccountId),
    Outbox(AccountId),
}

impl Place {
    /// What the list shows for it.
    pub fn source(self) -> Source {
        match self {
            Place::Inboxes => Source::Inboxes,
            Place::Folder(id) => Source::Folder(id),
            Place::Drafts(account) => Source::Drafts(account),
            Place::Outbox(account) => Source::Outbox(account),
        }
    }

    /// The account the place belongs to, among these; none for All Inboxes.
    pub fn account_in(self, accounts: &[AccountEntry]) -> Option<&AccountEntry> {
        accounts.iter().find(|a| match self {
            Place::Inboxes => false,
            Place::Folder(id) => a.folders.find(id).is_some(),
            Place::Drafts(account) | Place::Outbox(account) => a.account.id == account,
        })
    }

    /// Whether these accounts have it. All Inboxes only with more than one: of one
    /// account, it is that account's INBOX.
    pub fn is_in(self, accounts: &[AccountEntry]) -> bool {
        match self {
            Place::Inboxes => accounts.len() > 1,
            _ => self.account_in(accounts).is_some(),
        }
    }

    /// Where the mailbox starts: All Inboxes if there are, else the first INBOX (or the
    /// first folder that holds messages).
    pub fn start(accounts: &[AccountEntry]) -> Option<Place> {
        Place::Inboxes
            .is_in(accounts)
            .then_some(Place::Inboxes)
            .or_else(|| {
                accounts.iter().find_map(|a| {
                    a.folders
                        .with_role(FolderRole::Inbox)
                        .or_else(|| a.folders.nodes().find_map(|n| n.selectable()))
                        .map(|f| Place::Folder(f.id))
                })
            })
    }

    /// The folders whose messages it lists: one, or for All Inboxes each account's INBOX;
    /// none for the drafts and the outbox.
    pub fn folders(self, accounts: &[AccountEntry]) -> Vec<&Folder> {
        match self {
            Place::Inboxes => accounts
                .iter()
                .filter_map(|a| a.folders.with_role(FolderRole::Inbox))
                .collect(),
            Place::Folder(id) => accounts
                .iter()
                .find_map(|a| a.folders.find(id)?.selectable())
                .into_iter()
                .collect(),
            Place::Drafts(_) | Place::Outbox(_) => Vec::new(),
        }
    }

    /// Its name, as the sidebar shows it.
    pub fn label(self, accounts: &[AccountEntry]) -> Option<String> {
        Some(match self {
            Place::Inboxes => "All Inboxes".into(),
            Place::Folder(id) => self.account_in(accounts)?.folders.find(id)?.label.clone(),
            Place::Drafts(_) => FolderRole::Drafts.label().into(),
            Place::Outbox(_) => "Outbox".into(),
        })
    }

    /// Its icon, as the sidebar shows it.
    pub fn icon(self, accounts: &[AccountEntry]) -> FolderIcon {
        match self {
            Place::Inboxes => FolderIcon::Inboxes,
            Place::Folder(id) => {
                let node = self.account_in(accounts).and_then(|a| a.folders.find(id));
                folders::icon(node.and_then(|n| n.folder.as_ref()))
            }
            Place::Drafts(_) => FolderIcon::Role(FolderRole::Drafts),
            Place::Outbox(_) => FolderIcon::Outbox,
        }
    }
}

/// Where to take the mailbox: a place, or an account's INBOX and a message to open there
/// (what a notification was about), which the mailbox finds among its folders. Not All
/// Inboxes, even if shown: the message may be too far down to be loaded there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Place(Place),
    Inbox {
        account: AccountId,
        message: Option<MessageId>,
    },
}

impl Destination {
    /// The place it is among these accounts, if still there, and the message to open
    /// there.
    pub fn find(self, accounts: &[AccountEntry]) -> Option<(Place, Option<MessageId>)> {
        match self {
            Destination::Place(place) => place.is_in(accounts).then_some((place, None)),
            Destination::Inbox { account, message } => {
                let inbox = accounts
                    .iter()
                    .find(|entry| entry.account.id == account)?
                    .folders
                    .with_role(FolderRole::Inbox)?;
                Some((Place::Folder(inbox.id), message))
            }
        }
    }
}

/// What the list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Every account's INBOX.
    Inboxes,
    Folder(FolderId),
    Search(String),
    Drafts(AccountId),
    Outbox(AccountId),
}

impl Source {
    /// Whether it lists messages, which can be checked; not drafts or the outbox.
    pub fn lists_messages(&self) -> bool {
        matches!(
            self,
            Source::Inboxes | Source::Folder(_) | Source::Search(_)
        )
    }
}

/// The list's title: the place's name, or what is searched for.
pub fn list_title(
    source: Option<&Source>,
    place: Option<Place>,
    accounts: &[AccountEntry],
) -> String {
    match source {
        Some(Source::Search(text)) => format!("Results for “{text}”"),
        _ => place
            .and_then(|place| place.label(accounts))
            .unwrap_or_default(),
    }
}

/// What the list's title adds: how many are unread, or why the outbox waits.
pub fn list_note(
    source: Option<&Source>,
    place: Option<Place>,
    accounts: &[AccountEntry],
) -> Option<String> {
    match source? {
        Source::Inboxes | Source::Folder(_) => {
            let unread = crate::folders::unread(place?.folders(accounts));
            (unread > 0).then(|| format!("{unread} unread"))
        }
        Source::Outbox(account) => accounts
            .iter()
            .find(|a| a.account.id == *account)
            .filter(|a| !a.status.send.is_empty() || a.status.sync.is_some())
            .map(|_| "Sending resumes when the connection is back.".to_owned()),
        _ => None,
    }
}

/// One account as the sidebar shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountEntry {
    pub account: Account,
    /// The addresses it sends as, the default first.
    pub identities: Vec<Identity>,
    pub folders: AccountFolders,
    pub drafts: usize,
    /// Still to be sent (sent ones leave the outbox).
    pub outbox: Vec<OutboxEntry>,
    pub status: AccountStatus,
}

impl AccountEntry {
    pub fn new(overview: AccountOverview, status: AccountStatus) -> Self {
        Self {
            account: overview.account,
            identities: overview.identities,
            folders: AccountFolders::new(overview.folders),
            drafts: overview.drafts,
            outbox: overview.outbox,
            status,
        }
    }

    /// Whether something in the outbox waits for the user.
    pub fn outbox_needs_user(&self) -> bool {
        self.outbox.iter().any(|e| e.status.needs_user())
    }

    /// Whether its folders are Gmail's labels.
    pub fn has_labels(&self) -> bool {
        self.account.mode == Some(AccountMode::Gmail)
    }

    /// What its folders are called.
    pub fn folder_noun(&self) -> &'static str {
        if self.has_labels() { "label" } else { "folder" }
    }

    /// What the context menu of `folder` (`None`: of the account's heading) offers. New
    /// always makes one in what was right-clicked, the top for the heading, and is left
    /// out where the user may not make one ([`can_create_in`](catmail_core::can_create_in));
    /// Rename and Delete only for a folder the user may change.
    pub fn folder_menu_entries(&self, folder: Option<FolderId>) -> Vec<FolderMenuEntry> {
        let Some(id) = folder else {
            return vec![FolderMenuEntry::New(None)];
        };
        let mut entries = Vec::new();
        if self.folders.can_create_in(id) {
            entries.push(FolderMenuEntry::New(Some(id)));
        }
        if self.folders.can_change(id) {
            entries.extend([FolderMenuEntry::Rename(id), FolderMenuEntry::Delete(id)]);
        }
        entries
    }

    /// The words of a folder menu's item.
    pub fn folder_menu_label(&self, entry: FolderMenuEntry) -> &'static str {
        match entry {
            FolderMenuEntry::New(_) if self.has_labels() => "New label",
            FolderMenuEntry::New(_) => "New folder",
            FolderMenuEntry::Rename(_) => "Rename",
            FolderMenuEntry::Delete(_) => "Delete",
        }
    }

    /// The title of the dialog that names a new folder, in `parent` or at the top.
    pub fn new_folder_title(&self, parent: Option<FolderId>) -> String {
        let noun = self.folder_noun();
        match parent.and_then(|id| self.folders.get(id)) {
            Some(parent) => format!("New {noun} in “{}”", parent.leaf_name()),
            None => format!("New {noun}"),
        }
    }

    /// The folder (a level that holds no messages too), if the user may change it.
    pub fn changeable_folder(&self, id: FolderId) -> Option<&Folder> {
        self.folders.get(id).filter(|_| self.folders.can_change(id))
    }

    /// What deleting the folder asks and says, if the user may delete it. It can't be
    /// undone; a Gmail label only comes off its messages.
    pub fn folder_deletion(&self, id: FolderId) -> Option<FolderDeletion> {
        let folder = self.changeable_folder(id)?;
        // On Gmail, every folder the user may change is a label.
        let label = self.has_labels();
        let consequence = match (label, self.folders.has_subfolders(id)) {
            (true, false) => "Its messages stay in All Mail.",
            (true, true) => "Its nested labels go too. Their messages stay in All Mail.",
            (false, false) => "Its messages are deleted from the server.",
            (false, true) => "Its subfolders and all their messages are deleted from the server.",
        };
        Some(FolderDeletion {
            confirm: Confirm {
                title: format!("Delete “{}”?", folder.leaf_name()),
                description: consequence.into(),
                action: "Delete",
            },
            what: if label {
                "delete the label"
            } else {
                "delete the folder"
            },
        })
    }
}

/// An item of a folder's context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderMenuEntry {
    /// A new folder in this one, or at the top of the account's.
    New(Option<FolderId>),
    Rename(FolderId),
    Delete(FolderId),
}

/// The confirmation of deleting a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderDeletion {
    /// The question, and what goes with the folder.
    pub confirm: Confirm,
    /// What failed, for the error: "Couldn't delete the folder".
    pub what: &'static str,
}

/// One line on what is wrong with the connection, if anything: the first account the
/// engine can't get through to (each of its addresses that can't send). A sync going on
/// is told by the Sync button alone.
pub fn connection_note(accounts: &[AccountEntry]) -> Option<String> {
    accounts.iter().find_map(|entry| {
        let email = &entry.account.email;
        let status = &entry.status;
        if let Some(problem) = &status.sync {
            return Some(match problem.error.kind {
                ErrorKind::Auth => format!("Can't sign in to {email}"),
                ErrorKind::Network => format!("Can't reach {email}"),
                ErrorKind::OverLimit => {
                    format!("{email} is over the server's download limit for now")
                }
                _ => format!("Can't sync {email}"),
            });
        }
        // Every address whose server is out of reach.
        (!status.send.is_empty()).then(|| {
            let from: Vec<_> = status.send.iter().map(|p| p.email.as_str()).collect();
            format!("Can't send from {}", from.join(", "))
        })
    })
}

#[cfg(test)]
mod tests {
    use catmail_core::FolderRole;

    use super::*;
    use crate::fixtures::{account, folder, gmail, gmail_folder, standard};

    #[test]
    fn a_place_is_named_and_drawn_as_the_sidebar_shows_it() {
        let accounts = [
            standard(1),
            gmail(
                2,
                vec![gmail_folder(21, 2, "INBOX"), gmail_folder(22, 2, "Work")],
            ),
            account(3, vec![folder(31, 3, "Projects/2026")]),
        ];
        let (inbox, sent) = (FolderId(11), FolderId(12));
        #[rustfmt::skip]
        let cases = [
            (Place::Inboxes, "All Inboxes", FolderIcon::Inboxes),
            (Place::Folder(inbox), "Inbox", FolderIcon::Role(FolderRole::Inbox)),
            (Place::Folder(sent), "Sent", FolderIcon::Role(FolderRole::Sent)),
            (Place::Folder(FolderId(22)), "Work", FolderIcon::Label),
            (Place::Folder(FolderId(31)), "2026", FolderIcon::Folder),
            (Place::Drafts(AccountId(1)), "Drafts", FolderIcon::Role(FolderRole::Drafts)),
            (Place::Outbox(AccountId(1)), "Outbox", FolderIcon::Outbox),
        ];
        for (place, label, icon) in cases {
            assert_eq!(place.label(&accounts).as_deref(), Some(label), "{place:?}");
            assert_eq!(place.icon(&accounts), icon, "{place:?}");
        }
        assert_eq!(Place::Folder(FolderId(99)).label(&accounts), None);
        assert_eq!(
            Place::Folder(FolderId(99)).icon(&accounts),
            FolderIcon::Folder
        );
    }
}
