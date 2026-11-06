//! The mailbox's left pane: All Inboxes, each account's places under its address (folders
//! with a role first, the drafts and outbox after the inbox, then the other folders,
//! Gmail's labels in a section of their own), and at the bottom what is wrong with the
//! connection. Its model (the groups, the folds, what a read of the accounts changes) is
//! `catmail_frontend::mailbox::sidebar`'s; the commands in a folder's menu are
//! `folder_commands.rs`'.

use std::cell::RefCell;
use std::rc::Rc;

use catmail_core::{AccountId, FolderId};
use catmail_frontend::mailbox::sidebar::{
    Folds, Group, Heading, Item, Loaded, Selection, SidebarModel, sidebar_groups,
};
use catmail_frontend::mailbox::{self, AccountEntry, Destination, Place, SYNC_ON_OPEN_AFTER};
use nsh_base::theme;
use nsh_core::ContextMenuEvent;
use nsh_core::prelude::*;

use crate::buttons::{Size, icon_button, primary_button, tooltip};
use crate::engine::client;
use crate::icons::{Icon, icon};
use crate::mailbox::folder_commands::{folder_menu, rename_selected};
use crate::tasks::in_view;
use crate::toast::report;
use crate::{chrome, composer, settings};

/// The accounts as the sidebar shows them; `None` until the first read.
#[derive(Clone, Default)]
pub struct Accounts(pub Option<Vec<AccountEntry>>);

impl Accounts {
    /// The accounts shown: none before the first read.
    pub fn list(&self) -> &[AccountEntry] {
        self.0.as_deref().unwrap_or_default()
    }
}

/// Whether there is an account to write from or sync. Tracked.
pub fn has_accounts() -> bool {
    global::<Accounts>().with(|a| !a.list().is_empty())
}

/// Where the sidebar's selection is.
#[derive(Clone, Default)]
pub struct SelectedPlace(pub Option<Place>);

/// `f` with the accounts as the sidebar has them.
pub fn with_accounts<T>(f: impl FnOnce(&[AccountEntry]) -> T) -> T {
    global::<Accounts>().with_untracked(|a| f(a.list()))
}

/// `f` with account `id` as the sidebar has it; `None` while it has no such account.
pub fn with_account<T>(id: AccountId, f: impl FnOnce(&AccountEntry) -> T) -> Option<T> {
    with_accounts(|a| AccountEntry::find(a, id).map(f))
}

/// The reads of the sidebar. Not reactive: nothing shows it.
#[derive(Clone, Default)]
struct Reads(Rc<RefCell<SidebarModel>>);

fn reads() -> Rc<RefCell<SidebarModel>> {
    global::<Reads>().with_untracked(|r| r.0.clone())
}

/// Events were missed, a `DraftsChanged` among them maybe. Call it before reading the
/// sidebar again (`SidebarModel::missed_events`).
pub fn missed_events() {
    reads().borrow_mut().missed_events();
}

// --- Reading ------------------------------------------------------------------------

/// Reads the accounts, their folders, drafts and outbox again (one read at a time; one
/// asked for meanwhile follows it).
pub fn load() {
    let reads = reads();
    let Some(read) = reads.borrow_mut().start_read() else {
        return;
    };
    in_view(async move {
        let loaded = client().overview().await;
        let again = reads.borrow_mut().finish_read();
        match loaded {
            Ok(overviews) => {
                let place = global::<SelectedPlace>().with_untracked(|p| p.0);
                let loaded = global::<Accounts>().with_untracked(|shown| {
                    let status = |id| client().account_status(id);
                    let shown = shown.0.as_deref();
                    reads
                        .borrow_mut()
                        .loaded(read, overviews, status, shown, place)
                });
                if let Some(loaded) = loaded {
                    show(loaded);
                }
            }
            Err(e) => report("Couldn't load folders", e),
        }
        if again {
            load();
        }
    });
}

fn show(loaded: Loaded) {
    global::<Accounts>().set(Accounts(Some(loaded.accounts)));
    crate::mailbox::list::set_labels();
    match loaded.selection {
        Selection::Keep => {}
        Selection::KeepAndRefresh => crate::mailbox::list::refresh(),
        Selection::Go(place) => select(place),
    }
    if let Some(message) = loaded.reveal {
        crate::mailbox::list::reveal(message);
    }
    if loaded.compose {
        composer::new_message();
    }
}

