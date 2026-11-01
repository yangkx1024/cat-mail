//! The commands on the sidebar's folders (on Gmail, labels): New, Rename and Delete,
//! from their context menus. The dialog that names a folder, new or renamed, stays open
//! until the engine has made the change on the server, which it does at once, and says
//! why if it couldn't.

use catmail_core::{AccountId, AccountMode, Folder, FolderId, MailClient};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::v_flex;
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Window,
};

use super::{AccountEntry, Mailbox, Place, report_failure};
use crate::layout;
use crate::submission::{self, DialogForm, Submit};

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

/// Opens the dialog titled `title`, its field holding `name`, selected.
fn open(
    client: MailClient,
    naming: Naming,
    title: impl Into<SharedString>,
    name: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| FolderForm::new(client, naming, name, window, cx));
    let (ok, _) = naming.verbs();
    let ids = ("folder-cancel", "folder-ok");
    submission::open_dialog(form.clone(), title.into(), 24., ids, ok, window, cx);
    // Only once the dialog is open: it gives focus back to what had it when it closes.
    form.update(cx, |form, cx| {
        form.name.update(cx, |name, cx| {
            name.focus(window, cx);
            name.select_all(window, cx);
        })
    });
}

struct FolderForm {
    client: MailClient,
    naming: Naming,
    name: Entity<InputState>,
    submit: Submit,
}

impl FolderForm {
    fn new(
        client: MailClient,
        naming: Naming,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            client,
            naming,
            name: cx.new(|cx| InputState::new(window, cx).default_value(name.to_owned())),
            submit: Submit::new(naming.verbs().1),
        }
    }
}

impl DialogForm for FolderForm {
    fn submit(&self) -> &Submit {
        &self.submit
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (client, naming) = (self.client.clone(), self.naming);
        let name = self.name.read(cx).value().to_string();
        let request = Ok(async move {
            match naming {
                Naming::New { account, parent } => {
                    client.create_folder(account, parent, &name).await
                }
                Naming::Rename(folder) => client.rename_folder(folder, &name).await,
            }
        });
        // The sidebar shows it once the mailbox reads its folders again, for the
        // engine's event. Nothing can open over this dialog meanwhile, so it is the one
        // closed.
        self.submit.start(
            |this: &mut Self| &mut this.submit,
            request,
            |_, window, cx| window.close_dialog(cx),
            window,
            cx,
        );
    }
}

impl Render for FolderForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("folder-form")
            .gap_3()
            .child(Input::new(&self.name).id(NAME))
            .children(self.submit.error("folder-error", cx))
    }
}

/// A command a folder's context menu runs on the mailbox.
type FolderCommand = Box<dyn Fn(&mut Mailbox, &mut Window, &mut Context<Mailbox>)>;

/// An item of a folder's context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuEntry {
    /// A new folder in this one, or at the top of the account's.
    New(Option<FolderId>),
    Rename(FolderId),
    Delete(FolderId),
}

impl AccountEntry {
    /// What the context menu of `folder` (`None`: of the account's heading) offers. New
    /// always makes one in what was right-clicked, the top for the heading, and is left
    /// out where the user may not make one ([`can_create_in`](catmail_core::can_create_in));
    /// Rename and Delete only for a folder the user may change.
    pub(super) fn folder_menu_entries(&self, folder: Option<FolderId>) -> Vec<MenuEntry> {
        let Some(id) = folder else {
            return vec![MenuEntry::New(None)];
        };
        let mut entries = Vec::new();
        if self.folders.can_create_in(id) {
            entries.push(MenuEntry::New(Some(id)));
        }
        if self.folders.can_change(id) {
            entries.extend([MenuEntry::Rename(id), MenuEntry::Delete(id)]);
        }
        entries
    }

    /// Whether its folders are Gmail's labels.
    pub(super) fn has_labels(&self) -> bool {
        self.account.mode == Some(AccountMode::Gmail)
    }

    /// What its folders are called.
    fn folder_noun(&self) -> &'static str {
        if self.has_labels() { "label" } else { "folder" }
    }
}

impl Mailbox {
    /// The context menu of a folder in the sidebar (`None`: of the account's heading),
    /// with [`folder_menu_entries`](AccountEntry::folder_menu_entries). Worked out as it
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
            let new = if entry.has_labels() {
                "New label"
            } else {
                "New folder"
            };
            entry
                .folder_menu_entries(folder)
                .into_iter()
                .fold(menu, |menu, menu_entry| {
                    let (menu, label, command): (_, _, FolderCommand) = match menu_entry {
                        MenuEntry::New(parent) => (
                            menu,
                            new,
                            Box::new(move |this, window, cx| {
                                this.new_folder(account, parent, window, cx)
                            }),
                        ),
                        MenuEntry::Rename(id) => (
                            // Left out where it would come first.
                            menu.separator(),
                            "Rename",
                            Box::new(move |this, window, cx| this.rename_folder(id, window, cx)),
                        ),
                        MenuEntry::Delete(id) => (
                            menu,
                            "Delete",
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
    fn changeable_folder(&self, id: FolderId) -> Option<(&AccountEntry, &Folder)> {
        self.accounts
            .iter()
            .flatten()
            .find_map(|entry| Some((entry, entry.folders.get(id)?)))
            .filter(|(entry, _)| entry.folders.can_change(id))
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
        let noun = entry.folder_noun();
        let title = match parent.and_then(|id| entry.folders.get(id)) {
            Some(parent) => format!("New {noun} in “{}”", parent.leaf_name()),
            None => format!("New {noun}"),
        };
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
        let Some((entry, folder)) = self.changeable_folder(id) else {
            return;
        };
        // On Gmail, every folder the user may change is a label.
        let label = entry.has_labels();
        let consequence = match (label, entry.folders.has_subfolders(id)) {
            (true, false) => "Its messages stay in All Mail.",
            (true, true) => "Its nested labels go too. Their messages stay in All Mail.",
            (false, false) => "Its messages are deleted from the server.",
            (false, true) => "Its subfolders and all their messages are deleted from the server.",
        };
        let what = if label {
            "delete the label"
        } else {
            "delete the folder"
        };
        let title = format!("Delete “{}”?", folder.leaf_name());
        let client = self.client.clone();
        layout::confirm_danger(
            title,
            consequence,
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
