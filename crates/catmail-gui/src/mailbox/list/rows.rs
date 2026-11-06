//! The list's cells: a day's heading, or a row for a message, a draft or an outbox entry,
//! with its sender's avatar, its marks and its labels.

use catmail_core::{Flags, MessageSummary};
use catmail_frontend::format;
use catmail_frontend::list::{self, Line, Listing, ROW_LABELS, Row, RowId, Severity};
use nsh_base::theme;
use nsh_core::prelude::*;

use crate::buttons::Size;
use crate::icons::{Icon, icon};
use crate::mailbox::commands;

use super::{
    Checked, LabelNames, Selected, ShownIn, edit_selected_draft, open, reading, toggle_checked,
};

/// The row the pointer is on. One for the list rather than one per row: a cell that
/// scrolls to another row doesn't take the pointer with it.
#[derive(Clone, Default, PartialEq)]
struct Hovered(Option<RowId>);

/// What a cell shows: a day's heading or a row. A cell shows other items as the list
/// scrolls, so which it is follows its position.
pub fn item(at: Position) -> impl IntoElement {
    let item = computed(move || global::<Listing>().with(|l| l.line(at.index())));
    let kind = computed(move || {
        item.with(|item| match item {
            Some(Line::Day(_)) | None => 0,
            Some(Line::Row(Row::Message(_))) => 1,
            Some(Line::Row(Row::Draft(_))) => 2,
            Some(Line::Row(Row::Outbox(_))) => 3,
        })
    });
    dynamic(move || match kind.get() {
        1 => message_row(item).into_element(),
        2 => draft_row(item).into_element(),
        3 => outbox_row(item).into_element(),
        _ => text(move || match item.get() {
            Some(Line::Day(label)) => label,
            _ => String::new(),
        })
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .px(px(8.0))
        .pt(px(12.0))
        .pb(px(4.0))
        .text_color(move || theme().muted_foreground)
        .into_element(),
    })
}

/// A row's box: selected or under the pointer, it has the list's colors behind it; pressed,
/// it opens.
fn row_box(item: Memo<Option<Line>>) -> View {
    // Its id only, not the row: the pointer crossing rows doesn't copy their messages.
    let id = computed(move || {
        item.with(|item| match item {
            Some(Line::Row(row)) => Some(row.id()),
            _ => None,
        })
    });
    let id = move || id.get();
    let selected = move || {
        let id = id();
        id.is_some() && global::<Selected>().with(|s| s.0) == id
    };
    let hovered = move || {
        let id = id();
        id.is_some() && global::<Hovered>().with(|h| h.0) == id
    };
    // Apart from the next row, so a selected row's background and a hovered neighbour's
    // don't touch (GPUI Kit's list has no gap of its own).
    view()
        .rounded(px(8.0))
        .my(px(2.0))
        .bg(move || {
            if selected() {
                crate::theme::list_active()
            } else if hovered() {
                crate::theme::list_hover()
            } else {
                AdaptiveColor::TRANSPARENT
            }
        })
        .on_mouse_enter(move |_: &MouseEvent| global::<Hovered>().set(Hovered(id())))
        .on_mouse_leave(move |_: &MouseEvent| {
            if global::<Hovered>().with_untracked(|h| h.0) == id() {
                global::<Hovered>().set(Hovered(None));
            }
        })
        .on_press(move |press: &PressEvent| {
            if let Some(id) = id() {
                open(id);
                if press.clicks == 2 {
                    edit_selected_draft();
                }
            }
        })
}

