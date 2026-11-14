//! The right pane for a draft: what it says, and editing or deleting it.

use std::rc::Rc;

use catmail_core::{Draft, DraftId};
use catmail_frontend::composer::ComposeFrom;
use catmail_frontend::composer::draft_deletion;
use catmail_frontend::format;
use nsh_base::theme;
use nsh_core::prelude::*;

use crate::buttons::{Size, icon_button, primary_button, separator};
use crate::engine::client;
use crate::form::confirmed;
use crate::icons::{Icon, icon};
use crate::mailbox::list::{self};
use crate::mailbox::panes::{back_button, fact};
use crate::tasks::in_view;
use crate::theme::SelectableText as _;
use crate::toast::report;
use crate::{chrome, composer};

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
    chrome::toolbar()
        .gap(px(2.0))
        .child(back_button())
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
                    confirm_delete(draft.id, Some(&draft.content.subject));
                }
            },
        ))
}

fn confirm_delete(id: DraftId, subject: Option<&str>) {
    let answer = confirmed(draft_deletion(subject));
    in_view(async move {
        if answer.await
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
    .map(|(name, list)| fact(name, format::addresses(list)).into_element())
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
                    .selectable_text()
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
                // The text below is plain: the images in the formatted one show here.
                let which = if a.in_text() {
                    Icon::Photo
                } else {
                    Icon::Document
                };
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(30.0))
                    .px(px(10.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(move || theme().border)
                    .child(icon(move || which, 16.0).text_color(muted))
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
                    .selectable_text()
                    .text_color(move || theme().foreground),
            )
            .children(lines)
            .children(attachments)
            .child(
                text(draft.content.body.text.clone())
                    .pt(px(12.0))
                    .border_t_1()
                    .border_color(move || theme().border)
                    .text_sm()
                    .selectable_text()
                    .text_color(move || theme().foreground),
            ),
    )
}
