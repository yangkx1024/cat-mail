//! The mailbox's left pane, as the GUI's (`catmail-gui`'s `mailbox/sidebar.rs`): All
//! Inboxes, each account's places under its address (folders with a role first, the
//! drafts and outbox after the inbox, then the other folders, Gmail's labels in a section
//! of their own), and at the bottom what is wrong with the connection. Its model is
//! `catmail_frontend`'s, which the GUI uses too.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use catmail_core::{AccountId, FolderId, FolderRole};
use catmail_frontend::folders::{self, FolderIcon, FolderNode, RoleInfo};
use catmail_frontend::mailbox::{
    self, ALL_MAIL_HIDDEN, AccountEntry, Destination, FolderMenuEntry, Place, SYNC_ON_OPEN_AFTER,
};
use catmail_frontend::refresh::{Coalesce, DraftCounts};
use nsh_base::{alert_dialog, theme};
use nsh_core::ContextMenuEvent;
use nsh_core::element::desktop::{MenuItem, popup_menu};
use nsh_core::prelude::*;

use crate::app::{client, in_mailbox, report};
use crate::commands::{Size, icon_button, primary_button, tooltip};
use crate::form::{NameRequest, ask_name, asking_name};
use crate::icons::{Icon, icon};
use crate::{composer, settings};

/// The accounts as the sidebar shows them; `None` until the first read.
#[derive(Clone, Default)]
pub struct Accounts(pub Option<Vec<AccountEntry>>);

/// Where the sidebar's selection is.
#[derive(Clone, Default)]
pub struct SelectedPlace(pub Option<Place>);

/// What waits for the first read of the folders, asked of the mailbox from outside it
/// (`shell::Ask`) before then: where to go instead of where it starts, and a message to
/// write.
#[derive(Clone, Copy, Default)]
struct Waiting {
    to: Option<Destination>,
    compose: bool,
}

/// Whether each account the user folded or unfolded is open; see [`account_open`] for
/// the others.
#[derive(Clone, Default)]
struct Folds(HashMap<AccountId, bool>);

/// The folders the user folded, by row key: their subfolders don't show. Every one is
/// open until then, as in the GUI.
#[derive(Clone, Default)]
struct FolderFolds(HashSet<String>);

/// The reads of the sidebar: one at a time, and which draft count is the latest. Not
/// reactive: nothing shows it.
#[derive(Clone, Default)]
struct Reads(Rc<RefCell<ReadState>>);

#[derive(Default)]
struct ReadState {
    reading: Coalesce,
    draft_counts: DraftCounts,
}

fn reads() -> Rc<RefCell<ReadState>> {
    global::<Reads>().with_untracked(|r| r.0.clone())
}

/// Events were missed, a `DraftsChanged` among them maybe. Call it before reading the
/// sidebar again ([`DraftCounts::missed`]).
pub fn missed_events() {
    reads().borrow_mut().draft_counts.missed();
}

// --- Reading ------------------------------------------------------------------------

/// Reads the accounts, their folders, drafts and outbox again (one read at a time; one
/// asked for meanwhile follows it).
pub fn load() {
    let reads = reads();
    if !reads.borrow_mut().reading.start() {
        return;
    }
    let counts = reads.borrow().draft_counts.clone();
    in_mailbox(async move {
        let loaded = client().overview().await;
        let again = reads.borrow_mut().reading.finish();
        match loaded {
            Ok(overviews) => {
                let shown = global::<Accounts>();
                let accounts = overviews
                    .into_iter()
                    .map(|overview| {
                        let id = overview.account.id;
                        // Not stored: taken now, it is never older than its event.
                        let mut entry = AccountEntry::new(overview, client().account_status(id));
                        if !reads.borrow().draft_counts.read_is_latest(&counts, id)
                            && let Some(drafts) = shown.with_untracked(|a| {
                                Some(a.0.as_ref()?.iter().find(|e| e.account.id == id)?.drafts)
                            })
                        {
                            entry.drafts = drafts;
                        }
                        entry
                    })
                    .collect();
                set_accounts(accounts);
            }
            Err(e) => report("Couldn't load folders", e),
        }
        if again {
            load();
        }
    });
}

