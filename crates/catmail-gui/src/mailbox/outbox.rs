//! The right pane for an outbox entry: what happened to it, and the choices it leaves.

use catmail_core::{OutboxEntry, OutboxStatus};
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window, div};

use super::{Mailbox, report_failure};
use crate::format;
use crate::layout;
use catmail_frontend::outbox::{Choice, State, choices};

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
        v_flex()
            .size_full()
            .child(layout::toolbar(cx).child(self.back_button(cx)))
            .child(
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
        // Why it is where it is, under the state's own words.
        let note = |reason: Option<String>| {
            reason.map(|reason| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(reason)
            })
        };
        let state = match State::of(entry) {
            State::Unknown { title, text } => Alert::warning("unknown", text)
                .title(title)
                .into_any_element(),
            State::Rejected { title, reason } => Alert::error("failed", reason)
                .title(title)
                .into_any_element(),
            State::Sending => {
                return h_flex()
                    .gap_2()
                    .text_sm()
                    .child(Spinner::new().small())
                    .child("Sending…")
                    .into_any_element();
            }
            State::Note { text, reason } => v_flex()
                .gap_3()
                .child(div().text_sm().child(text))
                .children(note(reason))
                .into_any_element(),
        };
        let buttons = choices(entry).into_iter().enumerate().map(|(ix, choice)| {
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
        choice: Choice,
        status: OutboxStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((confirm, go_on)) = choice.confirm(status) else {
            self.run_outbox_choice(choice, window, cx);
            return;
        };
        let this = cx.entity().downgrade();
        layout::confirm_danger(
            confirm.title,
            confirm.description,
            go_on,
            window,
            cx,
            move |window, cx| {
                this.update(cx, |this, cx| this.run_outbox_choice(choice, window, cx))
                    .ok();
            },
        );
    }

    fn run_outbox_choice(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.clone();
        report_failure(
            choice.what(),
            async move { choice.run(&client).await },
            window,
            cx,
        );
    }
}
