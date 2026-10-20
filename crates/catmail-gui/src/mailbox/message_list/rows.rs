//! How a row of the list looks: a message, a draft or an outbox entry.

use catmail_core::{Draft, Flags, MessageSummary, OutboxEntry, OutboxStatus};
use gpui_kit::component::Icon;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{App, FontWeight, Hsla, IntoElement, SharedString, div};

use crate::assets::Hero;
use crate::format;

use super::*;

/// Every row has the same three lines: the list requires one row height. The leading
/// lane holds the unread dot; it is there (empty) on read rows too, so text lines up.
fn three_lines(
    dot: Option<Hsla>,
    top: (SharedString, SharedString, bool),
    middle: impl IntoElement,
    bottom: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    let (title, trailing, strong) = top;
    h_flex()
        .w_full()
        .items_start()
        .gap_2()
        .child(
            div().flex_none().w_2().pt_2().child(
                div()
                    .size_1p5()
                    .rounded_full()
                    .when_some(dot, |this, color| this.bg(color)),
            ),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .when(strong, |this| this.font_weight(FontWeight::SEMIBOLD))
                                .child(title),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(trailing),
                        ),
                )
                .child(middle)
                .child(bottom),
        )
}

fn snippet(text: Option<&str>, cx: &App) -> impl IntoElement + use<> {
    // More words than a row has room for; a draft's text may be a whole forwarded message.
    let text = text
        .map(|s| s.split_whitespace().take(40).collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "\u{a0}".into());
    div()
        .truncate()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

pub(super) fn message_row(message: &MessageSummary, cx: &App) -> ListItem {
    let unread = !message.flags.contains(Flags::SEEN);
    let flagged = message.flags.contains(Flags::FLAGGED);
    let sender = format::sender(&message.from);
    let subject = format::subject(message.subject.as_deref());
    let date = format::short_date(message.date);
    let muted = cx.theme().muted_foreground;
    let label = format!(
        "{sender}, {subject}, {date}{}{}{}",
        if unread { ", unread" } else { "" },
        if flagged { ", flagged" } else { "" },
        if message.has_attachments {
            ", has attachments"
        } else {
            ""
        },
    );
    let middle = h_flex()
        .gap_1p5()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .when(unread, |this| this.font_weight(FontWeight::SEMIBOLD))
                .child(subject),
        )
        .when(message.has_attachments, |this| {
            this.child(Icon::new(Hero::PaperClip).xsmall().text_color(muted))
        })
        .when(flagged, |this| {
            this.child(
                Icon::new(Hero::FlagSolid)
                    .xsmall()
                    .text_color(cx.theme().flag()),
            )
        });
    ListItem::new(("message", message.id.0 as u64))
        .accessibility_label(label)
        .child(three_lines(
            unread.then(|| cx.theme().primary),
            (sender.into(), date.into(), unread),
            middle,
            snippet(message.snippet.as_deref(), cx),
            cx,
        ))
}

pub(super) fn draft_row(draft: &Draft, cx: &App) -> ListItem {
    let to = format::names(draft.content.recipients());
    let to = if to.is_empty() {
        "No recipients".to_owned()
    } else {
        format!("To {to}")
    };
    let subject = format::subject(Some(&draft.content.subject));
    let date = format::short_date(Some(draft.updated_at));
    let muted = cx.theme().muted_foreground;
    let middle = h_flex()
        .gap_1p5()
        .child(div().flex_1().min_w_0().truncate().text_sm().child(subject))
        .when(!draft.attachments.is_empty(), |this| {
            this.child(Icon::new(Hero::PaperClip).xsmall().text_color(muted))
        });
    ListItem::new(("draft", draft.id.0 as u64)).child(three_lines(
        None,
        (to.into(), date.into(), false),
        middle,
        snippet(Some(&draft.content.text), cx),
        cx,
    ))
}

/// How an outbox entry's state reads, and whether it needs the user.
pub fn outbox_status(entry: &OutboxEntry) -> (&'static str, Option<Severity>) {
    match entry.status {
        OutboxStatus::Queued if entry.attempts > 0 => ("Waiting to retry", None),
        OutboxStatus::Queued => ("Waiting to send", None),
        OutboxStatus::Sending => ("Sending…", None),
        OutboxStatus::Accepted => ("Sent · saving to Sent", None),
        OutboxStatus::Unknown => ("Not confirmed", Some(Severity::Warning)),
        OutboxStatus::Failed => ("Rejected", Some(Severity::Danger)),
        OutboxStatus::Done => ("Sent", None),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Danger,
}

pub(super) fn outbox_row(entry: &OutboxEntry, cx: &App) -> ListItem {
    let (status, severity) = outbox_status(entry);
    let to = format!("To {}", entry.recipients.join(", "));
    let subject = format::subject(Some(&entry.subject));
    let date = format::short_date(Some(entry.created_at));
    let theme = cx.theme();
    let (bg, fg) = match severity {
        Some(Severity::Warning) => theme.warning_badge(),
        Some(Severity::Danger) => theme.danger_badge(),
        None => (theme.muted, theme.muted_foreground),
    };
    let badge = h_flex().child(
        div()
            .px_1p5()
            .rounded(theme.radius)
            .text_xs()
            .bg(bg)
            .text_color(fg)
            .child(status),
    );
    ListItem::new(("outbox", entry.id.0 as u64))
        .accessibility_label(format!("{subject}, {to}, {status}"))
        .child(three_lines(
            None,
            (subject.into(), date.into(), severity.is_some()),
            div()
                .truncate()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(to),
            badge,
            cx,
        ))
}