fn message_row(item: Memo<Option<Line>>) -> impl IntoElement {
    // Only a change between reading and not makes the row again, not each selection.
    let reading = computed(reading);
    let message = computed(move || match item.get() {
        Some(Line::Row(Row::Message(m))) => Some(m),
        _ => None,
    });
    let id = move || message.with(|m| m.as_ref().map(|m| m.id));
    let field = move |f: fn(&MessageSummary) -> String| {
        move || message.with(|m| m.as_ref().map(f).unwrap_or_default())
    };
    let unread =
        move || message.with(|m| m.as_ref().is_some_and(|m| !m.flags.contains(Flags::SEEN)));
    let weight = move || {
        if unread() {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        }
    };
    let hovered = move || {
        let id = id();
        id.is_some() && global::<Hovered>().with(|h| h.0) == id.map(RowId::Message)
    };
    let who = move || {
        text(field(|m| format::sender(&m.from).to_owned()))
            .number_of_lines(1)
            .text_sm()
    };
    let subject = move || {
        text(field(|m| format::subject(m.subject.as_deref()).to_owned()))
            .number_of_lines(1)
            .text_sm()
    };
    let time = move || {
        text(field(|m| format::time(m.date)))
            .text_xs()
            .flex_shrink_0()
            .text_color(move || theme().muted_foreground)
    };
    let avatar = move || avatar(message);
    let marks = move || marks(message);
    row_box(item)
        .label(move || {
            SharedString::from(message.with(|m| {
                m.as_ref()
                    .map(|m| list::message_label(m, &label_names(m)))
                    .unwrap_or_default()
            }))
        })
        .child(dynamic(move || {
            if reading.get() {
                // The subject under the sender, as the reading pane leaves the rows less
                // room; the avatar as on one line.
                view()
                    .flex_row()
                    .items_start()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(8.0))
                    .child(avatar().mt(px(2.0)))
                    .child(
                        view()
                            .flex_1()
                            .min_w(px(0.0))
                            .gap(px(2.0))
                            .child(
                                view()
                                    .flex_row()
                                    .gap(px(8.0))
                                    .child(
                                        who()
                                            .flex_1()
                                            .font_weight(weight())
                                            .text_color(move || theme().foreground),
                                    )
                                    .child(time()),
                            )
                            .child(
                                view()
                                    .flex_row()
                                    .items_start()
                                    .gap(px(6.0))
                                    .child(
                                        // Set apart from the sender by color: weight tells
                                        // unread.
                                        subject()
                                            .number_of_lines(2)
                                            .flex_1()
                                            .font_weight(weight())
                                            .text_color(move || theme().muted_foreground),
                                    )
                                    .child(marks()),
                            ),
                    )
                    .into_element()
            } else {
                // The senders as a column, so the subjects line up.
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(20.0))
                    .py(px(10.0))
                    .child(avatar())
                    .child(
                        who()
                            .w(px(192.0))
                            .flex_shrink_0()
                            .font_weight(weight())
                            .text_color(move || theme().foreground),
                    )
                    .child(
                        // Only as wide as it is: the commands shown while hovered
                        // follow it.
                        subject()
                            .flex_shrink(1.0)
                            .min_w(px(0.0))
                            .font_weight(weight())
                            .text_color(move || theme().foreground),
                    )
                    // The commands, within reach of the pointer: the message isn't open.
                    .child(when(hovered, move || {
                        commands::buttons(
                            move || message.with(|m| m.iter().cloned().collect()),
                            Size::Row,
                        )
                    }))
                    .child(view().flex_1())
                    // Whom it went to, while hovered.
                    .child(when(hovered, move || {
                        text(field(list::recipients))
                            .number_of_lines(1)
                            .max_w(px(256.0))
                            .text_xs()
                            .text_color(move || theme().muted_foreground)
                    }))
                    .child(marks())
                    .child(time())
                    .into_element()
            }
        }))
}

/// The names of the message's user labels a row shows: not the one it is looked at in
/// ([`ShownIn`]), which every row has.
fn label_names(message: &MessageSummary) -> Vec<String> {
    let shown_in = global::<ShownIn>().with(|s| s.0.get(&message.account_id).copied());
    global::<LabelNames>().with(|l| list::label_names(&l.0, message, shown_in))
}

/// The names of all the message's user labels, as the reading pane shows them.
pub fn user_label_names(message: &MessageSummary) -> Vec<String> {
    global::<LabelNames>().with(|l| list::label_names(&l.0, message, None))
}

/// What follows the subject: the labels, the paper clip, the flag.
fn marks(message: Memo<Option<MessageSummary>>) -> View {
    let labels =
        computed(move || message.with(|m| m.as_ref().map(label_names).unwrap_or_default()));
    let has = move |flag: fn(&MessageSummary) -> bool| {
        move || message.with(|m| m.as_ref().is_some_and(flag))
    };
    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .gap(px(6.0))
        .child(each(
            move || labels.with(|l| l.iter().take(ROW_LABELS).cloned().collect::<Vec<_>>()),
            |name| name.clone(),
            |name, _| {
                text(move || name.get())
                    .number_of_lines(1)
                    .max_w(px(96.0))
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .text_xs()
                    .bg(move || theme().muted)
                    .text_color(move || theme().muted_foreground)
            },
        ))
        .child(
            text(
                move || match labels.with(Vec::len).saturating_sub(ROW_LABELS) {
                    0 => String::new(),
                    more => format!("+{more}"),
                },
            )
            .text_xs()
            .text_color(move || theme().muted_foreground),
        )
        .child(when(has(|m| m.has_attachments), || {
            icon(|| Icon::PaperClip, 12.0).text_color(move || theme().muted_foreground)
        }))
        .child(when(has(|m| m.flags.contains(Flags::FLAGGED)), || {
            icon(|| Icon::FlagSolid, 12.0).text_color(crate::theme::flag)
        }))
}

/// An avatar's background, by the sender's address (`theme::avatar`).
pub fn avatar_color(seed: &str) -> AdaptiveColor {
    crate::theme::avatar(seed)
}

