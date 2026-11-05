//! The mailbox's left pane, as the GUI's (`catmail-gui`'s `mailbox/sidebar.rs`): All
//! Inboxes, each account's places under its address (folders with a role first, the
//! drafts and outbox after the inbox, then the other folders, Gmail's labels in a section
//! of their own), and at the bottom what is wrong with the connection. Its model is
//! `catmail_frontend`'s, which the GUI uses too.

use std::cell::RefCell;
use std::collections::HashMap;
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

fn toggle(account: AccountId) {
    let open = global::<Accounts>().with_untracked(|a| {
        let accounts = a.0.as_deref().unwrap_or_default();
        global::<Folds>().with_untracked(|f| account_open(f, accounts, account))
    });
    global::<Folds>().update(|f| {
        f.0.insert(account, !open);
    });
}

// --- Rows ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A section's title: "Accounts", "Labels".
    Title,
    /// An account's address, which folds and unfolds its places.
    Account,
    /// Something about the account to put right.
    Note,
    Place,
}

/// One line of the sidebar, flattened: nesting is its depth.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    key: String,
    kind: Kind,
    label: String,
    icon: FolderIcon,
    depth: usize,
    /// What it selects; none for a level that holds no messages.
    place: Option<Place>,
    account: Option<AccountId>,
    /// The folder its context menu is about (an account's heading: none).
    folder: Option<FolderId>,
    /// Unread messages, drafts, or what waits in the outbox; nothing for zero.
    count: usize,
    /// The outbox has something the user has to look at.
    attention: bool,
    /// An account's places show.
    open: bool,
}

impl Row {
    fn new(key: String, kind: Kind, label: impl Into<String>) -> Self {
        Self {
            key,
            kind,
            label: label.into(),
            icon: FolderIcon::Folder,
            depth: 0,
            place: None,
            account: None,
            folder: None,
            count: 0,
            attention: false,
            open: false,
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
            icon,
            place: Some(place),
            count,
            attention,
            ..Self::new(key, Kind::Place, label)
        }
    }
}

fn rows(accounts: &[AccountEntry], folds: &Folds) -> Vec<Row> {
    let mut rows = Vec::new();
    if Place::Inboxes.is_in(accounts) {
        let inboxes = accounts
            .iter()
            .filter_map(|a| a.folders.with_role(FolderRole::Inbox));
        rows.push(Row::place(
            Place::Inboxes,
            "All Inboxes",
            FolderIcon::Inboxes,
            folders::unread(inboxes) as usize,
            false,
        ));
    }
    for (ix, entry) in accounts.iter().enumerate() {
        let id = entry.account.id;
        if ix == 0 {
            rows.push(Row::new("title-accounts".into(), Kind::Title, "Accounts"));
        }
        let open = account_open(folds, accounts, id);
        rows.push(Row {
            account: Some(id),
            open,
            ..Row::new(format!("account-{id}"), Kind::Account, &entry.account.email)
        });
        // Folded too: it is about the account, not one of its places.
        if entry.account.all_mail_hidden() {
            rows.push(Row::new(format!("note-{id}"), Kind::Note, ALL_MAIL_HIDDEN));
        }
        if !open {
            continue;
        }
        account_rows(entry, &mut rows);
        if !entry.folders.labels.is_empty() {
            rows.push(Row::new(format!("labels-{id}"), Kind::Title, "Labels"));
            for node in &entry.folders.labels {
                folder_rows(id, node, 0, &format!("{id}"), &mut rows);
            }
        }
    }
    rows
}

/// Folders with a role first, the drafts and outbox after the inbox, then the rest of
/// the folders; not the labels, which have a section of their own.
fn account_rows(entry: &AccountEntry, rows: &mut Vec<Row>) {
    let account = entry.account.id;
    let path = format!("{account}");
    let mut roles = entry.folders.roles.iter();
    // The inbox, if the account has one (it always has), leads.
    if let Some(inbox) = entry
        .folders
        .roles
        .first()
        .filter(|n| n.role() == Some(FolderRole::Inbox))
    {
        folder_rows(account, inbox, 0, &path, rows);
        roles.next();
    }
    rows.push(Row {
        account: Some(account),
        ..Row::place(
            Place::Drafts(account),
            FolderRole::Drafts.label(),
            RoleInfo::of(FolderRole::Drafts).icon,
            entry.drafts,
            false,
        )
    });
    if !entry.outbox.is_empty() {
        rows.push(Row {
            account: Some(account),
            ..Row::place(
                Place::Outbox(account),
                "Outbox",
                FolderIcon::Outbox,
                entry.outbox.len(),
                entry.outbox_needs_user(),
            )
        });
    }
    for node in roles.chain(&entry.folders.others) {
        folder_rows(account, node, 0, &path, rows);
    }
}

