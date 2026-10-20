//! The right pane for an outbox entry: what happened to it, and the choices it leaves.

use catmail_core::{OutboxEntry, OutboxStatus};
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window, div};

use super::message_list::outbox_status;
use super::{Mailbox, report_failure};
use crate::format;
use crate::layout;

impl Mailbox {
    pub(super) fn render_outbox_entry(
        &self,
        entry: &OutboxEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let from = self
            .account(entry.account_id)
            .map(|a| a.account.email.clone())
            .unwrap_or_default();
        let facts = [
            ("To", entry.recipients.join(", ")),
            ("From", from),
            ("Queued", format::date(Some(entry.created_at))),
        ]
        .into_iter()
        .map(|(name, value)| layout::labelled(name, value, cx).text_sm())
        .collect::<Vec<_>>();
        v_flex().size_full().child(layout::toolbar(cx)).child(
            layout::page(
                "outbox-entry",
                v_flex()
                    .gap_4()
                    .child(layout::page_title(format::subject(Some(&entry.subject))))
                    .child(v_flex().gap_1().children(facts))
                    .child(self.render_outbox_state(entry, cx)),
            )
            .flex_1()
            .min_h_0(),
        )
    }

    /// What happened to the entry, and the choices it leaves.
    fn render_outbox_state(&self, entry: &OutboxEntry, cx: &mut Context<Self>) -> AnyElement {
        let id = entry.id;
        let account = entry.account_id;
        let back_to_drafts = || {
            Button::new("back-to-drafts")
                .ghost()
                .label("Back to drafts")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.outbox_command(OutboxCommand::Cancel(id), window, cx)
                }))
        };
        let reason = entry.last_error.clone().unwrap_or_default();
        // Why it is where it is, under the state's own words.
        let note = (!reason.is_empty()).then(|| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(reason.clone())
        });
        let (status, _) = outbox_status(entry);
        let actions = h_flex().gap_2().flex_wrap();
        match entry.status {
            OutboxStatus::Unknown => v_flex()
                .gap_3()
                .child(
                    Alert::warning(
                        "unknown",
                        "The connection dropped after the message went to the server, before \
                         it confirmed it, so it may have arrived. It won't be sent again on its \
                         own: check with a recipient, then choose.",
                    )
                    .title("Delivery not confirmed"),
                )
                .child(
                    actions
                        .child(
                            Button::new("mark-sent")
                                .outline()
                                .label("Mark as sent")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.outbox_command(OutboxCommand::MarkSent(id), window, cx)
                                })),
                        )
                        .child(
                            Button::new("send-again")
                                .outline()
                                .label("Send again…")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_send_again(id, window, cx)
                                })),
                        )
                        .child(back_to_drafts()),
                )
                .into_any_element(),
            OutboxStatus::Failed => v_flex()
                .gap_3()
                .child(Alert::error("failed", reason).title("The server rejected this message"))
                .child(
                    actions.child(back_to_drafts().outline()).child(
                        Button::new("retry")
                            .ghost()
                            .label("Try again")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.outbox_command(OutboxCommand::Retry(id), window, cx)
                            })),
                    ),
                )
                .into_any_element(),
            OutboxStatus::Accepted => v_flex()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .child("Sent. A copy is still to be saved to the Sent folder."),
                )
                .children(note)
                .child(
                    actions
                        .child(
                            Button::new("retry")
                                .outline()
                                .label("Save the copy now")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.outbox_command(OutboxCommand::Retry(id), window, cx)
                                })),
                        )
                        .child(
                            Button::new("skip-copy")
                                .ghost()
                                .label("Don't keep a copy")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.outbox_command(OutboxCommand::Cancel(id), window, cx)
                                })),
                        ),
                )
                .into_any_element(),
            OutboxStatus::Sending => h_flex()
                .gap_2()
                .text_sm()
                .child(Spinner::new().small())
                .child("Sending…")
                .into_any_element(),
            OutboxStatus::Queued | OutboxStatus::Done => v_flex()
                .gap_3()
                .child(div().text_sm().child(status))
                .children(note)
                .child(
                    actions
                        .child(
                            Button::new("send-now")
                                .outline()
                                .label("Send now")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.outbox_command(OutboxCommand::Flush(account), window, cx)
                                })),
                        )
                        .child(back_to_drafts()),
                )
                .into_any_element(),
        }
    }

    fn confirm_send_again(
        &mut self,
        id: catmail_core::OutboxId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .title("Send it again?")
                .description("If the first one did arrive, the recipients get it twice.")
                .confirm()
                .ok_text("Send again")
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| {
                        this.outbox_command(OutboxCommand::Retry(id), window, cx)
                    })
                    .ok();
                    true
                })
        });
    }

    fn outbox_command(
        &mut self,
        command: OutboxCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.client.clone();
        let what = match command {
            OutboxCommand::Retry(_) => "try again",
            OutboxCommand::Flush(_) => "send the message",
            OutboxCommand::MarkSent(_) => "mark the message as sent",
            OutboxCommand::Cancel(_) => "take the message out of the outbox",
        };
        let call = async move {
            match command {
                OutboxCommand::Retry(id) => client.retry_outbox(id).await.map(drop),
                OutboxCommand::MarkSent(id) => client.mark_outbox_sent(id).await.map(drop),
                OutboxCommand::Cancel(id) => client.cancel_outbox(id).await.map(drop),
                OutboxCommand::Flush(account) => client.flush_outbox(account).await.map(drop),
            }
        };
        report_failure(what, call, window, cx);
    }
}

#[derive(Debug, Clone, Copy)]
enum OutboxCommand {
    Retry(catmail_core::OutboxId),
    MarkSent(catmail_core::OutboxId),
    Cancel(catmail_core::OutboxId),
    Flush(catmail_core::AccountId),
}
