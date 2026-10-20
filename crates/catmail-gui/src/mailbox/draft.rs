//! The right pane for a draft: what it says, and editing or deleting it.

use catmail_core::Draft;
use gpui_kit::base::SelectableText;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _,
};

use super::{Mailbox, report_failure};
use crate::assets::Hero;
use crate::composer::ComposeFrom;
use crate::format;
use crate::layout;

impl Mailbox {
    pub(super) fn render_draft(&self, draft: &Draft, cx: &mut Context<Self>) -> impl IntoElement {
        let id = draft.id;
        let subject = format::subject(Some(&draft.content.subject));
        let lines: Vec<_> = [
            ("To", &draft.content.to),
            ("Cc", &draft.content.cc),
            ("Bcc", &draft.content.bcc),
        ]
        .into_iter()
        .filter(|(_, list)| !list.is_empty())
        .map(|(name, list)| layout::labelled(name, format::addresses(list), cx).text_sm())
        .collect();
        let subject_for_delete = subject.clone();
        v_flex()
            .size_full()
            .child(
                layout::toolbar(cx)
                    .child(
                        Button::new("edit-draft")
                            .primary()
                            .small()
                            .icon(Hero::PencilSquare)
                            .label("Edit")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.compose(ComposeFrom::Draft(id), window, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("delete-draft")
                            .ghost()
                            .small()
                            .icon(Hero::Trash)
                            .tooltip("Delete draft")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_delete_draft(
                                    id,
                                    subject_for_delete.clone(),
                                    window,
                                    cx,
                                )
                            })),
                    ),
            )
            .child(
                layout::page(
                    "draft",
                    v_flex()
                        .gap_3()
                        .when_some(draft.upload_error.clone(), |this, error| {
                            this.child(
                                Alert::warning("upload-error", error)
                                    .title("The server didn't save a copy of this draft"),
                            )
                        })
                        .child(layout::page_title(subject))
                        .children(lines)
                        .when(!draft.attachments.is_empty(), |this| {
                            this.child(h_flex().flex_wrap().gap_2().children(
                                draft.attachments.iter().map(|a| {
                                    layout::attachment_chip(a.filename.clone(), a.size, cx)
                                }),
                            ))
                        })
                        .child(
                            div()
                                .pt_3()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .child(SelectableText::new(
                                    ("draft-text", id.0 as u64),
                                    SharedString::from(draft.content.text.clone()),
                                )),
                        ),
                )
                .flex_1()
                .min_h_0(),
            )
    }

    fn confirm_delete_draft(
        &mut self,
        id: catmail_core::DraftId,
        subject: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.client.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let client = client.clone();
            alert
                .title(format!("Delete the draft “{subject}”?"))
                .description("Its copy in the server's Drafts folder is deleted too.")
                .confirm()
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let client = client.clone();
                    report_failure(
                        "delete the draft",
                        async move { client.delete_draft(id).await },
                        window,
                        cx,
                    );
                    true
                })
        });
    }
}