fn set_accounts(accounts: Vec<AccountEntry>) {
    let shown = global::<Accounts>();
    if shown.with_untracked(|a| a.0.as_ref() == Some(&accounts)) {
        // A sync's events often bring nothing the sidebar shows.
        return;
    }
    let place = global::<SelectedPlace>().with_untracked(|p| p.0);
    let still_there = place.is_some_and(|place| place.is_in(&accounts));
    let folders_before = shown_folders();
    // The first read: where it was asked to go, if that is there.
    let waiting = if shown.with_untracked(|a| a.0.is_none()) {
        global::<Waiting>().get_untracked()
    } else {
        Waiting::default()
    };
    let going = waiting.to.and_then(|to| to.find(&accounts));
    let start = match going {
        Some((place, _)) => Some(Some(place)),
        None => (!still_there).then(|| Place::start(&accounts)),
    };
    shown.set(Accounts(Some(accounts)));
    crate::list::set_labels();
    match start {
        Some(start) => select(start),
        // The list shows a folder more (All Inboxes, an account's first INBOX): its
        // messages' events may have come first, taken for a folder not shown.
        None if shown_folders() != folders_before => crate::list::refresh(),
        None => {}
    }
    if let Some((_, Some(message))) = going {
        crate::list::reveal(message);
    }
    if waiting.compose {
        composer::new_message();
    }
}

/// Goes to `to` if it is there, once the folders are read: where the main window was
/// when it closed, or what a notification was about, which opens its message once the
/// list has it.
pub fn go_to(to: Destination) {
    if global::<Accounts>().with_untracked(|a| a.0.is_none()) {
        global::<Waiting>().update(|w| w.to = Some(to));
        return;
    }
    let Some((place, message)) = crate::commands::with_accounts(|a| to.find(a)) else {
        return;
    };
    select(Some(place));
    if let Some(message) = message {
        crate::list::reveal(message);
    }
}

/// A new message, from the account shown, once the folders are read.
pub fn compose() {
    if global::<Accounts>().with_untracked(|a| a.0.is_none()) {
        global::<Waiting>().update(|w| w.compose = true);
    } else {
        composer::new_message();
    }
}

/// Syncs every account now. The engine's background sync does it: progress and failures
/// show as the accounts' status.
pub fn sync() {
    in_mailbox(async {
        if let Err(e) = client().sync_now().await {
            report("Couldn't sync", e);
        }
    });
}

/// Whether an account is syncing every folder, as the engine says.
pub fn syncing() -> bool {
    global::<Accounts>().with(|a| a.0.iter().flatten().any(|entry| entry.status.syncing))
}

/// Every pause in typing saves a draft: only the account's draft count can have changed,
/// so only that is read again.
pub fn count_drafts(account: AccountId) {
    let change = reads().borrow_mut().draft_counts.changed(account);
    in_mailbox(async move {
        let counted = client().count_drafts(account).await;
        if !reads().borrow().draft_counts.is_latest(account, change) {
            return; // A later count is on its way.
        }
        match counted {
            Ok(drafts) => update_account(account, |entry| entry.drafts = drafts),
            // No later count would put it right: the sidebar's read counts the drafts
            // too, and says so if it fails as well.
            Err(e) => {
                tracing::warn!(%account, error = %e, "couldn't count the drafts");
                load();
            }
        }
    });
}

/// The account's connection changed.
pub fn status_changed(account: AccountId) {
    let status = client().account_status(account);
    update_account(account, |entry| entry.status = status);
}

