//! How the composer looks: toolbar, header fields, attachments and the text.

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea};
use gpui_kit::component::select::Select;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    App, Context, Entity, ExternalPaths, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, rems,
};

use crate::actions::{self, Attach, CloseComposer, Send};
use crate::assets::Hero;
use crate::layout;

use super::*;

/// A line of the header: the field's name in a column, then the field.
fn field_row(label: &'static str, field: impl IntoElement, cx: &App) -> gpui_kit::Div {
    layout::labelled(div().text_sm().child(label), field, cx)
        .items_center()
        .h_10()
        .gap_3()
}

/// A text field of the header, borderless in its row. Read-only while the window is
/// ending: what it is saving or sending was taken when that began.
fn header_input(label: &'static str, input: &Entity<InputState>, ending: bool) -> Input {
    let id: SharedString = format!("composer-{}", label.to_lowercase()).into();
    Input::new(input)
        .id(id)
        .appearance(false)
        .p_0()
        .readonly(ending)
}

impl Composer {
    fn address_row(
        &self,
        label: &'static str,
        field: Field,
        input: &Entity<InputState>,
        trailing: Option<gpui_kit::AnyElement>,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let error = self
            .field_error
            .as_ref()
            .filter(|(f, _)| *f == field)
            .map(|(_, message)| message.clone());
        v_flex()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                field_row(
                    label,
                    header_input(label, input, self.finishing.is_some()),
                    cx,
                )
                .children(trailing),
            )
            .children(error.map(|error| {
                div()
                    .pb_2()
                    // Under the field, past the label column (`w_16`, see `layout::labelled`)
                    // and the row's gap (`gap_3`).
                    .pl(rems(4.75))
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error)
            }))
    }
}

impl Render for Composer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ending = self.finishing.is_some();
        let sending = self.finishing == Some(Finish::Send);
        let status: Option<SharedString> = match (&self.save_state, self.saving && !ending) {
            (_, true) => Some("Saving…".into()),
            (SaveState::Saved, _) if !self.dirty => Some("Draft saved".into()),
            (SaveState::Failed(e), _) => Some(format!("Not saved: {e}").into()),
            _ => None,
        };
        let toolbar = layout::toolbar(cx)
            .gap_2()
            .child(
                Button::new("send")
                    .primary()
                    .small()
                    .icon(Hero::PaperAirplane)
                    .label("Send")
                    .loading(sending)
                    .disabled(ending)
                    .tooltip_with_action("Send", &Send, Some(actions::COMPOSER))
                    .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
            )
            .child(
                Button::new("attach")
                    .ghost()
                    .small()
                    .icon(Hero::PaperClip)
                    .label("Attach")
                    .disabled(ending)
                    .tooltip_with_action("Attach files", &Attach, Some(actions::COMPOSER))
                    .on_click(cx.listener(|this, _, window, cx| this.attach(window, cx))),
            )
            .child(div().flex_1())
            .children(status.map(|status| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(status)
            }))
            .child(
                Button::new("discard")
                    .ghost()
                    .small()
                    .icon(Hero::Trash)
                    .disabled(ending)
                    .tooltip("Discard draft")
                    .on_click(cx.listener(|this, _, window, cx| this.discard(window, cx))),
            );

        let cc_toggle = (!self.show_cc_bcc).then(|| {
            Button::new("show-cc-bcc")
                .ghost()
                .xsmall()
                .label("Cc, Bcc")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.show_cc_bcc = true;
                    this.cc.update(cx, |cc, cx| cc.focus(window, cx));
                    cx.notify();
                }))
                .into_any_element()
        });
        let from = match &self.from {
            FromField::Choice { senders, .. } => Select::new(senders)
                .id("composer-from")
                .appearance(false)
                .small()
                .disabled(ending)
                .into_any_element(),
            FromField::One(_, title) => div()
                .text_sm()
                .truncate()
                .child(title.clone())
                .into_any_element(),
        };
        let fields = v_flex()
            .flex_none()
            .px_5()
            .child(
                field_row("From", from, cx)
                    .border_b_1()
                    .border_color(theme.border),
            )
            .child(self.address_row("To", Field::To, &self.to, cc_toggle, cx))
            .when(self.show_cc_bcc, |this| {
                this.child(self.address_row("Cc", Field::Cc, &self.cc, None, cx))
                    .child(self.address_row("Bcc", Field::Bcc, &self.bcc, None, cx))
            })
            .child(
                field_row(
                    "Subject",
                    header_input("Subject", &self.subject, ending),
                    cx,
                )
                .border_b_1()
                .border_color(theme.border),
            )
            .when(!self.attachments.is_empty(), |this| {
                this.child(
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .py_2p5()
                        .border_b_1()
                        .border_color(theme.border)
                        .children(self.attachments.iter().map(|attachment| {
                            let id = attachment.id;
                            layout::attachment_chip(
                                attachment.filename.clone(),
                                attachment.size,
                                cx,
                            )
                            .pr_1()
                            .child(
                                Button::new(("remove-attachment", id.0 as u64))
                                    .ghost()
                                    .xsmall()
                                    .icon(Hero::XMark)
                                    .tooltip(format!("Remove {}", attachment.filename))
                                    .disabled(ending)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.remove_attachment(id, window, cx)
                                    })),
                            )
                        })),
                )
            });

        v_flex()
            .id("composer")
            .key_context(actions::COMPOSER)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Send, window, cx| this.send(window, cx)))
            .on_action(cx.listener(|this, _: &CloseComposer, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|this, _: &Attach, window, cx| this.attach(window, cx)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.attach_paths(paths.paths().to_vec(), window, cx)
            }))
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(toolbar)
            .children(self.error.clone().map(|error| {
                div()
                    .px_5()
                    .pt_3()
                    .child(Alert::error("composer-error", error))
            }))
            .child(fields)
            .child(
                div().flex_1().min_h_0().px_3().py_2().child(
                    Textarea::new(&self.body)
                        .appearance(false)
                        .readonly(ending)
                        .h_full(),
                ),
            )
    }
}
