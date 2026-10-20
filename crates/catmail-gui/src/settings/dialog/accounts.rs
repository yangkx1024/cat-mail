//! The Accounts section: the accounts there are, adding one and removing one.

use catmail_core::Account;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, ElementId, IntoElement, ParentElement as _, Styled as _, Window, div,
};

use crate::assets::Hero;

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
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let client = client.clone();
            let this = this.clone();
            let id = account.id;
            alert
                .title(format!("Remove “{}”?", account.email))
                .description(
                    "Its mail, drafts and outbox are deleted from this computer, and its password \
                     from the keyring. Nothing changes on the server.",
                )
                .confirm()
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let client = client.clone();
                    let this = this.clone();
                    window
                        .spawn(cx, async move |cx| {
                            let removed = client.remove_account(id).await;
                            cx.update(|window, cx| {
                                if let Err(e) = removed {
                                    window.push_notification(
                                        Notification::error(format!(
                                            "Couldn't remove the account: {e}"
                                        )),
                                        cx,
                                    );
                                }
                                this.update(cx, |this, cx| this.load_accounts(window, cx))
                                    .ok();
                            })
                            .ok();
                        })
                        .detach();
                    true
                })
        });
    }

    pub(super) fn render_accounts(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_4()
            .child(section_title(
                "Accounts",
                "Removing an account deletes only what is kept on this computer.",
                cx,
            ))
            .child(v_flex().children(self.accounts.iter().map(|account| {
                let remove = account.clone();
                h_flex()
                    .gap_3()
                    .py_2p5()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Icon::new(Hero::UserCircle).text_color(muted))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().child(account.email.clone()))
                            .child(
                                div().text_xs().text_color(muted).child(format!(
                                    "{} · {}",
                                    account.imap.host, account.smtp.host
                                )),
                            ),
                    )
                    .child(
                        Button::new(ElementId::from(("remove-account", account.id.0 as u64)))
                            .ghost()
                            .small()
                            .label("Remove…")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_remove_account(remove.clone(), window, cx)
                            })),
                    )
            })))
            .child(
                h_flex().child(
                    Button::new("settings-add-account")
                        .outline()
                        .small()
                        .icon(Hero::Plus)
                        .label("Add account…")
                        .on_click(cx.listener(|this, _, window, cx| this.add_account(window, cx))),
                ),
            )
            .into_any_element()
    }
}
