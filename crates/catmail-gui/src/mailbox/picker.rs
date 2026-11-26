//! "Move to" and "Labels": a dialog that lists an account's folders, filtered as you
//! type, to move messages to one or to check a message's labels, and what picking one
//! does: the move, or the label put on or taken off.

use std::collections::HashSet;

use catmail_core::{FolderId, MessageId, MessageSummary};
use catmail_frontend::mailbox::commands;

use nsh_base::theme;
use nsh_core::ModalAnimation;
use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::accounts::with_accounts;
use crate::buttons::{Size, icon_button};
use crate::form;
use crate::icons::{Icon, icon};
use crate::mailbox::listing;

#[derive(Debug, Clone, PartialEq)]
pub enum Picking {
    /// Picks a folder to move these to, and closes.
    Move(Vec<MessageSummary>),
    /// Checks the message's labels and toggles one as it is picked, staying open.
    Labels(MessageId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Picker {
    pub kind: Picking,
    pub folders: Vec<(FolderId, String)>,
}

#[derive(Clone, Default)]
struct OpenPicker(Option<Picker>);

pub fn open_picker(picker: Picker) {
    global::<OpenPicker>().set(OpenPicker(Some(picker)));
}

fn close_picker() {
    global::<OpenPicker>().set(OpenPicker(None));
}

/// The picker, while one is open: filtered as you type, Enter or a click picks.
pub fn picker() -> impl IntoElement {
    dynamic(move || match global::<OpenPicker>().with(|p| p.0.clone()) {
        Some(picker) => picker_dialog(picker).into_element(),
        None => view().into_element(),
    })
}

fn picker_dialog(picker: Picker) -> impl IntoElement {
    let Picker { kind, folders } = picker;
    let labels = match &kind {
        Picking::Labels(id) => Some(*id),
        Picking::Move(_) => None,
    };
    if let Some(id) = labels {
        // Closes once the message isn't the one selected (in a label, taking it off the
        // label opens the next): it would change another.
        effect(move || {
            if listing::selected_id() != Some(id) {
                untracked(close_picker);
            }
        });
    }
    // The message's labels as the list has them now: a change refused and undone, or made
    // elsewhere, shows too.
    let checked = computed(move || -> HashSet<FolderId> {
        match labels {
            Some(id) => listing::message(id)
                .map(|m| m.labels.into_iter().collect())
                .unwrap_or_default(),
            None => HashSet::new(),
        }
    });
    let any = !folders.is_empty();
    let query = signal(String::new());
    let shown = computed(move || {
        query.with(|q| {
            let matches = commands::picker_filter(q);
            folders
                .iter()
                .filter(|(_, name)| matches(name))
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    let highlighted = signal(0usize);
    let kind = std::rc::Rc::new(kind);
    let pick = {
        let kind = kind.clone();
        move |folder: FolderId| match &*kind {
            Picking::Move(messages) => {
                close_picker();
                move_to(messages, folder);
            }
            Picking::Labels(id) => toggle_label(*id, folder),
        }
    };
    let pick = std::rc::Rc::new(pick);
    let pick_highlighted = {
        let pick = pick.clone();
        move || {
            let folder = shown.with_untracked(|s| s.get(highlighted.get_untracked()).map(|f| f.0));
            if let Some(folder) = folder {
                pick(folder);
            }
        }
    };
    on_key("down", move |_| {
        let len = shown.with_untracked(Vec::len);
        highlighted.update(|h| *h = (*h + 1).min(len.saturating_sub(1)));
    });
    on_key("up", move |_| {
        highlighted.update(|h| *h = h.saturating_sub(1))
    });
    let (title, placeholder) = match labels {
        Some(_) => ("Labels", "Label"),
        None => ("Move to", "Folder"),
    };
    let row = move |entry: ReadSignal<(FolderId, String)>, ix: ReadSignal<usize>| {
        let pick = pick.clone();
        let is_checked = move || checked.with(|c| c.contains(&entry.get().0));
        view()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .h(px(32.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .bg(move || {
                if highlighted.get() == ix.get() {
                    theme().accent
                } else {
                    AdaptiveColor::TRANSPARENT
                }
            })
            .hover(|s| s.bg(move || theme().accent))
            .label(move || {
                let name = entry.get().1;
                SharedString::from(match labels {
                    Some(_) if is_checked() => format!("{name}, checked"),
                    Some(_) => format!("{name}, not checked"),
                    None => name,
                })
            })
            .on_press(move || pick(entry.get_untracked().0))
            .child(
                icon(
                    move || {
                        if labels.is_some() {
                            Icon::Tag
                        } else {
                            Icon::Folder(catmail_frontend::mailbox::folders::FolderIcon::Folder)
                        }
                    },
                    16.0,
                )
                .text_color(move || theme().muted_foreground),
            )
            .child(
                text(move || entry.get().1)
                    .number_of_lines(1)
                    .flex_1()
                    .text_sm()
                    .text_color(move || theme().popover_foreground),
            )
            .child(when(is_checked, || {
                icon(|| Icon::Check, 16.0).text_color(move || theme().primary)
            }))
    };
    form::count_as_open();
    nsh_base::dialog()
        .animation(ModalAnimation::Fade)
        .on_cancel(close_picker)
        .child(
            view()
                .flex_1()
                .items_center()
                .justify_center()
                .bg(hsla(0.0, 0.0, 0.0, 0.4))
                .child(
                    view()
                        .w(px(416.0))
                        .h(px(420.0))
                        .p(px(16.0))
                        .gap(px(12.0))
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(move || theme().border)
                        .bg(move || theme().popover)
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .child(
                                    text(title)
                                        .flex_1()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(move || theme().popover_foreground),
                                )
                                // Closes it with the pointer, as Escape does. Taller than
                                // the title: it overhangs the row.
                                .child(
                                    icon_button(
                                        || Icon::XMark,
                                        || "Close".to_owned(),
                                        || true,
                                        Size::Row,
                                        close_picker,
                                    )
                                    .my(px(-4.0)),
                                ),
                        )
                        .child(
                            input()
                                .placeholder(placeholder.to_owned())
                                .auto_focus(true)
                                .on_change(move |text: &str| {
                                    query.set(text.to_owned());
                                    highlighted.set(0);
                                })
                                .on_submit(move |_: &str| pick_highlighted())
                                .h(px(36.0))
                                .px(px(10.0))
                                .rounded(px(8.0))
                                .border_1()
                                .border_color(move || theme().input)
                                .text_sm()
                                .text_color(move || theme().popover_foreground),
                        )
                        .child(
                            scroll().flex_1().child(
                                view()
                                    .child(each(move || shown.get(), |(id, _)| *id, row))
                                    .child(when(
                                        move || shown.with(Vec::is_empty),
                                        move || {
                                            text(commands::picker_empty_text(labels.is_some(), any))
                                                .text_sm()
                                                .p(px(16.0))
                                                .text_color(move || theme().muted_foreground)
                                        },
                                    )),
                            ),
                        ),
                ),
        )
}

/// Moves those of the messages, all of one account, that moving to `to` changes.
fn move_to(messages: &[MessageSummary], to: FolderId) {
    let source = listing::source();
    listing::run_change(with_accounts(|a| {
        commands::movable(messages, a, source.as_ref(), to)
    }));
}

/// Adds `label` to the message, the selected one, or takes it off, as it has it now.
/// Off the label it is listed in, it leaves the list: on to the next one.
fn toggle_label(id: MessageId, label: FolderId) {
    let Some(message) = listing::selected_message().filter(|m| m.id == id) else {
        return;
    };
    listing::run_change(commands::label_change(&message, label));
}
