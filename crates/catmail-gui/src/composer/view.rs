//! How the composer looks: the header fields, the text, the attachments and the bar of
//! commands.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::PoisonError;

use catmail_core::{DraftAttachment, format_addresses};
use catmail_frontend::composer::{Field, Finish, Lifecycle, Opening, senders, window_title};
use catmail_frontend::format;
use futures_util::StreamExt as _;
use nsh_base::rich_editor::view::rich_editor;
use nsh_base::theme;
use nsh_core::element::{Owner, input};
use nsh_core::prelude::*;

use crate::buttons::{Size, icon_button, primary_button, separator};
use crate::chrome;
use crate::form;
use crate::icons::{Icon, icon};
use crate::toast;

use super::{
    Attachments, Composing, Formatting, From, LinkDialog, START, Shared, Shown, Typed, ask_link,
    attach, changed, choose_sender, close_link_dialog, close_requested, composing, discard,
    dropped, ending, field_index, focus, follow_identities, format, formatted, formatting_moved,
    insert_images, link_to, pasted, remove_attachment, rich, send, text_changed,
};
use crate::theme::SelectableText as _;

#[component]
pub fn Composer() -> impl IntoElement {
    let start = START
        .with_borrow(Clone::clone)
        .expect("a composer's view is made by `root`");
    let Opening {
        identities,
        draft,
        focus_body,
        images,
        ..
    } = start.opening.clone();
    let content = &draft.content;
    crate::tasks::own_tasks(Owner::current());
    crate::settings::follow();
    crate::settings::bind_keys(false);
    global::<Shared>().set(Shared(Some(Rc::new(RefCell::new(Composing {
        editor: start.opening.editor.clone(),
        life: Lifecycle::new(&start.opening),
        debounce: None,
        owner: Owner::current(),
        inputs: [None; 3],
        rich: Rc::new(rich::Rich::new(&content.body, images)),
    })))));
    global::<Formatting>().set(Formatting::default());
    global::<Typed>().set(Typed {
        to: format_addresses(&content.to),
        cc: format_addresses(&content.cc),
        bcc: format_addresses(&content.bcc),
        subject: content.subject.clone(),
    });
    global::<From>().set(From {
        senders: senders(&identities),
        chosen: Some(draft.identity),
    });
    global::<Attachments>().set(Attachments::of(&draft.attachments));
    global::<Shown>().set(Shown {
        cc_bcc: !content.cc.is_empty() || !content.bcc.is_empty(),
        ..Shown::default()
    });
    let close_requests = start
        .close_requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(mut asks) = close_requests {
        spawn(async move {
            while asks.next().await.is_some() {
                close_requested();
            }
        });
    }
    spawn(follow_identities(draft.id));
    // The window is named after the subject.
    let title = computed(move || window_title(&global::<Typed>().with(|t| t.subject.clone())));
    chrome::follow_title(title);
    // Ctrl+W is the window's, which asks the view to close.
    on_key("mod+enter", |_| send());
    // The window's keys come first: Escape closes the link dialog, when it is open.
    on_key("escape", |_| {
        if global::<LinkDialog>().with_untracked(|d| d.0.is_some()) {
            close_link_dialog();
        } else {
            close_requested();
        }
    });
    on_key("mod+shift+a", |_| attach());
    on_key("mod+k", |_| ask_link());
    global::<LinkDialog>().set(LinkDialog::default());

    view()
        .flex_1()
        .bg(move || theme().background)
        .on_file_drop(dropped)
        .child(chrome::title_bar_of(title))
        .child(error())
        .child(fields(focus_body))
        .child(body(focus_body))
        .child(attachments())
        .child(footer())
        .child(link_dialog())
        .child(toast::toasts())
}

fn error() -> impl IntoElement {
    let error = move || global::<Shown>().with(|s| s.error.clone());
    when(
        move || error().is_some(),
        move || {
            view()
                .mx(px(20.0))
                .mt(px(12.0))
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(move || theme().destructive)
                .child(
                    text(move || error().unwrap_or_default())
                        .text_sm()
                        .selectable_text()
                        .text_color(move || theme().destructive),
                )
        },
    )
}

/// The field's name in a column, then the field.
fn field_row(label: &'static str) -> View {
    view()
        .flex_row()
        .items_center()
        .h(px(40.0))
        .gap(px(12.0))
        .child(
            text(label)
                .w(px(64.0))
                .flex_shrink_0()
                .text_sm()
                .text_color(move || theme().muted_foreground),
        )
}

/// A text field of the header, borderless in its row. Read-only while the window is
/// ending: what it is saving or sending was taken when that began.
fn header_input(value: String, on_change: impl Fn(&mut Typed, String) + 'static) -> Input {
    input()
        .default_value(value)
        .on_change(move |text: &str| {
            global::<Typed>().update(|t| on_change(t, text.to_owned()));
            changed();
        })
        .readonly(ending)
        .flex_1()
        .text_sm()
        .text_color(move || theme().foreground)
}