fn update_account(account: AccountId, change: impl FnOnce(&mut AccountEntry)) {
    let accounts = global::<Accounts>();
    let mut entry = accounts.with_untracked(|a| {
        a.0.as_ref()?
            .iter()
            .find(|e| e.account.id == account)
            .cloned()
    });
    let Some(entry) = entry.as_mut() else {
        return;
    };
    let before = entry.clone();
    change(entry);
    if *entry == before {
        return;
    }
    let entry = entry.clone();
    accounts.update(|a| {
        if let Some(shown) = a.0.iter_mut().flatten().find(|e| e.account.id == account) {
            *shown = entry;
        }
    });
}

// --- Selecting ----------------------------------------------------------------------

/// Selects `place`, unfolding its account, and syncs the folders it shows that weren't
/// synced lately: background sync keeps only INBOX current.
pub fn select(place: Option<Place>) {
    let selected = global::<SelectedPlace>();
    if selected.with_untracked(|p| p.0) == place && place.is_some() {
        return;
    }
    let account = global::<Accounts>()
        .with_untracked(|a| Some(place?.account_in(a.0.as_deref()?)?.account.id));
    if let Some(account) = account {
        global::<Folds>().update(|f| {
            f.0.insert(account, true);
        });
    }
    selected.set(SelectedPlace(place));
    for folder in shown_folders() {
        in_mailbox(async move {
            let error = match client().sync_folder(folder, SYNC_ON_OPEN_AFTER).await {
                Ok(report) => report.and_then(|r| r.error).map(|e| e.message),
                Err(e) => Some(e.to_string()),
            };
            if let Some(error) = error {
                tracing::warn!(%folder, %error, "couldn't sync the opened folder");
            }
        });
    }
}

/// The folders whose messages the selected place lists: one, or for All Inboxes each
/// account's INBOX; none for the drafts and the outbox. Untracked.
pub fn shown_folders() -> Vec<FolderId> {
    let place = global::<SelectedPlace>().with_untracked(|p| p.0);
    global::<Accounts>().with_untracked(|a| {
        let accounts = a.0.as_deref().unwrap_or_default();
        match place {
            Some(Place::Inboxes) => accounts
                .iter()
                .filter_map(|a| a.folders.with_role(FolderRole::Inbox))
                .map(|f| f.id)
                .collect(),
            Some(Place::Folder(id)) => accounts
                .iter()
                .find_map(|a| a.folders.find(id)?.selectable())
                .map(|f| f.id)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    })
}

/// Whether the sidebar shows the account's places: as the user left it, else only for
/// the only account. `CATMAIL_NSH_UNFOLD` set opens every account, for looking at the
/// places without a mouse (screenshots of the spike).
fn account_open(folds: &Folds, accounts: &[AccountEntry], account: AccountId) -> bool {
    folds
        .0
        .get(&account)
        .copied()
        .unwrap_or_else(|| accounts.len() == 1 || std::env::var_os("CATMAIL_NSH_UNFOLD").is_some())
}

fn toggle_folder(key: &str) {
    global::<FolderFolds>().update(|f| {
        if !f.0.remove(key) {
            f.0.insert(key.to_owned());
        }
    });
}

fn toggle(account: AccountId) {
    let open = global::<Accounts>().with_untracked(|a| {
        let accounts = a.0.as_deref().unwrap_or_default();
        global::<Folds>().with_untracked(|f| account_open(f, accounts, account))
    });
    global::<Folds>().update(|f| {
        f.0.insert(account, !open);
    });
}

// --- Groups -------------------------------------------------------------------------

/// A group of the sidebar, as the GUI's: All Inboxes alone, or an account's address over
/// its places, its labels in a section after them.
#[derive(Debug, Clone, PartialEq)]
struct Group {
    key: String,
    heading: Option<Heading>,
    /// Its places; none while the account is folded.
    items: Vec<Item>,
    /// The account's Gmail labels; none while it is folded.
    labels: Vec<Item>,
}

/// An account's address, which folds and unfolds its places.
#[derive(Debug, Clone, PartialEq)]
struct Heading {
    account: AccountId,
    email: String,
    /// The first account's: the section's title is over it.
    first: bool,
    /// Something about the account to put right. It shows while the account is folded
    /// too: it is about the account, not one of its places.
    note: Option<&'static str>,
    open: bool,
}

/// An item of a sidebar menu: a place, or a level of folders that holds no messages.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    key: String,
    label: String,
    icon: FolderIcon,
    /// What it selects; none for a level that holds no messages.
    place: Option<Place>,
    /// The account its context menu is about (All Inboxes: none).
    account: Option<AccountId>,
    /// The folder its context menu is about.
    folder: Option<FolderId>,
    /// Unread messages, drafts, or what waits in the outbox; nothing for zero.
    count: usize,
    /// The outbox has something the user has to look at.
    attention: bool,
    /// Whether its subfolders show; none for a folder without subfolders.
    open: Option<bool>,
    /// The subfolders that show: none while it is folded.
    children: Vec<Item>,
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
            children: Vec::new(),
        }
    }

    fn place(
        place: Place,
        label: impl Into<String>,
        icon: FolderIcon,
        count: usize,
        attention: bool,
    ) -> Self {
        let key = match place {
            Place::Inboxes => "inboxes".to_owned(),
            Place::Folder(id) => format!("folder-{id}"),
            Place::Drafts(account) => format!("drafts-{account}"),
            Place::Outbox(account) => format!("outbox-{account}"),
        };
        Self {
            place: Some(place),
            count,
            attention,
            ..Self::new(key, label, icon)
        }
    }
}

