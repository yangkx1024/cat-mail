//! The mailbox's left pane: "New message", each account's places, and at the bottom
//! Settings, the connection state and Sync.

use catmail_core::{AccountId, ErrorKind, FolderRole};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::sidebar::{
    Sidebar, SidebarGroup, SidebarItem, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Collapsible, Disableable as _, Icon, Sizable as _, h_flex, v_flex,
};
use gpui_kit::{
    App, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Role, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    div, prelude::FluentBuilder as _,
};

use super::folders::{self, FolderNode, RoleInfo};
use super::{AccountEntry, Mailbox, Place};
use crate::actions::{self, NewMessage, OpenSettings, Sync};
use crate::assets::Hero;

/// Why a Gmail account's labels are folders, and how to have labels again.
const ALL_MAIL_HIDDEN: &str = "Labels show as folders because All Mail is hidden from \
    IMAP. Turn on “Show in IMAP” for it in Gmail’s settings.";

/// The element id of the note under an account's address.
pub(super) fn account_note(account: AccountId) -> ElementId {
    ("account-note", account.0 as u64).into()
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
                        .text_color(cx.theme().warning),
                )
            })
            .child(n.to_string())
            .into_any_element()
    })
}

impl Mailbox {
    pub(crate) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let accounts = self.accounts.as_deref().unwrap_or_default();
        let note = self.connection_note();
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
                    .label("Settings")
                    .tooltip_with_action("Settings", &OpenSettings, Some(actions::MAILBOX))
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
            )
            .child(
                div()
                    .id("connection-note")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    // All of it, when the line is cut short: every address that can't
                    // send, say.
                    .when_some(note.clone(), |this, note| {
                        this.tooltip(move |window, cx| Tooltip::new(note.clone()).build(window, cx))
                    })
                    .children(note),
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
                AccountGroup {
                    account: entry.account.id,
                    email: entry.account.email.clone().into(),
                    note: entry
                        .account
                        .all_mail_hidden()
                        .then_some(ALL_MAIL_HIDDEN.into()),
                    items: self.account_items(entry, cx),
                    labels: entry
                        .folders
                        .labels
                        .iter()
                        .map(|node| self.folder_item(node, cx))
                        .collect(),
                    collapsed: false,
                }
            }))
    }

    /// One line on what is wrong with the connection, if anything: syncing, or the
    /// first account the engine can't get through to (each of its addresses that can't
    /// send).
    fn connection_note(&self) -> Option<SharedString> {
        if self.syncing > 0 {
            return Some("Syncing…".into());
        }
        self.accounts.as_deref()?.iter().find_map(|entry| {
            let email = &entry.account.email;
            let status = &entry.status;
            if let Some(problem) = &status.sync {
                return Some(
                    match problem.error.kind {
                        ErrorKind::Auth => format!("Can't sign in to {email}"),
                        ErrorKind::Network => format!("Can't reach {email}"),
                        _ => format!("Can't sync {email}"),
                    }
                    .into(),
                );
            }
            // Every address whose server is out of reach.
            (!status.send.is_empty()).then(|| {
                let from: Vec<_> = status.send.iter().map(|p| p.email.as_str()).collect();
                format!("Can't send from {}", from.join(", ")).into()
            })
        })
    }

    /// Folders with a role first, the drafts and outbox after the inbox, then the rest
    /// of the folders; not the labels, which have a section of their own.
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
            FolderRole::Drafts.label(),
            RoleInfo::of(FolderRole::Drafts).icon,
            entry.drafts,
            false,
            cx,
        ));
        if !entry.outbox.is_empty() {
            let attention = entry.outbox.iter().any(|e| e.status.needs_user());
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
        let mut item = SidebarMenuItem::new(node.label.clone())
            .icon(Icon::new(folders::icon(node.folder.as_ref())))
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

/// An account's places under its address, its labels in a section after them.
/// `SidebarGroup` would do, but draws its label small and faint, as for a section; the
/// address names whose mail this is, so it is a heading.
#[derive(Clone)]
struct AccountGroup {
    account: AccountId,
    email: SharedString,
    /// Something about the account to put right, under its address.
    note: Option<SharedString>,
    items: Vec<SidebarMenuItem>,
    labels: Vec<SidebarMenuItem>,
    collapsed: bool,
}

impl Collapsible for AccountGroup {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

impl SidebarItem for AccountGroup {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let id = id.into();
        v_flex()
            .when(!self.collapsed, |this| {
                this.child(
                    div()
                        .px_2()
                        .h_8()
                        .flex()
                        .items_center()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().sidebar_foreground)
                        .truncate()
                        .child(self.email),
                )
                .children(self.note.map(|note| {
                    div()
                        .id(account_note(self.account))
                        .role(Role::Note)
                        .test_support()
                        .aria_label(note.clone())
                        .px_2()
                        .pb_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(note)
                }))
            })
            // The id SidebarGroup gives its first child, so the items keep theirs.
            .child(
                SidebarMenu::new()
                    .children(self.items)
                    .collapsed(self.collapsed)
                    .render(format!("{id}-0"), window, cx),
            )
            .when(!self.labels.is_empty(), |this| {
                this.child(
                    SidebarGroup::new("Labels")
                        .child(SidebarMenu::new().children(self.labels))
                        .collapsed(self.collapsed)
                        .render(format!("{id}-labels"), window, cx),
                )
            })
    }
}
