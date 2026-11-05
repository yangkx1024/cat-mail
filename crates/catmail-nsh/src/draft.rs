//! The right pane for a draft, as the GUI's (`mailbox/draft.rs`): what it says, and
//! editing or deleting it.

use std::rc::Rc;

use catmail_core::{Draft, DraftId};
use catmail_frontend::composer::ComposeFrom;
use catmail_frontend::format;
use nsh_base::{alert_dialog, theme};
use nsh_core::prelude::*;

use crate::app::{client, in_mailbox, report};
use crate::commands::{Size, icon_button, primary_button, separator};
use crate::composer;
use crate::icons::{Icon, icon};
use crate::list::{self, Selected};

/// The pane, while a draft is selected.
pub fn pane() -> impl IntoElement {
    let draft = computed(list::selected_draft);
    view()
        .flex_1()
        .min_w(px(0.0))
        .child(toolbar(draft))
        .child(dynamic(move || match draft.get() {
            Some(draft) => page(draft).into_element(),
            None => view().flex_1().into_element(),
        }))
}

fn toolbar(draft: Memo<Option<Rc<Draft>>>) -> impl IntoElement {
    let id = move || draft.with(|d| d.as_ref().map(|d| d.id));
    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .h(px(48.0))
        .px(px(12.0))
        .gap(px(2.0))
        .border_b_1()
        .border_color(move || theme().border)
        .child(icon_button(
            || Icon::ArrowLeft,
            || "Back".to_owned(),
            || true,
            Size::Bar,
            || global::<Selected>().set(Selected(None)),
        ))
        .child(separator(Size::Bar))
        .child(primary_button(
            Icon::PencilSquare,
            || "Edit".to_owned(),
            || "Edit".to_owned(),
            move || id().is_some(),
            move || {
                if let Some(id) = untracked(id) {
                    composer::open(ComposeFrom::Draft(id));
                }
            },
        ))
        .child(view().flex_1())
        .child(icon_button(
            || Icon::Trash,
            || "Delete draft".to_owned(),
            move || id().is_some(),
            Size::Bar,
            move || {
                if let Some(draft) = draft.get_untracked() {
                    confirm_delete(draft.id, format::subject(Some(&draft.content.subject)));
                }
            },
        ))
}

fn confirm_delete(id: DraftId, subject: &str) {
    let answer = alert_dialog()
        .title(format!("Delete the draft “{subject}”?"))
        .description("Its copy in the server's Drafts folder is deleted too.")
        .cancel("cancel", "Cancel")
        .destructive("delete", "Delete")
        .show();
    in_mailbox(async move {
        if answer
            .await
            .is_ok_and(|r| r.action.as_deref() == Some("delete"))
            && let Err(e) = client().delete_draft(id).await
        {
            report("Couldn't delete the draft", e);
        }
    });
}

/// What the draft says: the server's complaint if it couldn't keep a copy, its subject,
/// recipients, attachments and text.
fn page(draft: Rc<Draft>) -> impl IntoElement {
    let muted = move || theme().muted_foreground;
    let lines = [
        ("To", &draft.content.to),
        ("Cc", &draft.content.cc),
        ("Bcc", &draft.content.bcc),
    ]
    .into_iter()
    .filter(|(_, list)| !list.is_empty())
    .map(|(name, list)| {
        view()
            .flex_row()
            .items_start()
            .gap(px(12.0))
            .child(
                text(name)
                    .w(px(64.0))
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(muted),
            )
            .child(
                text(format::addresses(list))
                    .flex_1()
                    .text_sm()
                    .selectable(true)
                    .text_color(move || theme().foreground),
            )
            .into_element()
    })
    .collect::<Vec<_>>();
    let upload_error = draft.upload_error.clone().map(|error| {
        view()
            .gap(px(4.0))
            .px(px(12.0))
            .py(px(10.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(move || theme().destructive)
            .child(
                text("The server didn't save a copy of this draft")
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(move || theme().destructive),
            )
            .child(
                text(error)
                    .text_sm()
                    .selectable(true)
                    .text_color(move || theme().foreground),
            )
            .into_element()
    });
    let attachments = (!draft.attachments.is_empty()).then(|| {
        view()
            .flex_row()
            .flex_wrap()
            .gap(px(8.0))
            .children(draft.attachments.iter().map(|a| {
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(30.0))
                    .px(px(10.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(move || theme().border)
                    .child(icon(|| Icon::Document, 16.0).text_color(muted))
                    .child(
                        text(format!("{} · {}", a.filename, format::size(a.size)))
                            .text_sm()
                            .text_color(move || theme().foreground),
                    )
                    .into_element()
            }))
            .into_element()
    });
    scroll().flex_1().child(
        view()
            .px(px(24.0))
            .pt(px(20.0))
            .pb(px(32.0))
            .gap(px(12.0))
            .children(upload_error)
            .child(
                text(format::subject(Some(&draft.content.subject)).to_owned())
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .selectable(true)
                    .text_color(move || theme().foreground),
            )
            .children(lines)
            .children(attachments)
            .child(
                text(draft.content.text.clone())
                    .pt(px(12.0))
                    .border_t_1()
                    .border_color(move || theme().border)
                    .text_sm()
                    .selectable(true)
                    .text_color(move || theme().foreground),
            ),
    )
}