fn groups(accounts: &[AccountEntry], folds: &Folds, folded: &FolderFolds) -> Vec<Group> {
    let mut groups = Vec::new();
    if Place::Inboxes.is_in(accounts) {
        let inboxes = accounts
            .iter()
            .filter_map(|a| a.folders.with_role(FolderRole::Inbox));
        groups.push(Group {
            key: "inboxes".to_owned(),
            heading: None,
            items: vec![Item::place(
                Place::Inboxes,
                "All Inboxes",
                FolderIcon::Inboxes,
                folders::unread(inboxes) as usize,
                false,
            )],
            labels: Vec::new(),
        });
    }
    for (ix, entry) in accounts.iter().enumerate() {
        let account = entry.account.id;
        let open = account_open(folds, accounts, account);
        let path = format!("{account}");
        groups.push(Group {
            key: format!("account-{account}"),
            heading: Some(Heading {
                account,
                email: entry.account.email.clone(),
                first: ix == 0,
                note: entry.account.all_mail_hidden().then_some(ALL_MAIL_HIDDEN),
                open,
            }),
            items: if open {
                places(entry, folded)
            } else {
                Vec::new()
            },
            labels: if open {
                (entry.folders.labels.iter())
                    .map(|node| folder_item(account, node, &path, folded))
                    .collect()
            } else {
                Vec::new()
            },
        });
    }
    groups
}

/// Folders with a role first, the drafts and outbox after the inbox, then the rest of
/// the folders; not the labels, which have a section of their own.
fn places(entry: &AccountEntry, folded: &FolderFolds) -> Vec<Item> {
    let account = entry.account.id;
    let path = format!("{account}");
    let mut items = Vec::new();
    let mut roles = entry.folders.roles.iter();
    // The inbox, if the account has one (it always has), leads.
    if let Some(inbox) = entry
        .folders
        .roles
        .first()
        .filter(|n| n.role() == Some(FolderRole::Inbox))
    {
        items.push(folder_item(account, inbox, &path, folded));
        roles.next();
    }
    items.push(Item {
        account: Some(account),
        ..Item::place(
            Place::Drafts(account),
            FolderRole::Drafts.label(),
            RoleInfo::of(FolderRole::Drafts).icon,
            entry.drafts,
            false,
        )
    });
    if !entry.outbox.is_empty() {
        items.push(Item {
            account: Some(account),
            ..Item::place(
                Place::Outbox(account),
                "Outbox",
                FolderIcon::Outbox,
                entry.outbox.len(),
                entry.outbox_needs_user(),
            )
        });
    }
    items.extend(
        roles
            .chain(&entry.folders.others)
            .map(|node| folder_item(account, node, &path, folded)),
    );
    items
}

