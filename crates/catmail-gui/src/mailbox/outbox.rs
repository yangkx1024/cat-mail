//! The right pane for an outbox entry: what happened to it, and the choices it leaves.

use catmail_core::{OutboxEntry, OutboxStatus};
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
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
        let reason = entry.last_error.clone().unwrap_or_default();
        // Why it is where it is, under the state's own words.
        let note = (!reason.is_empty()).then(|| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(reason.clone())
        });
        let (status, _) = outbox_status(entry);
        let state = match entry.status {
            OutboxStatus::Unknown => Alert::warning(
                "unknown",
                "The connection dropped after the message went to the server, before it \
                 confirmed it, so it may have arrived. It won't be sent again on its own: check \
                 with a recipient, then choose.",
            )
            .title("Delivery not confirmed")
            .into_any_element(),
            OutboxStatus::Failed => Alert::error("failed", reason)
                .title("The server rejected this message")
                .into_any_element(),
            OutboxStatus::Sending => {
                return h_flex()
                    .gap_2()
                    .text_sm()
                    .child(Spinner::new().small())
                    .child("Sending…")
                    .into_any_element();
            }
            OutboxStatus::Accepted => v_flex()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .child("Sent. A copy is still to be saved to the Sent folder."),
                )
                .children(note)
                .into_any_element(),
            OutboxStatus::Queued | OutboxStatus::Done => v_flex()
                .gap_3()
                .child(div().text_sm().child(status))
                .children(note)
                .into_any_element(),
        };
        let buttons = outbox_choices(entry)
            .into_iter()
            .enumerate()
            .map(|(ix, choice)| {
                let status = entry.status;
                let button = Button::new(choice.id()).label(choice.label(entry.status));
                let button = if ix == 0 {
                    button.outline()
                } else {
                    button.ghost()
                };
                button.on_click(cx.listener(move |this, _, window, cx| {
                    this.outbox_choice(choice, status, window, cx)
                }))
            });
        v_flex()
            .gap_3()
            .child(state)
            .child(h_flex().gap_2().flex_wrap().children(buttons))
            .into_any_element()
    }

    /// Does what the user chose; sending again what may have arrived asks first.
    fn outbox_choice(
        &mut self,
        choice: OutboxChoice,
        status: OutboxStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = match choice {
            OutboxChoice::Retry(id) => {
                if status.retry_may_duplicate() {
                    self.confirm_send_again(id, window, cx);
                    return;
                }
                OutboxCommand::Retry(id)
            }
            OutboxChoice::SendNow(account) => OutboxCommand::Flush(account),
            OutboxChoice::MarkSent(id) => OutboxCommand::MarkSent(id),
            OutboxChoice::Cancel(id) => OutboxCommand::Cancel(id),
        };
        self.outbox_command(command, window, cx);
    }

    fn confirm_send_again(
        &mut self,
        id: catmail_core::OutboxId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        layout::confirm_danger(
            "Send it again?",
            "If the first one did arrive, the recipients get it twice.",
            "Send again",
            window,
            cx,
            move |window, cx| {
                this.update(cx, |this, cx| {
                    this.outbox_command(OutboxCommand::Retry(id), window, cx)
                })
                .ok();
            },
        );
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

/// What the user can do about an entry, as its status allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutboxChoice {
    SendNow(catmail_core::AccountId),
    MarkSent(catmail_core::OutboxId),
    Retry(catmail_core::OutboxId),
    Cancel(catmail_core::OutboxId),
}

impl OutboxChoice {
    fn id(self) -> &'static str {
        match self {
            Self::SendNow(_) => "send-now",
            Self::MarkSent(_) => "mark-sent",
            Self::Retry(_) => "retry",
            Self::Cancel(_) => "cancel",
        }
    }

    fn label(self, status: OutboxStatus) -> &'static str {
        match self {
            Self::SendNow(_) => "Send now",
            Self::MarkSent(_) => "Mark as sent",
            Self::Retry(_) if status.went_out() => "Save the copy now",
            Self::Retry(_) if status.retry_may_duplicate() => "Send again",
            Self::Retry(_) => "Try again",
            Self::Cancel(_) if status.went_out() => "Don't keep a copy",
            Self::Cancel(_) => "Back to drafts",
        }
    }
}

/// The entry's choices, the likeliest first. A queued entry is sent with the rest of
/// the account's outbox; one the server rejected most likely needs editing.
fn outbox_choices(entry: &OutboxEntry) -> Vec<OutboxChoice> {
    let status = entry.status;
    let mut choices = Vec::new();
    if status.can_mark_sent() {
        choices.push(OutboxChoice::MarkSent(entry.id));
    }
    if status == OutboxStatus::Queued {
        choices.push(OutboxChoice::SendNow(entry.account_id));
    } else if status.can_retry() {
        choices.push(OutboxChoice::Retry(entry.id));
    }
    if status.can_cancel() {
        let cancel = OutboxChoice::Cancel(entry.id);
        match status {
            OutboxStatus::Failed => choices.insert(0, cancel),
            _ => choices.push(cancel),
        }
    }
    choices
}

#[derive(Debug, Clone, Copy)]
enum OutboxCommand {
    Retry(catmail_core::OutboxId),
    MarkSent(catmail_core::OutboxId),
    Cancel(catmail_core::OutboxId),
    Flush(catmail_core::AccountId),
}

#[cfg(test)]
mod tests {
    use catmail_core::{AccountId, OutboxId};

    use super::*;

    fn entry(status: OutboxStatus) -> OutboxEntry {
        OutboxEntry {
            id: OutboxId(1),
            account_id: AccountId(2),
            identity: catmail_core::IdentityId(3),
            draft_id: None,
            subject: String::new(),
            recipients: Vec::new(),
            message_id: String::new(),
            status,
            attempts: 0,
            next_attempt_at: None,
            last_error: None,
            created_at: 0,
        }
    }

    #[test]
    fn choices_follow_what_the_engine_allows() {
        let labels = |status| {
            outbox_choices(&entry(status))
                .into_iter()
                .map(|c| c.label(status))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(OutboxStatus::Unknown),
            ["Mark as sent", "Send again", "Back to drafts"]
        );
        assert_eq!(
            labels(OutboxStatus::Failed),
            ["Back to drafts", "Try again"]
        );
        assert_eq!(
            labels(OutboxStatus::Accepted),
            ["Save the copy now", "Don't keep a copy"]
        );
        assert_eq!(labels(OutboxStatus::Queued), ["Send now", "Back to drafts"]);
        assert!(labels(OutboxStatus::Sending).is_empty());
        assert!(labels(OutboxStatus::Done).is_empty());
    }
}
