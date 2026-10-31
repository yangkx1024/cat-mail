//! How a row of the list looks: a message, a draft or an outbox entry.

use catmail_core::{Draft, Flags, MessageSummary, OutboxEntry, OutboxStatus};
use gpui_kit::component::Icon;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{App, Div, FontWeight, SharedString, div, rems};

use crate::assets::Hero;
use crate::format;

use super::*;

/// How the rows of messages and drafts are laid out: on one line while the list has the
/// window to itself, the subject under the sender while the reading pane is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shape {
    Line,
    Stacked,
}

/// What a message or draft row shows: who it is from or to, its subject, its time (all a
/// row says of when: its day is the heading above it) and the marks after the subject.
/// `strong` sets the first two in bold.
struct Summary {
    who: SharedString,
    subject: SharedString,
    time: SharedString,
    strong: bool,
    marks: Div,
}

/// A stacked subject's line height: it always takes two lines, as every row of a list
/// keeps one height.
const SUBJECT_LINE: f32 = 1.25;

impl Summary {
    /// `item` laid out as `shape`, its padding too: a row on one line has the width to
    /// spare.
    fn row(self, item: ListItem, shape: Shape, cx: &App) -> ListItem {
        let Self {
            who,
            subject,
            time,
            strong,
            marks,
        } = self;
        match shape {
            // The senders as a column, so the subjects line up.
            Shape::Line => item.px_5().py_3().child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .child(text(who, strong).w_48().flex_none().truncate())
                    .child(text(subject, strong).flex_1().truncate())
                    .child(marks)
                    .child(time_of_day(time, cx)),
            ),
            Shape::Stacked => item.px_2().child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_0p5()
                    .child(top_line(who, time, strong, cx))
                    .child(
                        h_flex()
                            .items_start()
                            .gap_1p5()
                            .child(
                                // Set apart from the sender above it by color: weight tells
                                // unread.
                                text(subject, strong)
                                    .flex_1()
                                    .text_color(cx.theme().muted_foreground)
                                    .line_height(rems(SUBJECT_LINE))
                                    .h(rems(2. * SUBJECT_LINE))
                                    .line_clamp(2)
                                    .text_ellipsis(),
                            )
                            // Beside the subject's first line.
                            .child(h_flex().flex_none().h(rems(SUBJECT_LINE)).child(marks)),
                    ),
            ),
        }
    }
}

fn text(text: SharedString, strong: bool) -> Div {
    div()
        .min_w_0()
        .text_sm()
        .when(strong, |this| this.font_weight(FontWeight::BOLD))
        .child(text)
}

fn time_of_day(time: SharedString, cx: &App) -> Div {
    div()
        .flex_none()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(time)
}

/// The title of a row over the rest, its time at the end.
fn top_line(title: SharedString, time: SharedString, strong: bool, cx: &App) -> Div {
    h_flex()
        .gap_2()
        .child(text(title, strong).flex_1().truncate())
        .child(time_of_day(time, cx))
}

/// The most labels a row shows by name; the rest are counted.
const ROW_LABELS: usize = 2;

/// `labels` are the names of the message's labels to show.
pub(super) fn message_row(
    message: &MessageSummary,
    labels: &[SharedString],
    shape: Shape,
    cx: &App,
) -> ListItem {
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
    let marks = h_flex()
        .gap_1p5()
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
    let summary = Summary {
        who: sender.into(),
        subject: subject.into(),
        time: date.into(),
        // Unread is told by weight alone: sender and subject in bold.
        strong: unread,
        marks,
    };
    let item = ListItem::new(("message", message.id.0 as u64)).accessibility_label(label);
    summary.row(item, shape, cx)
}

pub(super) fn draft_row(draft: &Draft, shape: Shape, cx: &App) -> ListItem {
    let to = format::names(draft.content.recipients());
    let to = if to.is_empty() {
        "No recipients".to_owned()
    } else {
        format!("To {to}")
    };
    let subject = format::subject(Some(&draft.content.subject)).to_owned();
    let date = format::time(Some(draft.updated_at));
    let marks = h_flex().when(!draft.attachments.is_empty(), |this| {
        this.child(
            Icon::new(Hero::PaperClip)
                .xsmall()
                .text_color(cx.theme().muted_foreground),
        )
    });
    let summary = Summary {
        who: to.into(),
        subject: subject.into(),
        time: date.into(),
        strong: false,
        marks,
    };
    summary.row(ListItem::new(("draft", draft.id.0 as u64)), shape, cx)
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
    let accessibility_label = format!("{subject}, {to}, {status}");
    // Three lines in every row: the list requires one row height.
    ListItem::new(("outbox", entry.id.0 as u64))
        .accessibility_label(accessibility_label)
        .px_2()
        .child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap_0p5()
                .child(top_line(
                    subject.into(),
                    date.into(),
                    severity.is_some(),
                    cx,
                ))
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(to),
                )
                .child(badge),
        )
}