/// The node and the folders in it. `path` names the levels above, for the key of a
/// level the server didn't list.
fn folder_rows(
    account: AccountId,
    node: &FolderNode,
    depth: usize,
    path: &str,
    rows: &mut Vec<Row>,
) {
    let path = format!("{path}/{}", node.label);
    let icon = folders::icon(node.folder.as_ref());
    let mut row = match node.selectable() {
        Some(folder) => Row::place(
            Place::Folder(folder.id),
            &node.label,
            icon,
            node.unread() as usize,
            false,
        ),
        None => Row {
            icon,
            ..Row::new(format!("level-{path}"), Kind::Place, &node.label)
        },
    };
    row.depth = depth;
    row.account = Some(account);
    row.folder = node.folder.as_ref().map(|f| f.id);
    rows.push(row);
    for child in &node.children {
        folder_rows(account, child, depth + 1, &path, rows);
    }
}

// --- The pane -----------------------------------------------------------------------

pub fn sidebar() -> impl IntoElement {
    let rows = computed(move || {
        let folds = global::<Folds>().get();
        global::<Accounts>().with(|a| rows(a.0.as_deref().unwrap_or_default(), &folds))
    });
    let note = computed(move || {
        global::<Accounts>().with(|a| mailbox::connection_note(a.0.as_deref().unwrap_or_default()))
    });
    on_key("F2", move |_| rename_selected());
    let has_accounts =
        move || global::<Accounts>().with(|a| a.0.as_ref().is_some_and(|a| !a.is_empty()));
    view()
        .w(px(240.0))
        .border_r_1()
        .border_color(move || theme().border)
        .bg(move || theme().sidebar)
        .child(view().px(px(12.0)).pt(px(12.0)).child(primary_button(
            Icon::PencilSquare,
            || "Compose".to_owned(),
            || "Compose (Ctrl+N)".to_owned(),
            has_accounts,
            composer::new_message,
        )))
        .child(
            scroll()
                .flex_1()
                .child(view().px(px(8.0)).py(px(12.0)).child(each(
                    move || rows.get(),
                    |row| row.key.clone(),
                    row,
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
                    || "Settings (Ctrl+,)".to_owned(),
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

fn row(row: ReadSignal<Row>, _ix: ReadSignal<usize>) -> AnyElement {
    match row.with_untracked(|r| r.kind) {
        Kind::Title => text(move || row.with(|r| r.label.clone()))
            .text_xs()
            .px(px(8.0))
            .pt(px(12.0))
            .pb(px(4.0))
            .text_color(move || theme().muted_foreground)
            .into_element(),
        Kind::Note => text(move || row.with(|r| r.label.clone()))
            .text_xs()
            .px(px(8.0))
            .pb(px(4.0))
            .text_color(move || theme().muted_foreground)
            .into_element(),
        Kind::Account => account_row(row).into_element(),
        Kind::Place => place_row(row).into_element(),
    }
}

fn account_row(row: ReadSignal<Row>) -> impl IntoElement {
    let account = move || row.with(|r| r.account);
    view()
        .h(px(32.0))
        .mt(px(4.0))
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(8.0))
        .rounded(px(6.0))
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
            text(move || row.with(|r| r.label.clone()))
                .number_of_lines(1)
                .flex_1()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(move || theme().sidebar_foreground),
        )
        .child(
            icon(
                move || {
                    if row.with(|r| r.open) {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    }
                },
                12.0,
            )
            .text_color(move || theme().muted_foreground),
        )
}

fn place_row(row: ReadSignal<Row>) -> impl IntoElement {
    let place = move || row.with(|r| r.place);
    let selected = move || {
        let place = place();
        place.is_some() && global::<SelectedPlace>().with(|p| p.0) == place
    };
    view()
        .h(px(30.0))
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .pr(px(8.0))
        .pl(move || px(8.0 + row.with(|r| r.depth) as f32 * 14.0))
        .rounded(px(6.0))
        .style_when(selected, |s| s.bg(move || theme().sidebar_accent))
        .on_press(move || {
            if let Some(place) = place() {
                select(Some(place));
            }
        })
        .on_context_menu(move |_: &ContextMenuEvent| {
            let (account, folder) = row.with(|r| (r.account, r.folder));
            // Drafts, the outbox and All Inboxes aren't folders to make in or change.
            if let (Some(account), Some(folder)) = (account, folder) {
                folder_menu(account, Some(folder));
            }
        })
        .child(
            icon(move || row.with(|r| r.icon).into(), 16.0)
                .text_color(move || theme().sidebar_foreground),
        )
        .child(
            text(move || row.with(|r| r.label.clone()))
                .number_of_lines(1)
                .flex_1()
                .text_sm()
                .text_color(move || theme().sidebar_foreground),
        )
        .child(when(
            move || row.with(|r| r.attention),
            || icon(|| Icon::ExclamationTriangle, 12.0).text_color(crate::theme::warning),
        ))
        .child(
            text(move || match row.with(|r| r.count) {
                0 => String::new(),
                n => n.to_string(),
            })
            .text_xs()
            .text_color(move || theme().muted_foreground),
        )
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