/// The node and the folders in it, unless it is `folded`. `path` names the levels above,
/// for the key of a level the server didn't list.
fn folder_item(account: AccountId, node: &FolderNode, path: &str, folded: &FolderFolds) -> Item {
    let path = format!("{path}/{}", node.label);
    let icon = folders::icon(node.folder.as_ref());
    let mut item = match node.selectable() {
        Some(folder) => Item::place(
            Place::Folder(folder.id),
            &node.label,
            icon,
            node.unread() as usize,
            false,
        ),
        None => Item::new(format!("level-{path}"), &node.label, icon),
    };
    item.account = Some(account);
    item.folder = node.folder.as_ref().map(|f| f.id);
    if !node.children.is_empty() {
        let open = !folded.0.contains(&item.key);
        item.open = Some(open);
        if open {
            item.children = (node.children.iter())
                .map(|child| folder_item(account, child, &path, folded))
                .collect();
        }
    }
    item
}

// --- The pane -----------------------------------------------------------------------

pub fn sidebar() -> impl IntoElement {
    let groups = computed(move || {
        global::<Folds>().with(|folds| {
            global::<FolderFolds>().with(|folded| {
                global::<Accounts>()
                    .with(|a| groups(a.0.as_deref().unwrap_or_default(), folds, folded))
            })
        })
    });
    let note = computed(move || {
        global::<Accounts>().with(|a| mailbox::connection_note(a.0.as_deref().unwrap_or_default()))
    });
    on_key("F2", move |_| rename_selected());
    let has_accounts =
        move || global::<Accounts>().with(|a| a.0.as_ref().is_some_and(|a| !a.is_empty()));
    // As wide as the pane it is in, which the user drags.
    view()
        .flex_1()
        .bg(move || theme().sidebar)
        // 20 down to the first place, with the 12 above it, as the GUI's.
        .child(
            view()
                .px(px(12.0))
                .pt(px(12.0))
                .pb(px(8.0))
                .child(primary_button(
                    Icon::PencilSquare,
                    || "Compose".to_owned(),
                    || format!("Compose ({})", key_label("mod+n")),
                    has_accounts,
                    composer::new_message,
                )),
        )
        .child(
            scroll()
                .flex_1()
                .child(view().px(px(12.0)).py(px(12.0)).child(each(
                    move || groups.get(),
                    |group| group.key.clone(),
                    group,
                ))),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .px(px(6.0))
                .py(px(4.0))
                .child(icon_button(
                    || Icon::Cog6Tooth,
                    || format!("Settings ({})", key_label("mod+,")),
                    || true,
                    Size::Bar,
                    settings::open,
                ))
                .child(
                    text(move || note.get().unwrap_or_default())
                        .number_of_lines(1)
                        .flex_1()
                        .text_xs()
                        .text_color(move || theme().muted_foreground)
                        // All of it, when the line is cut short: every address that can't
                        // send, say.
                        .tooltip(tooltip(move || note.get().unwrap_or_default())),
                ),
        )
}

/// A group, as the GUI's: an account's address 8 below the places above (the first has
/// the section's title over it instead), its places 8 below it.
fn group(group: ReadSignal<Group>, _ix: ReadSignal<usize>) -> AnyElement {
    let headed = group.with_untracked(|g| g.heading.is_some());
    view()
        .child(when(move || headed, move || heading(group)))
        .child(when(
            move || group.with(|g| !g.items.is_empty()),
            move || {
                menu(move || group.with(|g| g.items.clone())).pt(px(if headed { 8.0 } else { 0.0 }))
            },
        ))
        .child(when(
            move || group.with(|g| !g.labels.is_empty()),
            move || {
                view()
                    .child(title("Labels"))
                    .child(menu(move || group.with(|g| g.labels.clone())))
            },
        ))
        .into_element()
}

