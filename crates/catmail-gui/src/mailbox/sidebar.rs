//! The mailbox's left pane: "New message", each account's places, and at the bottom
//! Settings, the connection state and Sync.

use catmail_core::FolderRole;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex};
use gpui_kit::{
    App, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div,
    prelude::FluentBuilder as _,
};

use super::folders::FolderNode;
use super::message_list::outbox_status;
use super::{AccountEntry, Mailbox, Place};
use crate::actions::{self, NewMessage, OpenSettings, Sync};
use crate::assets::Hero;
use crate::theme::colors::ThemeColors as _;

fn role_icon(role: Option<FolderRole>) -> Hero {
    match role {
        Some(FolderRole::Inbox) => Hero::Inbox,
        Some(FolderRole::Sent) => Hero::PaperAirplane,
        Some(FolderRole::Drafts) => Hero::DocumentText,
        Some(FolderRole::Trash) => Hero::Trash,
        Some(FolderRole::Junk) => Hero::ShieldExclamation,
        Some(FolderRole::Archive) => Hero::ArchiveBox,
        Some(FolderRole::All) => Hero::Envelope,
        Some(FolderRole::Flagged) => Hero::Flag,
        None => Hero::Folder,
    }
}

/// A count at the end of a row; nothing for zero.
fn count(
    n: usize,
    attention: bool,
) -> Option<impl Fn(&mut gpui_kit::Window, &mut App) -> gpui_kit::AnyElement + 'static> {
    (n > 0).then_some(move |_: &mut gpui_kit::Window, cx: &mut App| {
        h_flex()
            .gap_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .when(attention, |this| {
                this.child(
                    Icon::new(Hero::ExclamationTriangle)
                        .xsmall()
                        .text_color(cx.theme().flag()),
                )
            })
            .child(n.to_string())
            .into_any_element()
    })
}

impl Mailbox {
    pub(crate) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let accounts = self.accounts.as_deref().unwrap_or_default();
        let header = Button::new("new-message")
            .outline()
            .w_full()
            .icon(Hero::PencilSquare)
            .label("New message")
            .tooltip_with_action("New message", &NewMessage, Some(actions::MAILBOX))
            .disabled(accounts.is_empty())
            .on_click(cx.listener(|this, _, window, cx| this.new_message(window, cx)));
        let footer = h_flex()
            .w_full()
            .gap_1()
            .child(
                Button::new("settings")
                    .ghost()
                    .small()
                    .icon(Hero::Cog6Tooth)
                    .label("Settings…")
                    .tooltip_with_action("Settings", &OpenSettings, Some(actions::MAILBOX))
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .children(self.connection_note()),
            )
            .child(
                Button::new("sync")
                    .ghost()
                    .small()
                    .icon(Hero::ArrowPath)
                    .loading(self.syncing > 0)
                    .disabled(accounts.is_empty())
                    .tooltip_with_action("Sync now", &Sync, Some(actions::MAILBOX))
                    .on_click(cx.listener(|this, _, window, cx| this.sync(window, cx))),
            );
        Sidebar::new("folders")
            .w_full()
            .header(header)
            .footer(footer)
            .children(accounts.iter().map(|entry| {
                SidebarGroup::new(entry.account.email.clone())
                    .child(SidebarMenu::new().children(self.account_items(entry, cx)))
            }))
    }

    /// One line on what is wrong with the connection, if anything: syncing, or the
    /// first account the engine can't get through to.
    fn connection_note(&self) -> Option<SharedString> {
        if self.syncing > 0 {
            return Some("Syncing…".into());
        }
        self.accounts.as_deref()?.iter().find_map(|entry| {
            let email = &entry.account.email;
            let status = &entry.status;
            match (&status.sync, &status.send) {
                (Some(problem), _) if problem.auth_failed => {
                    Some(format!("Can't sign in to {email}").into())
                }
                (Some(_), _) => Some(format!("Can't reach {email}").into()),
                (None, Some(_)) => Some(format!("Can't send from {email}").into()),
                (None, None) => None,
            }
        })
    }

    /// Folders with a role first, the drafts and outbox after the inbox, then the rest.
    fn account_items(&self, entry: &AccountEntry, cx: &mut Context<Self>) -> Vec<SidebarMenuItem> {
        let account = entry.account.id;
        let mut items: Vec<SidebarMenuItem> = Vec::new();
        let mut roles = entry.folders.roles.iter();
        // The inbox, if the account has one (it always has), leads.
        if let Some(inbox) = entry
            .folders
            .roles
            .first()
            .filter(|n| n.role() == Some(FolderRole::Inbox))
        {
            items.push(self.folder_item(inbox, cx));
            roles.next();
        }
        items.push(self.place_item(
            Place::Drafts(account),
            "Drafts",
            Hero::DocumentText,
            entry.drafts,
            false,
            cx,
        ));
        if !entry.outbox.is_empty() {
            let attention = entry.outbox.iter().any(|e| outbox_status(e).1.is_some());
            items.push(self.place_item(
                Place::Outbox(account),
                "Outbox",
                Hero::ArrowUpTray,
                entry.outbox.len(),
                attention,
                cx,
            ));
        }
        items.extend(roles.map(|node| self.folder_item(node, cx)));
        items.extend(
            entry
                .folders
                .others
                .iter()
                .map(|node| self.folder_item(node, cx)),
        );
        items
    }

    fn place_item(
        &self,
        place: Place,
        label: &'static str,
        icon: Hero,
        n: usize,
        attention: bool,
        cx: &mut Context<Self>,
    ) -> SidebarMenuItem {
        let mut item = SidebarMenuItem::new(label)
            .icon(Icon::new(icon))
            .active(self.place == Some(place))
            .on_click(
                cx.listener(move |this, _, window, cx| this.select_place(Some(place), window, cx)),
            );
        if attention {
            item = item.accessibility_label(format!("{label}, {n}, needs attention"));
        }
        match count(n, attention) {
            Some(suffix) => item.suffix(suffix),
            None => item,
        }
    }

    fn folder_item(&self, node: &FolderNode, cx: &mut Context<Self>) -> SidebarMenuItem {
        let role = node.role();
        let mut item = SidebarMenuItem::new(node.label.clone())
            .icon(Icon::new(role_icon(role)))
            .default_open(true)
            .children(
                node.children
                    .iter()
                    .map(|child| self.folder_item(child, cx)),
            );
        if let Some(folder) = node.selectable() {
            let place = Place::Folder(folder.id);
            let unread = node.unread() as usize;
            item = item.active(self.place == Some(place)).on_click(
                cx.listener(move |this, _, window, cx| this.select_place(Some(place), window, cx)),
            );
            if unread > 0 {
                item = item.accessibility_label(format!("{}, {unread} unread", node.label));
            }
            if let Some(suffix) = count(unread, false) {
                item = item.suffix(suffix);
            }
        }
        item
    }
}