/// Goes to `to` if it is there, once the folders are read: where the main window was
/// when it closed, or what a notification was about, which opens its message once the
/// list has it.
pub fn go_to(to: Destination) {
    let Some((place, message)) = with_accounts(|a| reads().borrow_mut().go_to(to, a)) else {
        return;
    };
    select(Some(place));
    if let Some(message) = message {
        crate::mailbox::list::reveal(message);
    }
}

/// A new message, from the account shown, once the folders are read.
pub fn compose() {
    if reads().borrow_mut().compose() {
        composer::new_message();
    }
}

/// Syncs every account now. The engine's background sync does it: progress and failures
/// show as the accounts' status.
pub fn sync() {
    in_view(async {
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
    let change = reads().borrow_mut().drafts_changed(account);
    in_view(async move {
        let counted = client().count_drafts(account).await;
        if !reads().borrow().is_latest_count(account, change) {
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
    let shown = |a: &Accounts| {
        a.0.iter()
            .flatten()
            .find(|e| e.account.id == account)
            .cloned()
    };
    let Some(mut entry) = accounts.with_untracked(shown) else {
        return;
    };
    change(&mut entry);
    let unchanged = accounts.with_untracked(|a| {
        a.0.iter()
            .flatten()
            .any(|e| e.account.id == account && *e == entry)
    });
    if unchanged {
        return;
    }
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
        global::<Folds>().update(|f| f.set_account_open(account, true));
    }
    selected.set(SelectedPlace(place));
    for folder in shown_folders() {
        in_view(async move {
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
    let Some(place) = place else {
        return Vec::new();
    };
    with_accounts(|accounts| place.folders(accounts).iter().map(|f| f.id).collect())
}

fn toggle_folder(key: &str) {
    global::<Folds>().update(|f| f.toggle_folder(key));
}

// --- The pane -----------------------------------------------------------------------

pub fn sidebar() -> impl IntoElement {
    // `CATMAIL_GUI_UNFOLD` set opens every account, for looking at the places without a
    // mouse (screenshots).
    if std::env::var_os("CATMAIL_GUI_UNFOLD").is_some() {
        global::<Folds>().update(|f| f.unfold_all = true);
    }
    let groups = computed(move || {
        global::<Folds>()
            .with(|folds| global::<Accounts>().with(|a| sidebar_groups(a.list(), folds)))
    });
    let note = computed(move || global::<Accounts>().with(|a| mailbox::connection_note(a.list())));
    on_key("F2", move |_| rename_selected());
    // As wide as the pane it is in, which the user drags.
    view()
        .flex_1()
        .bg(move || theme().sidebar)
        // Under the window's buttons, "Compose" just below their row.
        .child(when(chrome::has_window_buttons, chrome::move_row))
        // 20 down to the first place, with the 12 above it.
        .child(
            view()
                .px(px(12.0))
                .pt(move || {
                    px(if chrome::has_window_buttons() {
                        0.0
                    } else {
                        12.0
                    })
                })
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

/// A group: an account's address 8 below the places above (the first has
/// the section's title over it instead), its places 8 below it.
fn group(group: ReadSignal<Group>, _ix: ReadSignal<usize>) -> AnyElement {
    let headed = group.with_untracked(|g| g.heading.is_some());
    view()
        .child(when(move || headed, move || heading(group)))
        .child(when(
            move || group.with(|g| !g.items.is_empty()),
            move || {
                menu(move || group.with(|g| g.items.to_vec())).pt(px(if headed {
                    8.0
                } else {
                    0.0
                }))
            },
        ))
        .child(when(
            move || group.with(|g| !g.labels.is_empty()),
            move || {
                view()
                    .child(title("Labels"))
                    .child(menu(move || group.with(|g| g.labels.to_vec())))
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
                        let open = !open();
                        global::<Folds>().update(|f| f.set_account_open(account, open));
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
                        move || item.with(|i| i.children.to_vec()),
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

/// An item of a sidebar menu, the mailbox's or the settings': GPUI Kit's. Selected, the icon fills and the icon and label take the
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