/// A section's title, as GPUI Kit's `SidebarGroup` draws its label.
fn title(label: &'static str) -> View {
    view().h(px(32.0)).px(px(8.0)).justify_center().child(
        text(label)
            .text_xs()
            .text_color(move || theme().sidebar_foreground.opacity(0.7)),
    )
}

/// GPUI Kit's sidebar menu: its items 8 apart.
fn menu(items: impl Fn() -> Vec<Item> + 'static) -> View {
    view()
        .gap(px(8.0))
        .child(each(items, |item| item.key.clone(), menu_entry))
}

fn heading(group: ReadSignal<Group>) -> View {
    let heading =
        move |f: fn(&Heading) -> bool| move || group.with(|g| g.heading.as_ref().is_some_and(f));
    let account = move || group.with(|g| g.heading.as_ref().map(|h| h.account));
    let note = move || group.with(|g| g.heading.as_ref().and_then(|h| h.note));
    let open = heading(|h| h.open);
    let first = heading(|h| h.first);
    view()
        .pt(move || px(if first() { 0.0 } else { 8.0 }))
        .child(when(first, || title("Accounts")))
        .child(
            view()
                .h(px(32.0))
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .rounded(px(6.0))
                .hover(|s| s.bg(row_background(|| false, true)))
                .on_press(move || {
                    if let Some(account) = account() {
                        toggle(account);
                    }
                })
                .on_context_menu(move |_: &ContextMenuEvent| {
                    if let Some(account) = account() {
                        folder_menu(account, None);
                    }
                })
                .child(
                    text(move || {
                        group
                            .with(|g| g.heading.as_ref().map(|h| h.email.clone()))
                            .unwrap_or_default()
                    })
                    .number_of_lines(1)
                    .flex_1()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(move || theme().sidebar_foreground),
                )
                .child(
                    icon(
                        move || {
                            if open() {
                                Icon::ChevronDown
                            } else {
                                Icon::ChevronRight
                            }
                        },
                        12.0,
                    )
                    .text_color(move || theme().muted_foreground),
                ),
        )
        .child(when(
            move || note().is_some(),
            move || {
                text(move || note().unwrap_or_default())
                    .text_xs()
                    .px(px(8.0))
                    .text_color(move || theme().muted_foreground)
            },
        ))
}

/// An item and, while it is open, its subfolders: in GPUI Kit's submenu, 4 apart in a
/// list with a line down its left.
fn menu_entry(item: ReadSignal<Item>, _ix: ReadSignal<usize>) -> AnyElement {
    view()
        .child(place_row(item))
        .child(when(
            move || item.with(|i| !i.children.is_empty()),
            move || {
                view()
                    .ml(px(14.0))
                    .border_l_1()
                    .border_color(move || theme().sidebar_border)
                    .pl(px(10.0))
                    .py(px(2.0))
                    .gap(px(4.0))
                    .child(each(
                        move || item.with(|i| i.children.clone()),
                        |item| item.key.clone(),
                        menu_entry,
                    ))
            },
        ))
        .into_element()
}

