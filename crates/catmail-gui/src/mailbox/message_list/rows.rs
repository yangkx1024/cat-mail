//! How a row of the list looks: a message, a draft or an outbox entry.

use catmail_core::{Draft, Flags, MessageSummary, OutboxEntry, OutboxStatus};
use gpui_kit::component::Icon;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{App, FontWeight, IntoElement, SharedString, div};

use crate::assets::Hero;
use crate::format;

use super::*;

/// Every row has the same three lines: the list requires one row height. The time is
/// all a row says of when: its day is the heading above it.
fn three_lines(
    top: (SharedString, SharedString, bool),
    middle: impl IntoElement,
    bottom: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    let (title, trailing, strong) = top;
    v_flex()
        .w_full()
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
                        .when(strong, |this| this.font_weight(FontWeight::BOLD))
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
        .child(bottom)
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

/// The most labels a row shows by name; the rest are counted.
const ROW_LABELS: usize = 2;

/// `labels` are the names of the message's labels to show.
pub(super) fn message_row(message: &MessageSummary, labels: &[SharedString], cx: &App) -> ListItem {
    let unread = !message.flags.contains(Flags::SEEN);
    let flagged = message.flags.contains(Flags::FLAGGED);
    let sender = format::sender(&message.from);
    let subject = format::subject(message.subject.as_deref()).to_owned();
    let date = format::time(message.date);
    let muted = cx.theme().muted_foreground;
    let label = format!(
        "{sender}, {subject}, {date}{}{}{}{}",
        if unread { ", unread" } else { "" },
        if flagged { ", flagged" } else { "" },
        if message.has_attachments {
            ", has attachments"
        } else {
            ""
        },
        if labels.is_empty() {
            String::new()
        } else {
            format!(", labels {}", labels.join(", "))
        },
    );
    let more = labels.len().saturating_sub(ROW_LABELS);
    let neutral = cx.theme().neutral_badge();
    let middle = h_flex()
        .gap_1p5()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                // Unread is told by weight alone: sender and subject in bold.
                .when(unread, |this| this.font_weight(FontWeight::BOLD))
                .child(subject),
        )
        .children(labels.iter().take(ROW_LABELS).map(|name| {
            crate::layout::badge(name.clone(), neutral, cx)
                .min_w_0()
                .max_w_24()
                .truncate()
        }))
        .when(more > 0, |this| {
            this.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("+{more}")),
            )
        })
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
    let subject = format::subject(Some(&draft.content.subject)).to_owned();
    let date = format::time(Some(draft.updated_at));
    let muted = cx.theme().muted_foreground;
    let middle = h_flex()
        .gap_1p5()
        .child(div().flex_1().min_w_0().truncate().text_sm().child(subject))
        .when(!draft.attachments.is_empty(), |this| {
            this.child(Icon::new(Hero::PaperClip).xsmall().text_color(muted))
        });
    ListItem::new(("draft", draft.id.0 as u64)).child(three_lines(
        (to.into(), date.into(), false),
        middle,
        snippet(Some(&draft.content.text), cx),
        cx,
    ))
}

/// How an outbox entry's state reads, and how loud it is if it needs the user.
pub fn outbox_status(entry: &OutboxEntry) -> (&'static str, Option<Severity>) {
    let text = match entry.status {
        OutboxStatus::Queued if entry.attempts > 0 => "Waiting to retry",
        OutboxStatus::Queued => "Waiting to send",
        OutboxStatus::Sending => "Sending…",
        OutboxStatus::Accepted => "Sent · saving to Sent",
        OutboxStatus::Unknown => "Not confirmed",
        OutboxStatus::Failed => "Rejected",
        OutboxStatus::Done => "Sent",
    };
    let severity = entry.status.needs_user().then_some(match entry.status {
        OutboxStatus::Failed => Severity::Danger,
        _ => Severity::Warning,
    });
    (text, severity)
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
    let date = format::time(Some(entry.created_at));
    let theme = cx.theme();
    let (bg, fg) = match severity {
        Some(Severity::Warning) => theme.warning_badge(),
        Some(Severity::Danger) => theme.danger_badge(),
        None => theme.neutral_badge(),
    };
    let badge = h_flex().child(crate::layout::badge(status, (bg, fg), cx));
    ListItem::new(("outbox", entry.id.0 as u64))
        .accessibility_label(format!("{subject}, {to}, {status}"))
        .child(three_lines(
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
