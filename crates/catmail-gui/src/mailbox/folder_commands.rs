//! The commands on a folder, from its menu in the sidebar: a new folder in it, renaming
//! and deleting it.

use std::rc::Rc;

use catmail_core::{AccountId, FolderId};
use catmail_frontend::mailbox::{AccountEntry, FolderMenuEntry, Place};
use nsh_core::element::desktop::{MenuItem, popup_menu};
use nsh_core::prelude::*;

use crate::engine::client;
use crate::form::{NameRequest, ask_name, asking_name, confirmed};
use crate::mailbox::sidebar::{Accounts, SelectedPlace, with_account};
use crate::tasks::in_view;
use crate::toast::report;

/// The context menu of a folder (`None`: of the account's heading), with
/// [`AccountEntry::folder_menu_entries`]. One left with nothing doesn't open.
pub fn folder_menu(account: AccountId, folder: Option<FolderId>) {
    let items = with_account(account, |entry| {
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
        items
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
    let title = with_account(account, |entry| entry.new_folder_title(parent));
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
pub fn rename_selected() {
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
    let answer = confirmed(deletion.confirm);
    in_view(async move {
        if !answer.await {
            return;
        }
        if let Err(e) = client().delete_folder(id).await {
            report(&format!("Couldn't {}", deletion.what), e);
        }
    });
}
