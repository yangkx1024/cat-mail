//! The sidebar apart from how it looks: its groups (All Inboxes, then each account's
//! places under its address, its labels in a section after them), which are folded, and
//! what a read of the accounts changes: the draft counts kept, where the mailbox goes
//! first, whether the list reads again.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use catmail_core::{AccountId, AccountOverview, AccountStatus, FolderId, FolderRole, MessageId};

use super::{ALL_MAIL_HIDDEN, AccountEntry, Destination, Place};
use crate::folders::{self, FolderIcon, FolderNode};
use crate::refresh::{Coalesce, DraftCounts};

// --- Groups -------------------------------------------------------------------------

/// Which accounts and folders are open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folds {
    /// Whether each account the user folded or unfolded is open; see
    /// [`account_open`](Self::account_open) for the others.
    accounts: HashMap<AccountId, bool>,
    /// The folders the user folded, by item key: their subfolders don't show. Every one
    /// is open until then.
    folders: HashSet<String>,
    /// Every account is open until the user folds it, rather than only the only one.
    pub unfold_all: bool,
}

impl Folds {
    /// Whether the sidebar shows the account's places: as the user left it, else only for
    /// the only account (or every account, with [`unfold_all`](Self::unfold_all)).
    pub fn account_open(&self, accounts: &[AccountEntry], account: AccountId) -> bool {
        let unfolded = || accounts.len() == 1 || self.unfold_all;
        self.accounts
            .get(&account)
            .copied()
            .unwrap_or_else(unfolded)
    }

    /// Opens or folds the account: the user pressed its heading, or selected a place of it.
    pub fn set_account_open(&mut self, account: AccountId, open: bool) {
        self.accounts.insert(account, open);
    }

    /// Folds or unfolds the item with `key`.
    pub fn toggle_folder(&mut self, key: &str) {
        if !self.folders.remove(key) {
            self.folders.insert(key.to_owned());
        }
    }
}

/// A group of the sidebar: All Inboxes alone, or an account's address over its places,
/// its labels in a section after them.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub key: String,
    pub heading: Option<Heading>,
    /// Its places; none while the account is folded.
    pub items: Rc<[Item]>,
    /// The account's Gmail labels; none while it is folded.
    pub labels: Rc<[Item]>,
}

/// An account's address, which folds and unfolds its places.
#[derive(Debug, Clone, PartialEq)]
pub struct Heading {
    pub account: AccountId,
    pub email: String,
    /// The first account's: the section's title is over it.
    pub first: bool,
    /// Something about the account to put right. It shows while the account is folded
    /// too: it is about the account, not one of its places.
    pub note: Option<&'static str>,
    pub open: bool,
}

/// An item of a sidebar menu: a place, or a level of folders that holds no messages.
/// Its subtree is shared, so the groups are copied cheaply.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub key: String,
    pub label: String,
    pub icon: FolderIcon,
    /// What it selects; none for a level that holds no messages.
    pub place: Option<Place>,
    /// The account its context menu is about (All Inboxes: none).
    pub account: Option<AccountId>,
    /// The folder its context menu is about.
    pub folder: Option<FolderId>,
    /// Unread messages, drafts, or what waits in the outbox; nothing for zero.
    pub count: usize,
    /// The outbox has something the user has to look at.
    pub attention: bool,
    /// Whether its subfolders show; none for a folder without subfolders.
    pub open: Option<bool>,
    /// The subfolders that show: none while it is folded.
    pub children: Rc<[Item]>,
}

impl Item {
    fn new(key: String, label: impl Into<String>, icon: FolderIcon) -> Self {
        Self {
            key,
            label: label.into(),
            icon,
            place: None,
            account: None,
            folder: None,
            count: 0,
            attention: false,
            open: None,
            children: Rc::default(),
        }
    }