fn place_row(item: ReadSignal<Item>) -> View {
    let place = move || item.with(|i| i.place);
    let selected = move || {
        let place = place();
        place.is_some() && global::<SelectedPlace>().with(|p| p.0) == place
    };
    let open = move || item.with(|i| i.open == Some(true));
    menu_item(
        move || item.with(|i| i.icon).into(),
        move || item.with(|i| i.label.clone()),
        selected,
    )
    .on_press(move || {
        if let Some(place) = place() {
            select(Some(place));
        }
    })
    .on_context_menu(move |_: &ContextMenuEvent| {
        let (account, folder) = item.with(|i| (i.account, i.folder));
        // Drafts, the outbox and All Inboxes aren't folders to make in or change.
        if let (Some(account), Some(folder)) = (account, folder) {
            folder_menu(account, Some(folder));
        }
    })
    .child(when(
        move || item.with(|i| i.attention),
        || icon(|| Icon::ExclamationTriangle, 12.0).text_color(crate::theme::warning),
    ))
    .child(
        text(move || match item.with(|i| i.count) {
            0 => String::new(),
            n => n.to_string(),
        })
        .text_xs()
        .text_color(move || theme().muted_foreground),
    )
    // Its own press: the row's selects the folder, this only folds it (nsh gives a press
    // to the innermost view alone).
    .child(when(
        move || item.with(|i| i.open.is_some()),
        move || {
            icon_button(
                move || {
                    if open() {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    }
                },
                move || {
                    let label = item.with(|i| i.label.clone());
                    if open() {
                        format!("Collapse {label}")
                    } else {
                        format!("Expand {label}")
                    }
                },
                || true,
                Size::Row,
                move || toggle_folder(&item.with_untracked(|i| i.key.clone())),
            )
            .mr(px(-4.0))
        },
    ))
}

/// An item of a sidebar menu, the mailbox's or the settings': GPUI Kit's, filled as the
/// GUI's `layout::sidebar_row`. Selected, the icon fills and the icon and label take the
/// primary color; the label keeps its weight.
pub fn menu_item(
    which: impl Fn() -> Icon + 'static,
    label: impl IntoReactive<SharedString>,
    selected: impl Fn() -> bool + Copy + 'static,
) -> View {
    view()
        .h(px(28.0))
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .bg(row_background(selected, false))
        .hover(move |s| s.bg(row_background(selected, true)))
        // As large as the label's text, as GPUI Kit draws it.
        .child(
            icon(
                move || if selected() { which().solid() } else { which() },
                14.0,
            )
            .text_color(row_color(selected)),
        )
        .child(
            text(label)
                .number_of_lines(1)
                .flex_1()
                .text_sm()
                .text_color(row_color(selected)),
        )
}

/// A sidebar row's background, as GPUI Kit's sidebar draws it: the accent when selected,
/// lighter under the pointer.
fn row_background(
    selected: impl Fn() -> bool + 'static,
    hovered: bool,
) -> impl Fn() -> AdaptiveColor + 'static {
    move || {
        if selected() {
            theme().sidebar_accent
        } else if hovered {
            theme().sidebar_accent.opacity(0.8)
        } else {
            AdaptiveColor::TRANSPARENT
        }
    }
}

