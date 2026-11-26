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
use catmail_frontend::mailbox::{
    self, AccountEntry, Destination, Place, SYNC_ON_OPEN_AFTER, Source,
};
use nsh_base::theme;
use nsh_core::ContextMenuEvent;
use nsh_core::prelude::*;

use crate::accounts::{Accounts, has_accounts, with_accounts};
use crate::buttons::{
    Size, fold_button, icon_button, menu_item, menu_item_background, primary_button, tooltip,
};
use crate::engine::client;
use crate::icons::{Icon, icon};
use crate::mailbox::compose;
use crate::mailbox::folder_commands::{folder_menu, rename_selected};
use crate::mailbox::listing::{self, SelectedPlace};
use crate::motion;
use crate::shell::tasks::in_view;
use crate::toast::report;
use crate::{form, shell::chrome, windows::settings};

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
    listing::set_labels();
    match loaded.selection {
        Selection::Keep => {}
        Selection::KeepAndRefresh => listing::refresh(),
        Selection::Go(place) => select(place),
    }
    if let Some(message) = loaded.reveal {
        listing::reveal(message);
    }
    if loaded.compose {
        compose::new_message();
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
        listing::reveal(message);
    }
}

/// A new message, from the account shown, once the folders are read.
pub fn compose() {
    if reads().borrow_mut().compose() {
        compose::new_message();
    }
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

/// Selects `place`, unfolding its account, and syncs the folders it shows that weren't
/// synced lately: background sync keeps only INBOX current.
pub fn select(place: Option<Place>) {
    let selected = global::<SelectedPlace>();
    if selected.with_untracked(|p| p.0) == place && place.is_some() {
        // Back from a search to the place it was made in: nothing follows the selection,
        // which didn't change.
        if matches!(listing::source(), Some(Source::Search(_))) {
            listing::clear_search();
            listing::show(place.map(Place::source));
        }
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
    form::keys_without_dialog(|| on_key("F2", move |_| rename_selected()));
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
                    compose::new_message,
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
/// the section's title over it instead), its places 8 below it. Folding the account
/// closes them up under the address; what follows moves up with them.
fn group(group: ReadSignal<Group>, _ix: ReadSignal<usize>) -> AnyElement {
    let headed = group.with_untracked(|g| g.heading.is_some());
    let open = move || group.with(|g| g.heading.as_ref().is_none_or(|h| h.open));
    let places = view()
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
        ));
    motion::fold(open)
        .child(when(move || headed, move || heading(group)))
        .content(places)
        .into_element()
}

fn title(label: &'static str) -> View {
    view().h(px(32.0)).px(px(8.0)).justify_center().child(
        text(label)
            .text_xs()
            .text_color(move || theme().sidebar_foreground.opacity(0.7)),
    )
}

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
    let toggle = move || {
        if let Some(account) = account() {
            let open = !open();
            global::<Folds>().update(|f| f.set_account_open(account, open));
        }
    };
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
                .hover(|s| s.bg(menu_item_background(|| false, true)))
                // The whole row folds the account, which has nothing else to do on a
                // press; its chevron is a folder's, in the same lane.
                .on_press(toggle)
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
                    fold_button(
                        open,
                        move || {
                            let email = group
                                .with(|g| g.heading.as_ref().map(|h| h.email.clone()))
                                .unwrap_or_default();
                            if open() {
                                format!("Collapse {email}")
                            } else {
                                format!("Expand {email}")
                            }
                        },
                        Size::Row,
                        toggle,
                    )
                    .mr(px(-4.0)),
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

/// An item and, while it is open, its subfolders: 4 apart in a list with a line down
/// its left, which close up under it as it folds.
fn menu_entry(item: ReadSignal<Item>, _ix: ReadSignal<usize>) -> AnyElement {
    let subfolders = when(
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
    );
    motion::fold(move || item.with(|i| i.open == Some(true)))
        .child(place_row(item))
        .content(subfolders)
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
    // A press selects the folder; a double press folds it too, as its chevron does.
    .on_press(move |press: &PressEvent| {
        if let Some(place) = place() {
            select(Some(place));
        }
        if press.clicks == 2 && item.with_untracked(|i| i.open.is_some()) {
            toggle_folder(&item.with_untracked(|i| i.key.clone()));
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
        || icon(|| Icon::ExclamationTriangle, 12.0).text_color(crate::shell::theme::warning),
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
            fold_button(
                open,
                move || {
                    let label = item.with(|i| i.label.clone());
                    if open() {
                        format!("Collapse {label}")
                    } else {
                        format!("Expand {label}")
                    }
                },
                Size::Row,
                move || toggle_folder(&item.with_untracked(|i| i.key.clone())),
            )
            .mr(px(-4.0))
        },
    ))
}