    fn place(place: Place, label: impl Into<String>, icon: FolderIcon, count: usize) -> Self {
        let key = match place {
            Place::Inboxes => "inboxes".to_owned(),
            Place::Folder(id) => format!("folder-{id}"),
            Place::Drafts(account) => format!("drafts-{account}"),
            Place::Outbox(account) => format!("outbox-{account}"),
        };
        Self {
            place: Some(place),
            count,
            ..Self::new(key, label, icon)
        }
    }

    /// A place that isn't a folder of the tree, named and drawn as [`Place`] has it.
    fn of(place: Place, accounts: &[AccountEntry], count: usize) -> Self {
        let label = place.label(accounts).unwrap_or_default();
        Self::place(place, label, place.icon(accounts), count)
    }
}

/// The sidebar's groups, top to bottom.
pub fn sidebar_groups(accounts: &[AccountEntry], folds: &Folds) -> Vec<Group> {
    let mut groups = Vec::new();
    if Place::Inboxes.is_in(accounts) {
        let unread = folders::unread(Place::Inboxes.folders(accounts)) as usize;
        groups.push(Group {
            key: "inboxes".to_owned(),
            heading: None,
            items: Rc::new([Item::of(Place::Inboxes, accounts, unread)]),
            labels: Rc::default(),
        });
    }
    for (ix, entry) in accounts.iter().enumerate() {
        let account = entry.account.id;
        let open = folds.account_open(accounts, account);
        let (items, labels) = if open {
            places(accounts, entry, folds)
        } else {
            Default::default()
        };
        groups.push(Group {
            key: format!("account-{account}"),
            heading: Some(Heading {
                account,
                email: entry.account.email.clone(),
                first: ix == 0,
                note: entry.account.all_mail_hidden().then_some(ALL_MAIL_HIDDEN),
                open,
            }),
            items,
            labels,
        });
    }
    groups
}

/// An account's places, and its labels for a section of their own: folders with a role
/// first, the drafts and outbox after the inbox, then the rest of the folders.
fn places(
    accounts: &[AccountEntry],
    entry: &AccountEntry,
    folds: &Folds,
) -> (Rc<[Item]>, Rc<[Item]>) {
    let account = entry.account.id;
    let path = format!("{account}");
    let item = |node| folder_item(account, node, &path, folds);
    let mut items = Vec::new();
    let mut roles = entry.folders.roles.iter();
    // The inbox, if the account has one (it always has), leads.
    if let Some(inbox) = entry
        .folders
        .roles
        .first()
        .filter(|n| n.role() == Some(FolderRole::Inbox))
    {
        items.push(item(inbox));
        roles.next();
    }
    items.push(Item {
        account: Some(account),
        ..Item::of(Place::Drafts(account), accounts, entry.drafts)
    });
    if !entry.outbox.is_empty() {
        items.push(Item {
            account: Some(account),
            attention: entry.outbox_needs_user(),
            ..Item::of(Place::Outbox(account), accounts, entry.outbox.len())
        });
    }
    items.extend(roles.chain(&entry.folders.others).map(item));
    let labels = entry.folders.labels.iter().map(item).collect();
    (items.into(), labels)
}

/// The node and the folders in it, unless it is folded. `path` names the levels above,
/// for the key of a level the server didn't list.
fn folder_item(account: AccountId, node: &FolderNode, path: &str, folds: &Folds) -> Item {
    let path = format!("{path}/{}", node.label);
    let icon = folders::icon(node.folder.as_ref());
    let mut item = match node.selectable() {
        Some(folder) => Item::place(
            Place::Folder(folder.id),
            &node.label,
            icon,
            node.unread() as usize,
        ),
        None => Item::new(format!("level-{path}"), &node.label, icon),
    };
    item.account = Some(account);
    item.folder = node.folder.as_ref().map(|f| f.id);
    if !node.children.is_empty() {
        let open = !folds.folders.contains(&item.key);
        item.open = Some(open);
        if open {
            item.children = (node.children.iter())
                .map(|child| folder_item(account, child, &path, folds))
                .collect();
        }
    }
    item
}

