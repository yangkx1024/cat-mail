//! The right pane for an outbox entry: what happened to it, and the choices it leaves
//! (`catmail_frontend::outbox`).

use catmail_core::{OutboxEntry, OutboxStatus};
use catmail_frontend::format;
use catmail_frontend::outbox::{Choice, State, choices, facts};
use nsh_base::theme;
use nsh_core::prelude::*;

use crate::buttons::text_button;
use crate::chrome;
use crate::clock::clock;
use crate::engine::client;
use crate::form::confirmed;
use crate::icons::{Icon, icon};
use crate::mailbox::list::{self};
use crate::mailbox::panes::{back_button, fact};
use crate::mailbox::sidebar::with_accounts;
use crate::tasks::in_view;
use crate::theme::SelectableText as _;
use crate::toast::report;

/// The pane, while an outbox entry is selected.
pub fn pane() -> impl IntoElement {
    let entry = computed(list::selected_outbox);
    view()
        .flex_1()
        .min_w(px(0.0))
        .child(chrome::toolbar().child(back_button()))
        .child(dynamic(move || match entry.get() {
            Some(entry) => page(entry).into_element(),
            None => view().flex_1().into_element(),
        }))
}

fn page(entry: OutboxEntry) -> impl IntoElement {
    let identities: Vec<_> = with_accounts(|accounts| {
        accounts
            .iter()
            .flat_map(|a| a.identities.iter().cloned())
            .collect()
    });
    let lines = facts(&entry, &identities, &clock())
        .into_iter()
        .map(|(name, value)| fact(name, value).into_element())
        .collect::<Vec<_>>();
    scroll().flex_1().child(
        view()
            .px(px(24.0))
            .pt(px(20.0))
            .pb(px(32.0))
            .gap(px(16.0))
            .child(
                text(format::subject(Some(&entry.subject)).to_owned())
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .selectable_text()
                    .text_color(move || theme().foreground),
            )
            .child(view().gap(px(4.0)).children(lines))
            .child(state(&entry)),
    )
}

/// What happened to the entry, and the choices it leaves.
fn state(entry: &OutboxEntry) -> impl IntoElement {
    let muted = move || theme().muted_foreground;
    let state = match State::of(entry) {
        State::Unknown { title, text } => alert(
            Icon::ExclamationTriangle,
            crate::theme::warning,
            title,
            text.to_owned(),
        ),
        State::Rejected { title, reason } => {
            alert(Icon::XCircle, || theme().destructive, title, reason)
        }
        State::Sending => {
            return text("Sending…").text_sm().text_color(muted).into_element();
        }
        // Why it is where it is, under the state's own words.
        State::Note { text: says, reason } => view()
            .gap(px(12.0))
            .child(text(says).text_sm().text_color(move || theme().foreground))
            .children(reason.map(|reason| {
                text(reason)
                    .text_sm()
                    .selectable_text()
                    .text_color(muted)
                    .into_element()
            }))
            .into_element(),
    };
    let status = entry.status;
    let buttons = choices(entry)
        .into_iter()
        .enumerate()
        .map(|(ix, choice)| {
            // The likeliest choice stands out.
            text_button(choice.label(status), ix == 0, move || {
                choose(choice, status)
            })
            .into_element()
        })
        .collect::<Vec<_>>();
    view()
        .gap(px(12.0))
        .child(state)
        .child(view().flex_row().flex_wrap().gap(px(8.0)).children(buttons))
        .into_element()
}

/// A note that needs the user: an icon, a title, and what it is about, in `tone` (the
/// warning color, or the destructive one).
fn alert(
    which: Icon,
    tone: fn() -> AdaptiveColor,
    title: &'static str,
    about: String,
) -> AnyElement {
    view()
        .flex_row()
        .items_start()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(tone)
        .child(icon(move || which, 18.0).flex_shrink_0().text_color(tone))
        .child(
            view()
                .flex_1()
                .gap(px(4.0))
                .child(
                    text(title)
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tone),
                )
                .child(
                    text(about)
                        .text_sm()
                        .selectable_text()
                        .text_color(move || theme().foreground),
                ),
        )
        .into_element()
}

/// Does what the user chose; sending again what may have arrived asks first.
fn choose(choice: Choice, status: OutboxStatus) {
    let Some(confirm) = choice.confirm(status) else {
        run(choice);
        return;
    };
    let answer = confirmed(confirm);
    in_view(async move {
        if answer.await {
            run(choice);
        }
    });
}

fn run(choice: Choice) {
    in_view(async move {
        if let Err(e) = choice.run(&client()).await {
            report(&format!("Couldn't {}", choice.what()), e);
        }
    });
}
