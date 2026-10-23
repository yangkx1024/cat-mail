//! The Accounts section: the accounts there are, adding one and removing one, and the
//! other addresses each sends as.

use catmail_core::{Account, Identity, IdentityId};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, ElementId, IntoElement, ParentElement as _, Styled as _, Window, div,
};

use crate::assets::Hero;
use crate::layout;

use super::identity::{AddIdentity, IdentityFormClosed};
use super::*;

impl SettingsView {
    pub(super) fn add_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        self.mailbox
            .update(cx, |mailbox, cx| mailbox.add_account(true, window, cx))
            .ok();
    }

    pub(super) fn confirm_remove_account(
        &mut self,
        account: Account,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.client.clone();
        let id = account.id;
        layout::confirm_danger(
            format!("Remove “{}”?", account.email),
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

    fn add_identity(&mut self, account: Account, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.new(|cx| AddIdentity::new(self.client.clone(), account, window, cx));
        let subscription = cx.subscribe(&form, |this, _, _: &IdentityFormClosed, cx| {
            // Added, it is listed with the account once the mailbox reads it again.
            this.identity_form = None;
            cx.notify();
        });
        self.identity_form = Some((form, subscription));
        cx.notify();
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

    /// `email` is the identity's, `default` its account's default's.
    fn confirm_remove_identity(
        &mut self,
        id: IdentityId,
        email: &str,
        default: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.client.clone();
        layout::confirm_danger(
            format!("Remove “{email}”?"),
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
        if let Some((form, _)) = &self.identity_form {
            return form.clone().into_any_element();
        }
        let muted = cx.theme().muted_foreground;
        let accounts: Vec<(Account, Vec<Identity>)> = self
            .mailbox
            .upgrade()
            .and_then(|mailbox| {
                let entries = mailbox.read(cx).accounts.as_ref()?;
                Some(
                    entries
                        .iter()
                        .map(|e| (e.account.clone(), e.identities.clone()))
                        .collect(),
                )
            })
            .unwrap_or_default();
        v_flex()
            .gap_4()
            .child(section_title(
                "Accounts",
                "Removing an account deletes only what is kept on this computer.",
                cx,
            ))
            .child(
                v_flex().children(accounts.iter().map(|(account, identities)| {
                    let remove = account.clone();
                    let add = account.clone();
                    let default: SharedString = identities
                        .iter()
                        .find(|i| i.is_default)
                        .map_or_else(|| account.email.clone(), |i| i.email.clone())
                        .into();
                    let identity_rows = identities.iter().map(|identity| {
                        let id = identity.id;
                        let emails: (SharedString, SharedString) =
                            (identity.email.clone().into(), default.clone());
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
                                            let (email, default) = &emails;
                                            this.confirm_remove_identity(
                                                id, email, default, window, cx,
                                            )
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
                                        account.id.0 as u64,
                                    )))
                                    .ghost()
                                    .small()
                                    .label("Remove")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.confirm_remove_account(remove.clone(), window, cx)
                                    })),
                                ),
                        )
                        .children(identity_rows)
                        .child(
                            h_flex().pl_8().pt_1().child(
                                Button::new(ElementId::from(("add-identity", add.id.0 as u64)))
                                    .ghost()
                                    .small()
                                    .icon(Hero::Plus)
                                    .label("Add sending address")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.add_identity(add.clone(), window, cx)
                                    })),
                            ),
                        )
                })),
            )
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
