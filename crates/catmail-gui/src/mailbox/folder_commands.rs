//! The commands on the sidebar's folders (on Gmail, labels): New, Rename and Delete,
//! from their context menus. The dialog that names a folder, new or renamed, stays open
//! until the engine has made the change on the server, which it does at once, and says
//! why if it couldn't.

use catmail_core::{AccountId, FolderId, MailClient};
use catmail_frontend::mailbox::FolderMenuEntry;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{App, Context, SharedString, Window};

use super::{AccountEntry, Mailbox, Place, report_failure};
use crate::layout;
use crate::submission::{NameDialog, NameIds};

/// The element id of the name field.
pub const NAME: &str = "folder-name";

/// What the name is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Naming {
    /// A new folder in `parent`, or at the top of the account's.
    New {
        account: AccountId,
        parent: Option<FolderId>,
    },
    Rename(FolderId),
}

impl Naming {
    /// The button that submits the name, and what it does, for the error.
    fn verbs(self) -> (&'static str, &'static str) {
        match self {
            Naming::New { .. } => ("Create", "create it"),
            Naming::Rename(_) => ("Rename", "rename it"),
        }
    }
}

/// Opens the dialog titled `title`, its field holding `name`.
fn open(
    client: MailClient,
    naming: Naming,
    title: impl Into<SharedString>,
    name: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let (ok, what) = naming.verbs();
    let dialog = NameDialog {
        ids: NameIds {
            field: NAME,
            cancel: "folder-cancel",
            ok: "folder-ok",
            error: "folder-error",
        },
        title: title.into(),
        name: name.to_owned(),
        placeholder: None,
        ok,
        what,
    };
    let request = move |name: String| {
        let client = client.clone();
        async move {
            match naming {
                Naming::New { account, parent } => {
                    client.create_folder(account, parent, &name).await
                }
                Naming::Rename(folder) => client.rename_folder(folder, &name).await,
            }
        }
    };
    dialog.open(request, window, cx);
}

/// A command a folder's context menu runs on the mailbox.
type FolderCommand = Box<dyn Fn(&mut Mailbox, &mut Window, &mut Context<Mailbox>)>;

impl Mailbox {
    /// The context menu of a folder in the sidebar (`None`: of the account's heading),
    /// with [`AccountEntry::folder_menu_entries`]. Worked out as it
    /// opens; a menu left with nothing doesn't open.
    pub(super) fn folder_menu(
        &self,
        account: AccountId,
        folder: Option<FolderId>,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut App) -> PopupMenu + 'static {
        let mailbox = cx.weak_entity();
        move |menu, _, cx| {
            let Some(this) = mailbox.upgrade() else {
                return menu;
            };
            let Some(entry) = this.read(cx).account(account) else {
                return menu;
            };
            entry
                .folder_menu_entries(folder)
                .into_iter()
                .fold(menu, |menu, menu_entry| {
                    let label = entry.folder_menu_label(menu_entry);
                    let (menu, command): (_, FolderCommand) = match menu_entry {
                        FolderMenuEntry::New(parent) => (
                            menu,
                            Box::new(move |this, window, cx| {
                                this.new_folder(account, parent, window, cx)
                            }),
                        ),
                        FolderMenuEntry::Rename(id) => (
                            // Left out where it would come first.
                            menu.separator(),
                            Box::new(move |this, window, cx| this.rename_folder(id, window, cx)),
                        ),
                        FolderMenuEntry::Delete(id) => (
                            menu,
                            Box::new(move |this, window, cx| this.delete_folder(id, window, cx)),
                        ),
                    };
                    menu.item(
                        PopupMenuItem::new(label)
                            .on_click(layout::after_menu(mailbox.clone(), command)),
                    )
                })
        }
    }

    /// The folder (a level that holds no messages too) and its account, if the user may
    /// change it.
    fn changeable_folder(&self, id: FolderId) -> Option<(&AccountEntry, &catmail_core::Folder)> {
        self.accounts
            .iter()
            .flatten()
            .find_map(|entry| Some((entry, entry.changeable_folder(id)?)))
    }

    /// Asks for the name of a new folder of `account`, in `parent` or at the top.
    pub(super) fn new_folder(
        &mut self,
        account: AccountId,
        parent: Option<FolderId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.account(account) else {
            return;
        };
        let title = entry.new_folder_title(parent);
        let naming = Naming::New { account, parent };
        open(self.client.clone(), naming, title, "", window, cx);
    }

    /// Asks for the folder's new name.
    pub(super) fn rename_folder(
        &mut self,
        id: FolderId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, folder)) = self.changeable_folder(id) else {
            return;
        };
        let name = folder.leaf_name();
        let title = format!("Rename “{name}”");
        open(
            self.client.clone(),
            Naming::Rename(id),
            title,
            name,
            window,
            cx,
        );
    }

    /// Renames the folder the sidebar has selected, if the user may.
    pub(super) fn rename_selected_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Place::Folder(id)) = self.place {
            self.rename_folder(id, window, cx);
        }
    }

    /// Deletes the folder, the folders in it and their messages once the user confirms:
    /// it can't be undone. A Gmail label only comes off its messages.
    pub(super) fn delete_folder(
        &mut self,
        id: FolderId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(deletion) = self
            .accounts
            .iter()
            .flatten()
            .find_map(|entry| entry.folder_deletion(id))
        else {
            return;
        };
        let client = self.client.clone();
        let what = deletion.what;
        layout::confirm_danger(
            deletion.title,
            deletion.consequence,
            "Delete",
            window,
            cx,
            move |window, cx| {
                let client = client.clone();
                report_failure(
                    what,
                    async move { client.delete_folder(id).await },
                    window,
                    cx,
                );
            },
        );
    }
}