fn fields(focus_body: bool) -> impl IntoElement {
    let typed = global::<Typed>().with_untracked(Clone::clone);
    let border = move || theme().border;
    let cc_bcc = move || global::<Shown>().with(|s| s.cc_bcc);
    let show_cc_bcc = move || {
        global::<Shown>().update(|s| s.cc_bcc = true);
        focus(Field::Cc);
    };
    view()
        .flex_shrink_0()
        .px(px(20.0))
        .child(
            field_row("From")
                .border_b_1()
                .border_color(border)
                .child(from()),
        )
        .child(address_row(
            "To",
            Field::To,
            typed.to,
            !focus_body,
            when(
                move || !cc_bcc(),
                move || {
                    view()
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(6.0))
                        .on_press(show_cc_bcc)
                        .child(
                            text("Cc, Bcc")
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(move || theme().foreground),
                        )
                },
            )
            .into_element(),
        ))
        .child(when(cc_bcc, move || {
            let typed = global::<Typed>().with_untracked(Clone::clone);
            view()
                .child(address_row(
                    "Cc",
                    Field::Cc,
                    typed.cc,
                    false,
                    view().into_element(),
                ))
                .child(address_row(
                    "Bcc",
                    Field::Bcc,
                    typed.bcc,
                    false,
                    view().into_element(),
                ))
        }))
        .child(
            field_row("Subject")
                .border_b_1()
                .border_color(border)
                .child(
                    header_input(typed.subject, |t, text| t.subject = text)
                        .placeholder("Subject".to_owned()),
                ),
        )
}

/// From: who the draft is from, or a choice when it can be sent as more than one
/// identity.
fn from() -> impl IntoElement {
    let title = move || {
        global::<From>().with(|f| {
            f.senders
                .iter()
                .find(|s| Some(s.identity) == f.chosen)
                .map(|s| s.title.clone())
                .unwrap_or_default()
        })
    };
    let choice = move || global::<From>().with(|f| f.senders.len() > 1);
    view()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .min_w(px(0.0))
        .opacity(move || if choice() && ending() { 0.4_f32 } else { 1.0 })
        .on_press(move || {
            if untracked(choice) && !untracked(ending) {
                choose_sender();
            }
        })
        .child(
            text(title)
                .number_of_lines(1)
                .flex_shrink(1.0)
                .min_w(px(0.0))
                .text_sm()
                .text_color(move || theme().foreground),
        )
        .child(when(choice, || {
            icon(|| Icon::ChevronDown, 12.0).text_color(move || theme().muted_foreground)
        }))
}

fn address_row(
    label: &'static str,
    field: Field,
    value: String,
    auto_focus: bool,
    trailing: AnyElement,
) -> impl IntoElement {
    let input_ref = element_ref();
    composing().borrow_mut().inputs[field_index(field)] = Some(input_ref);
    let error = move || {
        global::<Shown>().with(|s| {
            s.field_error
                .as_ref()
                .filter(|(f, _)| *f == field)
                .map(|(_, message)| message.clone())
        })
    };
    let on_change = move |t: &mut Typed, text: String| match field {
        Field::To => t.to = text,
        Field::Cc => t.cc = text,
        Field::Bcc => t.bcc = text,
    };
    view()
        .border_b_1()
        .border_color(move || theme().border)
        .child(
            field_row(label)
                .child(
                    header_input(value, on_change)
                        .auto_focus(auto_focus)
                        .element_ref(&input_ref),
                )
                .child(trailing),
        )
        .child(when(
            move || error().is_some(),
            move || {
                text(move || error().unwrap_or_default())
                    // Under the field, past the label column and the row's gap.
                    .pl(px(76.0))
                    .pb(px(8.0))
                    .text_xs()
                    .text_color(move || theme().destructive)
            },
        ))
}

fn body(focus_body: bool) -> impl IntoElement {
    rich_editor()
        .editor(super::rich().session())
        .editable(move || !ending())
        .auto_focus(focus_body)
        .classes(rich::classes())
        .on_change(|_: &_| text_changed())
        .on_selection_change(|_: &_| formatting_moved())
        .on_paste_files(pasted)
        .flex_1()
        .min_h(px(0.0))
        .text_sm()
        .text_color(move || theme().foreground)
}

/// Many of them scroll, in no more than a few rows: the bar under them stays in the
/// window, however small.
fn attachments() -> impl IntoElement {
    when(
        move || global::<Attachments>().with(|a| !a.0.is_empty()),
        move || {
            scroll().flex_shrink_0().max_h(px(128.0)).child(
                view()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(8.0))
                    .px(px(20.0))
                    .py(px(10.0))
                    .child(each(
                        move || global::<Attachments>().with(|a| a.0.clone()),
                        |a| a.id,
                        |attachment, _| attachment_chip(attachment),
                    )),
            )
        },
    )
}