// --- Reads --------------------------------------------------------------------------

/// The reads of the accounts (`MailClient::overview`) and what waits for the first: one
/// read at a time, which draft count is the latest, and where the mailbox was asked to
/// go before it had the folders.
#[derive(Debug)]
pub struct SidebarModel {
    reading: Coalesce,
    draft_counts: DraftCounts,
    /// Asked of the mailbox from outside it (the tray, a notification) before the first
    /// read: taken by it. `None` once read.
    waiting: Option<Waiting>,
}

impl Default for SidebarModel {
    fn default() -> Self {
        Self {
            reading: Coalesce::default(),
            draft_counts: DraftCounts::default(),
            waiting: Some(Waiting::default()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Waiting {
    to: Option<Destination>,
    compose: bool,
}

/// A read of the accounts on its way, for [`SidebarModel::loaded`].
#[derive(Debug)]
pub struct Read(DraftCounts);

/// What a read of the accounts changes, beyond the accounts shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub accounts: Vec<AccountEntry>,
    pub selection: Selection,
    /// The message to open once the list has it: what a notification was about.
    pub reveal: Option<MessageId>,
    /// A message to write was asked for before the read.
    pub compose: bool,
}

/// What becomes of the sidebar's selection after a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Keep,
    /// The place selected stays, and lists a folder more or less (All Inboxes, an
    /// account's first INBOX): its messages' events may have come first, taken for a
    /// folder not shown, so the list reads again.
    KeepAndRefresh,
    /// Where the mailbox was asked to go, or where it starts, when nothing was selected
    /// yet or the place selected is gone.
    Go(Option<Place>),
}

impl SidebarModel {
    /// Events were missed, a `DraftsChanged` among them maybe. Call it before reading the
    /// accounts again ([`DraftCounts::missed`]).
    pub fn missed_events(&mut self) {
        self.draft_counts.missed();
    }

    /// A read of the accounts to start now, if none is running; if one is, another
    /// follows it ([`finish_read`](Self::finish_read)).
    pub fn start_read(&mut self) -> Option<Read> {
        self.reading
            .start()
            .then(|| Read(self.draft_counts.clone()))
    }

    /// The read is done, however it went: whether to read again.
    pub fn finish_read(&mut self) -> bool {
        self.reading.finish()
    }

    /// `read` came with `overviews`, while the sidebar shows `shown` (`None` before the
    /// first read) with `place` selected; `status` is each account's connection, which
    /// is taken now so it is never older than its event. Nothing changes if the accounts
    /// are as shown: a sync's events often bring nothing the sidebar shows.
    pub fn loaded(
        &mut self,
        read: Read,
        overviews: Vec<AccountOverview>,
        status: impl Fn(AccountId) -> AccountStatus,
        shown: Option<&[AccountEntry]>,
        place: Option<Place>,
    ) -> Option<Loaded> {
        let accounts: Vec<_> = overviews
            .into_iter()
            .map(|overview| {
                let id = overview.account.id;
                let mut entry = AccountEntry::new(overview, status(id));
                // A count started since the read is the one to show.
                if !self.draft_counts.read_is_latest(&read.0, id)
                    && let Some(drafts) = shown
                        .and_then(|a| AccountEntry::find(a, id))
                        .map(|e| e.drafts)
                {
                    entry.drafts = drafts;
                }
                entry
            })
            .collect();
        if shown == Some(&accounts[..]) {
            return None;
        }
        let waiting = self.waiting.take().unwrap_or_default();
        let going = waiting.to.and_then(|to| to.find(&accounts));
        let still_there = place.is_some_and(|place| place.is_in(&accounts));
        let folders = |accounts: &[AccountEntry]| -> Vec<FolderId> {
            let folders = place.map(|p| p.folders(accounts)).unwrap_or_default();
            folders.iter().map(|f| f.id).collect()
        };
        let selection = match going {
            Some((place, _)) => Selection::Go(Some(place)),
            None if !still_there => Selection::Go(Place::start(&accounts)),
            None if folders(shown.unwrap_or_default()) != folders(&accounts) => {
                Selection::KeepAndRefresh
            }
            None => Selection::Keep,
        };
        Some(Loaded {
            selection,
            reveal: going.and_then(|(_, message)| message),
            compose: waiting.compose,
            accounts,
        })
    }

    /// Where to go for `to`, among `shown`, if it is there: where the main window was when
    /// it closed, or what a notification was about, with its message to open. Before the
    /// first read, kept for it instead.
    pub fn go_to(
        &mut self,
        to: Destination,
        shown: &[AccountEntry],
    ) -> Option<(Place, Option<MessageId>)> {
        if let Some(waiting) = &mut self.waiting {
            waiting.to = Some(to);
            return None;
        }
        to.find(shown)
    }

    /// Whether to write a new message now; before the first read, it is written once
    /// the folders are read ([`Loaded::compose`]).
    pub fn compose(&mut self) -> bool {
        match &mut self.waiting {
            Some(waiting) => {
                waiting.compose = true;
                false
            }
            None => true,
        }
    }

    /// Every pause in typing saves a draft: only the account's draft count can have
    /// changed. Returns the change a count started now answers.
    pub fn drafts_changed(&mut self, account: AccountId) -> u64 {
        self.draft_counts.changed(account)
    }

    /// Whether the count started for `change` is still the latest, to show.
    pub fn is_latest_count(&self, account: AccountId, change: u64) -> bool {
        self.draft_counts.is_latest(account, change)
    }
}

#[cfg(test)]
mod tests {
    use catmail_core::OutboxStatus;

    use super::*;
    use crate::fixtures::{
        account, folder, gmail, gmail_folder, outbox_entry, standard_overview, unread,
    };

    /// Each item's label, depth first, indented by its depth, with its count and whether
    /// it is open.
    fn shape(groups: &[Group]) -> Vec<String> {
        fn walk(item: &Item, depth: usize, out: &mut Vec<String>) {
            let mut line = format!("{}{}", "  ".repeat(depth), item.label);
            if item.count > 0 {
                line += &format!(" {}", item.count);
            }
            if item.attention {
                line += " !";
            }
            match item.open {
                Some(true) => line += " -",
                Some(false) => line += " +",
                None => {}
            }
            out.push(line);
            for child in item.children.iter() {
                walk(child, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        for group in groups {
            if let Some(heading) = &group.heading {
                let fold = if heading.open { "-" } else { "+" };
                out.push(format!("{} {fold}", heading.email));
            }
            group.items.iter().for_each(|i| walk(i, 1, &mut out));
            if !group.labels.is_empty() {
                out.push("  Labels".into());
                group.labels.iter().for_each(|i| walk(i, 2, &mut out));
            }
        }
        out
    }

    #[test]
    fn an_account_lists_the_inbox_drafts_and_outbox_first_then_its_folders() {
        let mut entry = account(
            1,
            vec![
                unread(folder(11, 1, "INBOX"), 3),
                folder(12, 1, "Trash"),
                unread(folder(13, 1, "Projects"), 2),
                folder(14, 1, "Projects/2026"),
                folder(15, 1, "Sent"),
                folder(16, 1, "Drafts"),
            ],
        );
        entry.drafts = 2;
        entry.outbox = vec![outbox_entry(1, 1, OutboxStatus::Failed)];
        assert_eq!(
            shape(&sidebar_groups(&[entry.clone()], &Folds::default())),
            [
                "user1@example.com -",
                "  Inbox 3",
                "  Drafts 2",
                "  Outbox 1 !",
                "  Sent",
                "  Trash",
                "  Projects 2 -",
                "    2026",
            ]
        );
        // No outbox while nothing waits; the folded folder hides its subfolder.
        entry.outbox.clear();
        let mut folds = Folds::default();
        folds.toggle_folder("folder-13");
        let groups = sidebar_groups(&[entry], &folds);
        assert_eq!(shape(&groups)[3..], ["  Sent", "  Trash", "  Projects 2 +"]);
        let drafts = &groups[0].items[1];
        assert_eq!(drafts.place, Some(Place::Drafts(AccountId(1))));
        assert_eq!(drafts.icon, FolderIcon::Role(FolderRole::Drafts));
        assert_eq!((drafts.account, drafts.folder), (Some(AccountId(1)), None));
    }

    #[test]
    fn all_inboxes_leads_several_accounts_folded() {
        let first = account(1, vec![unread(folder(11, 1, "INBOX"), 2)]);
        let second = gmail(
            2,
            vec![
                unread(gmail_folder(21, 2, "INBOX"), 5),
                gmail_folder(22, 2, "Work"),
                gmail_folder(23, 2, "Work/Clients"),
            ],
        );
        let accounts = [first, second];
        let mut folds = Folds::default();
        assert_eq!(
            shape(&sidebar_groups(&accounts, &folds)),
            [
                "  All Inboxes 7",
                "user1@example.com +",
                "user2@example.com +"
            ]
        );
        folds.set_account_open(AccountId(2), true);
        assert_eq!(
            shape(&sidebar_groups(&accounts, &folds))[3..],
            [
                "  Inbox 5",
                "  Drafts",
                "  Labels",
                "    Work -",
                "      Clients",
            ]
        );
        assert!(folds.account_open(&accounts, AccountId(2)));
        folds.unfold_all = true;
        assert!(folds.account_open(&accounts, AccountId(1)));
        // One account is open until the user folds it.
        let one = Folds::default();
        assert!(one.account_open(&accounts[..1], AccountId(1)));
    }

    #[test]
    fn a_level_the_server_didnt_list_is_keyed_by_its_path() {
        let entry = account(1, vec![folder(11, 1, "INBOX"), folder(12, 1, "Lists/rust")]);
        let groups = sidebar_groups(&[entry], &Folds::default());
        let lists = &groups[0].items[2];
        assert_eq!(lists.key, "level-1/Lists");
        assert_eq!((lists.place, lists.folder), (None, None));
        assert_eq!(lists.children[0].key, "folder-12");
    }

    /// Account `id`'s INBOX, as `standard_overview` numbers it.
    fn inbox(id: i64) -> Place {
        Place::Folder(FolderId(id * 10 + 1))
    }

    fn load(
        model: &mut SidebarModel,
        overviews: Vec<AccountOverview>,
        shown: Option<&[AccountEntry]>,
        place: Option<Place>,
    ) -> Option<Loaded> {
        let read = model.start_read().expect("no read running");
        let loaded = model.loaded(read, overviews, |_| AccountStatus::default(), shown, place);
        assert!(!model.finish_read());
        loaded
    }

    #[test]
    fn the_first_read_goes_where_the_mailbox_starts_or_was_asked_to() {
        // All Inboxes, with more than one account.
        let mut model = SidebarModel::default();
        let loaded = load(
            &mut model,
            vec![standard_overview(1), standard_overview(2)],
            None,
            None,
        );
        let loaded = loaded.unwrap();
        assert_eq!(loaded.selection, Selection::Go(Some(Place::Inboxes)));
        assert!(!loaded.compose && loaded.reveal.is_none());

        // Asked for before the read: an account's INBOX and a message there, and a new
        // message.
        let mut model = SidebarModel::default();
        let to = Destination::Inbox {
            account: AccountId(2),
            message: Some(MessageId(7)),
        };
        assert_eq!(model.go_to(to, &[]), None);
        assert!(!model.compose());
        let loaded = load(
            &mut model,
            vec![standard_overview(1), standard_overview(2)],
            None,
            None,
        );
        let loaded = loaded.unwrap();
        assert_eq!(loaded.selection, Selection::Go(Some(inbox(2))));
        assert_eq!(loaded.reveal, Some(MessageId(7)));
        assert!(loaded.compose);
        // From now on, at once.
        assert_eq!(
            model.go_to(to, &loaded.accounts),
            Some((inbox(2), Some(MessageId(7))))
        );
        assert!(model.compose());

        // Asked for a place that isn't there: where it starts.
        let mut model = SidebarModel::default();
        model.go_to(Destination::Place(Place::Folder(FolderId(99))), &[]);
        let loaded = load(&mut model, vec![standard_overview(1)], None, None).unwrap();
        assert_eq!(loaded.selection, Selection::Go(Some(inbox(1))));
    }

    #[test]
    fn a_read_again_keeps_the_place_or_starts_over_if_it_is_gone() {
        let mut model = SidebarModel::default();
        let first = load(&mut model, vec![standard_overview(1)], None, None).unwrap();
        let shown = first.accounts;
        let sent = Place::Folder(FolderId(12));
        // The same: nothing to do.
        assert_eq!(
            load(
                &mut model,
                vec![standard_overview(1)],
                Some(&shown),
                Some(sent)
            ),
            None
        );

        // Another folder came: the place stays, the list shows the same.
        let mut more = standard_overview(1);
        more.folders.push(folder(15, 1, "Projects"));
        let loaded = load(&mut model, vec![more], Some(&shown), Some(sent)).unwrap();
        assert_eq!(loaded.selection, Selection::Keep);

        // Sent went away: back to where the mailbox starts.
        let mut fewer = standard_overview(1);
        fewer.folders.truncate(1);
        let loaded = load(&mut model, vec![fewer], Some(&shown), Some(sent)).unwrap();
        assert_eq!(loaded.selection, Selection::Go(Some(inbox(1))));

        // What was asked for before the first read is done with.
        model.go_to(Destination::Place(Place::Folder(FolderId(11))), &shown);
        let loaded = load(&mut model, vec![standard_overview(1)], Some(&[]), None).unwrap();
        assert_eq!(loaded.selection, Selection::Go(Some(inbox(1))));
        assert!(!loaded.compose);
    }

    #[test]
    fn all_inboxes_reads_the_list_again_for_an_inbox_more() {
        let mut model = SidebarModel::default();
        let shown = load(
            &mut model,
            vec![standard_overview(1), standard_overview(2)],
            None,
            None,
        )
        .unwrap()
        .accounts;
        let three = vec![
            standard_overview(1),
            standard_overview(2),
            standard_overview(3),
        ];
        let loaded = load(&mut model, three, Some(&shown), Some(Place::Inboxes)).unwrap();
        assert_eq!(loaded.selection, Selection::KeepAndRefresh);
    }

    #[test]
    fn a_draft_count_started_after_the_read_is_kept() {
        let mut model = SidebarModel::default();
        let mut shown = load(&mut model, vec![standard_overview(1)], None, None)
            .unwrap()
            .accounts;
        shown[0].drafts = 4;
        let read = model.start_read().unwrap();
        // Typing saved a draft meanwhile: its count is on its way, newer than the read's.
        let change = model.drafts_changed(AccountId(1));
        let mut overview = standard_overview(1);
        overview.drafts = 3;
        let loaded = model.loaded(
            read,
            vec![overview.clone()],
            |_| AccountStatus::default(),
            Some(&shown),
            None,
        );
        assert_eq!(loaded, None, "the count shown stays");
        assert!(!model.finish_read());
        assert!(model.is_latest_count(AccountId(1), change));

        // A read started after it has the latest count.
        let loaded = load(&mut model, vec![overview], Some(&shown), None).unwrap();
        assert_eq!(loaded.accounts[0].drafts, 3);
    }

    #[test]
    fn a_read_asked_for_meanwhile_follows() {
        let mut model = SidebarModel::default();
        let _read = model.start_read().unwrap();
        assert!(model.start_read().is_none());
        assert!(model.finish_read());
        assert!(model.start_read().is_some());
    }
}
