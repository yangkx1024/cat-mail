//! The Accounts section: the accounts there are, adding one and removing one, and the
//! other addresses each sends as.

use catmail_core::{AccountId, IdentityId};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, ElementId, IntoElement, ParentElement as _, Styled as _, Window, div,
};

use crate::assets::Hero;
use crate::layout;
use crate::mailbox::AccountEntry;

use super::*;

impl SettingsView {
    pub(super) fn add_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        self.mailbox
            .update(cx, |mailbox, cx| mailbox.add_account(true, window, cx))
            .ok();
    }

    /// The account as the mailbox has it, unless it is gone.
    fn account<'a>(&self, id: AccountId, cx: &'a App) -> Option<&'a AccountEntry> {
        self.mailbox.upgrade()?.read(cx).account(id)
    }

    pub(super) fn confirm_remove_account(
        &mut self,
        id: AccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.account(id, cx) else {
            return;
        };
        let client = self.client.clone();
        layout::confirm_danger(
            format!("Remove “{}”?", entry.account.email),
            "Its mail, drafts and outbox are deleted from this computer, and its password from \
             the keyring. Nothing changes on the server.",
            "Remove",
            window,
            cx,
            move |window, cx| {
                let client = client.clone();
                crate::mailbox::report_failure(
                    "remove the account",
                    async move { client.remove_account(id).await },
                    window,
                    cx,
                );
            },
        );
    }

    fn add_identity(&mut self, id: AccountId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.account(id, cx) else {
            return;
        };
        let (email, name) = (
            entry.account.email.clone(),
            entry.account.display_name.clone(),
        );
        identity::open(self.client.clone(), id, &email, name, window, cx);
    }

    fn make_default(&mut self, id: IdentityId, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.clone();
        crate::mailbox::report_failure(
            "make the address the default",
            async move { client.set_default_identity(id).await },
            window,
            cx,
        );
    }

    /// Whether its server saves what it sends to Sent itself, as the user says now: the
    /// guess is not made again.
    fn set_saves_sent(
        &mut self,
        id: IdentityId,
        saves: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.client.clone();
        crate::mailbox::report_failure(
            "change how sent mail is saved",
            async move { client.set_identity_saves_sent(id, Some(saves)).await },
            window,
            cx,
        );
    }

    fn confirm_remove_identity(
        &mut self,
        account: AccountId,
        id: IdentityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.account(account, cx) else {
            return;
        };
        let Some(identity) = entry.identities.iter().find(|i| i.id == id) else {
            return;
        };
        // The default is listed first.
        let default = entry
            .identities
            .first()
            .map_or(&entry.account.email, |i| &i.email);
        let client = self.client.clone();
        layout::confirm_danger(
            format!("Remove “{}”?", identity.email),
            format!(
                "Drafts from it will be sent from {default} instead, and its password is \
                 deleted from the keyring."
            ),
            "Remove",
            window,
            cx,
            move |window, cx| {
                let client = client.clone();
                crate::mailbox::report_failure(
                    "remove the address",
                    async move { client.remove_identity(id).await },
                    window,
                    cx,
                );
            },
        );
    }

    pub(super) fn render_accounts(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        // Borrowed for as long as the rows are built: the handlers keep only ids, and
        // look the rest up when clicked.
        let mailbox = self.mailbox.upgrade();
        let accounts = mailbox
            .as_ref()
            .and_then(|mailbox| mailbox.read(cx).accounts.as_deref())
            .unwrap_or_default();
        v_flex()
            .gap_4()
            .child(section_title(
                "Accounts",
                "Removing an account deletes only what is kept on this computer.",
                cx,
            ))
            .child(v_flex().children(accounts.iter().map(|entry| {
                let account = &entry.account;
                let account_id = account.id;
                let identity_rows = entry.identities.iter().map(|identity| {
                    let id = identity.id;
                    let actions = if identity.is_default {
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Default")
                            .into_any_element()
                    } else {
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new(ElementId::from(("default-identity", id.0 as u64)))
                                    .ghost()
                                    .small()
                                    .label("Make default")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.make_default(id, window, cx)
                                    })),
                            )
                            .child(
                                Button::new(ElementId::from(("remove-identity", id.0 as u64)))
                                    .ghost()
                                    .small()
                                    .label("Remove")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.confirm_remove_identity(account_id, id, window, cx)
                                    })),
                            )
                            .into_any_element()
                    };
                    h_flex()
                        .gap_3()
                        .py_1p5()
                        // Under the account's email, past its icon.
                        .pl_8()
                        .child(Icon::new(Hero::PaperAirplane).text_color(muted))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().child(identity.email.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(identity.smtp.host.clone()),
                                )
                                .child(
                                    // Gmail's does: a copy saved too would be a second one.
                                    Switch::new(ElementId::from(("saves-sent", id.0 as u64)))
                                        .small()
                                        .pt_1()
                                        .checked(identity.server_saves_sent(account))
                                        .label("Server saves to Sent")
                                        .on_change(cx.listener(
                                            move |this, checked: &bool, window, cx| {
                                                this.set_saves_sent(id, *checked, window, cx)
                                            },
                                        )),
                                ),
                        )
                        .child(actions)
                });
                v_flex()
                    .py_2p5()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_3()
                            .child(Icon::new(Hero::UserCircle).text_color(muted))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().child(account.email.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(account.imap.host.clone()),
                                    ),
                            )
                            .child(
                                Button::new(ElementId::from((
                                    "remove-account",
                                    account_id.0 as u64,
                                )))
                                .ghost()
                                .small()
                                .label("Remove")
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.confirm_remove_account(account_id, window, cx)
                                    },
                                )),
                            ),
                    )
                    .children(identity_rows)
                    .child(
                        h_flex().pl_8().pt_1().child(
                            Button::new(ElementId::from(("add-identity", account_id.0 as u64)))
                                .ghost()
                                .small()
                                .icon(Hero::Plus)
                                .label("Add sending address")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.add_identity(account_id, window, cx)
                                })),
                        ),
                    )
            })))
            .child(
                h_flex().child(
                    Button::new("settings-add-account")
                        .outline()
                        .small()
                        .icon(Hero::Plus)
                        .label("Add account")
                        .on_click(cx.listener(|this, _, window, cx| this.add_account(window, cx))),
                ),
            )
            .into_any_element()
    }
}