fn attachment_chip(attachment: ReadSignal<DraftAttachment>) -> impl IntoElement {
    let name = move || attachment.with(|a| a.filename.clone());
    view()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(30.0))
        .pl(px(10.0))
        .pr(px(2.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(move || theme().border)
        .child(icon(|| Icon::Document, 16.0).text_color(move || theme().muted_foreground))
        .child(
            text(move || attachment.with(|a| format!("{} · {}", a.filename, format::size(a.size))))
                .text_sm()
                .text_color(move || theme().foreground),
        )
        .child(icon_button(
            || Icon::XMark,
            move || format!("Remove {}", name()),
            move || !ending(),
            Size::Row,
            move || remove_attachment(attachment.get_untracked().id),
        ))
}

fn footer() -> impl IntoElement {
    let sending = move || global::<Shown>().with(|s| s.ending == Some(Finish::Send));
    let status = move || global::<Shown>().with(|s| s.status.clone());
    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .h(px(56.0))
        .px(px(16.0))
        .gap(px(8.0))
        .border_t_1()
        .border_color(move || theme().border)
        .child(primary_button(
            Icon::PaperAirplane,
            move || if sending() { "Sending" } else { "Send" }.to_owned(),
            || format!("Send ({})", key_label("mod+enter")),
            move || !ending(),
            send,
        ))
        .child(icon_button(
            || Icon::PaperClip,
            || "Attach files".to_owned(),
            move || !ending(),
            Size::Bar,
            attach,
        ))
        .child(icon_button(
            || Icon::Photo,
            || "Insert images".to_owned(),
            move || !ending(),
            Size::Bar,
            insert_images,
        ))
        .child(separator(Size::Bar))
        .children(FORMATS.map(format_button))
        .child(
            icon_button(
                || Icon::Link,
                || format!("Link ({})", key_label("mod+k")),
                move || !ending(),
                Size::Bar,
                ask_link,
            )
            .bg(move || {
                if formatted("link") {
                    theme().accent
                } else {
                    AdaptiveColor::TRANSPARENT
                }
            }),
        )
        .child(view().flex_1())
        .child(
            text(move || status().unwrap_or_default().into_owned())
                .text_xs()
                .text_color(move || theme().muted_foreground),
        )
        .child(icon_button(
            || Icon::Trash,
            || "Discard draft".to_owned(),
            move || !ending(),
            Size::Bar,
            discard,
        ))
}

/// The formatting the bar has: the icon, the name, the key, the mark or node it puts
/// on (or takes off) the selection, and the editor's command that does.
const FORMATS: [(Icon, &str, &str, &str, &str); 5] = [
    (Icon::Bold, "Bold", "mod+b", "bold", "toggleBold"),
    (Icon::Italic, "Italic", "mod+i", "italic", "toggleItalic"),
    (
        Icon::Underline,
        "Underline",
        "mod+u",
        "underline",
        "toggleUnderline",
    ),
    (
        Icon::ListBullet,
        "Bulleted list",
        "mod+shift+8",
        "bulletList",
        "toggleBulletList",
    ),
    (
        Icon::NumberedList,
        "Numbered list",
        "mod+shift+7",
        "orderedList",
        "toggleOrderedList",
    ),
];

/// One of [`FORMATS`]: on while the selection has its mark, or is in its node.
fn format_button(
    (which, name, key, active, command): (
        Icon,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
    ),
) -> View {
    icon_button(
        move || which,
        move || format!("{name} ({})", key_label(key)),
        move || !ending(),
        Size::Bar,
        move || format(command),
    )
    .bg(move || {
        if formatted(active) {
            theme().accent
        } else {
            AdaptiveColor::TRANSPARENT
        }
    })
}

/// Where the selection links to, asked while the dialog is open: an address, or none to
/// take the link off.
fn link_dialog() -> impl IntoElement {
    dynamic(move || match global::<LinkDialog>().get().0 {
        Some(href) => link_form(href).into_element(),
        None => view().into_element(),
    })
}

fn link_form(href: String) -> impl IntoElement {
    let value = signal(href.clone());
    let refused = signal(false);
    let submit = move || refused.set(!link_to(&value.get_untracked()));
    let length = href.chars().count() as u32;
    form::modal(
        400.0,
        close_link_dialog,
        view()
            .gap(px(12.0))
            .child(form::title("Link"))
            .child(
                form::text_input()
                    .default_value(href)
                    .placeholder("https://".to_owned())
                    .selected_range((0, length))
                    .auto_focus(true)
                    .on_change(move |typed: &str| {
                        value.set(typed.to_owned());
                        refused.set(false);
                    })
                    .on_submit(move |_: &str| submit()),
            )
            .child(when(
                move || refused.get(),
                || {
                    text("A link can't go there.")
                        .text_sm()
                        .text_color(move || theme().destructive)
                },
            ))
            .child(form::actions(
                Some(Rc::new(close_link_dialog)),
                "Link",
                || false,
                submit,
            )),
    )
}