fn row_color(selected: impl Fn() -> bool + 'static) -> impl Fn() -> AdaptiveColor + 'static {
    move || {
        if selected() {
            theme().primary
        } else {
            theme().sidebar_foreground
        }
    }
}

// --- Folder commands ----------------------------------------------------------------

/// The context menu of a folder (`None`: of the account's heading), with
/// [`AccountEntry::folder_menu_entries`]. One left with nothing doesn't open.
fn folder_menu(account: AccountId, folder: Option<FolderId>) {
    let items = global::<Accounts>().with_untracked(|a| {
        let entry = a.0.as_ref()?.iter().find(|e| e.account.id == account)?;
        let mut items = Vec::new();
        for menu_entry in entry.folder_menu_entries(folder) {
            // Left out where it would come first.
            if matches!(menu_entry, FolderMenuEntry::Rename(_)) && !items.is_empty() {
                items.push(MenuItem::separator());
            }
            items.push(
                MenuItem::new(entry.folder_menu_label(menu_entry))
                    .on_select(move || run(account, menu_entry)),
            );
        }
        Some(items)
    });
    if let Some(items) = items.filter(|items| !items.is_empty()) {
        // Nothing waits on it closing.
        drop(popup_menu(items));
    }
}

fn run(account: AccountId, entry: FolderMenuEntry) {
    match entry {
        FolderMenuEntry::New(parent) => new_folder(account, parent),
        FolderMenuEntry::Rename(id) => rename_folder(id),
        FolderMenuEntry::Delete(id) => delete_folder(id),
    }
}

/// The account the folder (a level that holds no messages too) is in.
fn with_entry<R>(folder: FolderId, f: impl FnOnce(&AccountEntry) -> Option<R>) -> Option<R> {
    global::<Accounts>().with_untracked(|a| {
        let entry =
            a.0.as_ref()?
                .iter()
                .find(|e| e.folders.get(folder).is_some())?;
        f(entry)
    })
}

fn new_folder(account: AccountId, parent: Option<FolderId>) {
    let title = global::<Accounts>().with_untracked(|a| {
        let entry = a.0.as_ref()?.iter().find(|e| e.account.id == account)?;
        Some(entry.new_folder_title(parent))
    });
    if let Some(title) = title {
        ask_name(NameRequest {
            title,
            name: String::new(),
            placeholder: None,
            ok: "Create",
            what: "create it",
            may_be_empty: false,
            request: Rc::new(move |name| {
                Box::pin(async move {
                    client()
                        .create_folder(account, parent, &name)
                        .await
                        .map(drop)
                })
            }),
        });
    }
}

fn rename_folder(id: FolderId) {
    let name = with_entry(id, |entry| {
        Some(entry.changeable_folder(id)?.leaf_name().to_owned())
    });
    if let Some(name) = name {
        ask_name(NameRequest {
            title: format!("Rename “{name}”"),
            name,
            placeholder: None,
            ok: "Rename",
            what: "rename it",
            may_be_empty: false,
            request: Rc::new(move |name| {
                Box::pin(async move { client().rename_folder(id, &name).await.map(drop) })
            }),
        });
    }
}

/// F2: renames the folder the sidebar has selected, if the user may.
fn rename_selected() {
    if asking_name() {
        return;
    }
    if let Some(Place::Folder(id)) = global::<SelectedPlace>().with_untracked(|p| p.0) {
        rename_folder(id);
    }
}

/// Deletes the folder, the folders in it and their messages once the user confirms: it
/// can't be undone. A Gmail label only comes off its messages.
fn delete_folder(id: FolderId) {
    let Some(deletion) = with_entry(id, |entry| entry.folder_deletion(id)) else {
        return;
    };
    let answer = alert_dialog()
        .title(deletion.title)
        .description(deletion.consequence)
        .cancel("cancel", "Cancel")
        .destructive("delete", "Delete")
        .show();
    in_mailbox(async move {
        if !answer
            .await
            .is_ok_and(|r| r.action.as_deref() == Some("delete"))
        {
            return;
        }
        if let Err(e) = client().delete_folder(id).await {
            report(&format!("Couldn't {}", deletion.what), e);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(label: &str, children: Vec<FolderNode>) -> FolderNode {
        FolderNode {
            label: label.to_owned(),
            folder: None,
            children,
        }
    }

    /// Each item's label and whether it is open, depth first.
    fn labels(folded: &FolderFolds) -> Vec<(String, Option<bool>)> {
        fn walk(item: &Item, out: &mut Vec<(String, Option<bool>)>) {
            out.push((item.label.clone(), item.open));
            for child in &item.children {
                walk(child, out);
            }
        }
        let tree = node("Clients", vec![node("Acme", vec![node("2026", vec![])])]);
        let mut out = Vec::new();
        walk(&folder_item(AccountId(1), &tree, "1", folded), &mut out);
        out
    }

    #[test]
    fn a_folded_folder_hides_its_subfolders() {
        let open = labels(&FolderFolds::default());
        assert_eq!(
            open,
            [
                ("Clients".into(), Some(true)),
                ("Acme".into(), Some(true)),
                ("2026".into(), None),
            ]
        );
        let folded = FolderFolds(HashSet::from(["level-1/Clients/Acme".to_owned()]));
        assert_eq!(
            labels(&folded),
            [("Clients".into(), Some(true)), ("Acme".into(), Some(false))]
        );
    }
}