/// The sender's initial on their hue, or a check while the message is checked; pressing
/// it checks or unchecks the message, not opening it as well.
fn avatar(message: Memo<Option<MessageSummary>>) -> View {
    let id = move || message.with(|m| m.as_ref().map(|m| m.id));
    let checked = move || {
        let id = id();
        id.is_some_and(|id| global::<Checked>().with(|c| c.0.contains(&id)))
    };
    let seed = move || {
        message.with(|m| {
            m.as_ref()
                .and_then(|m| m.from.first())
                .map(|a| a.email.clone())
                .unwrap_or_default()
        })
    };
    view()
        .w(px(24.0))
        .h(px(24.0))
        .flex_shrink_0()
        .rounded(px(12.0))
        .items_center()
        .justify_center()
        .bg(move || {
            if checked() {
                theme().primary
            } else {
                avatar_color(&seed())
            }
        })
        .label("Select")
        .on_press(move || {
            if let Some(id) = id() {
                toggle_checked(id);
            }
        })
        .child(dynamic(move || {
            if checked() {
                icon(|| Icon::Check, 14.0)
                    .text_color(move || theme().primary_foreground)
                    .into_element()
            } else {
                text(move || {
                    message.with(|m| {
                        m.as_ref()
                            .map(|m| list::initial(format::sender(&m.from)))
                            .unwrap_or_default()
                    })
                })
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(move || theme().foreground)
                .into_element()
            }
        }))
}

fn draft_row(item: Memo<Option<Line>>) -> impl IntoElement {
    // Only a change between reading and not makes the row again, not each selection.
    let reading = computed(reading);
    let draft = computed(move || match item.get() {
        Some(Line::Row(Row::Draft(d))) => Some(d),
        _ => None,
    });
    let field = move |f: fn(&catmail_core::Draft) -> String| {
        move || draft.with(|d| d.as_deref().map(f).unwrap_or_default())
    };
    let who = move || {
        text(field(list::draft_recipients))
            .number_of_lines(1)
            .text_sm()
    };
    let subject = move || {
        text(field(|d| {
            format::subject(Some(&d.content.subject)).to_owned()
        }))
        .number_of_lines(1)
        .text_sm()
    };
    let time = move || {
        text(field(|d| format::time(Some(d.updated_at))))
            .text_xs()
            .flex_shrink_0()
            .text_color(move || theme().muted_foreground)
    };
    let clip = move || {
        when(
            move || draft.with(|d| d.as_ref().is_some_and(|d| !d.attachments.is_empty())),
            || icon(|| Icon::PaperClip, 12.0).text_color(move || theme().muted_foreground),
        )
    };
    row_box(item).child(dynamic(move || {
        if reading.get() {
            view()
                .px(px(8.0))
                .py(px(8.0))
                .gap(px(2.0))
                .child(
                    view()
                        .flex_row()
                        .gap(px(8.0))
                        .child(who().flex_1().text_color(move || theme().foreground))
                        .child(time()),
                )
                .child(
                    view()
                        .flex_row()
                        .gap(px(6.0))
                        .child(
                            subject()
                                .number_of_lines(2)
                                .flex_1()
                                .text_color(move || theme().muted_foreground),
                        )
                        .child(clip()),
                )
                .into_element()
        } else {
            view()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .px(px(20.0))
                .py(px(10.0))
                .child(
                    who()
                        .w(px(192.0))
                        .flex_shrink_0()
                        .text_color(move || theme().foreground),
                )
                .child(subject().flex_1().text_color(move || theme().foreground))
                .child(clip())
                .child(time())
                .into_element()
        }
    }))
}

fn outbox_row(item: Memo<Option<Line>>) -> impl IntoElement {
    let entry = computed(move || match item.get() {
        Some(Line::Row(Row::Outbox(e))) => Some(e),
        _ => None,
    });
    let field = move |f: fn(&catmail_core::OutboxEntry) -> String| {
        move || entry.with(|e| e.as_ref().map(f).unwrap_or_default())
    };
    let severity = move || entry.with(|e| e.as_ref().and_then(|e| list::outbox_status(e).1));
    row_box(item)
        .px(px(8.0))
        .py(px(8.0))
        .gap(px(2.0))
        .child(
            view()
                .flex_row()
                .gap(px(8.0))
                .child(
                    text(field(|e| format::subject(Some(&e.subject)).to_owned()))
                        .number_of_lines(1)
                        .flex_1()
                        .text_sm()
                        .style_when(
                            move || severity().is_some(),
                            |s| s.font_weight(FontWeight::BOLD),
                        )
                        .text_color(move || theme().foreground),
                )
                .child(
                    text(field(|e| format::time(Some(e.created_at))))
                        .text_xs()
                        .text_color(move || theme().muted_foreground),
                ),
        )
        .child(
            text(field(|e| format!("To {}", e.recipients.join(", "))))
                .number_of_lines(1)
                .text_xs()
                .text_color(move || theme().muted_foreground),
        )
        .child(
            view().flex_row().child(
                text(field(|e| list::outbox_status(e).0.to_owned()))
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .text_xs()
                    .bg(move || match severity() {
                        Some(Severity::Warning) => crate::theme::warning_badge(),
                        Some(Severity::Danger) => crate::theme::danger_badge(),
                        None => theme().muted,
                    })
                    .text_color(move || theme().foreground),
            ),
        )
}
