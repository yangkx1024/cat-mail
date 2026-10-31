//! How the pane looks: the message's header, body and attachments.

use gpui_kit::base::SelectableText;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _, Render, Styled as _, Window,
    div, prelude::FluentBuilder as _,
};

use crate::assets::Hero;
use crate::format;
use crate::layout;
use crate::theme::colors::ThemeColors as _;

use super::*;

impl Render for Reader {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content: AnyElement = match &self.state {
            ReaderState::Empty => layout::empty("No message selected", cx).into_any_element(),
            ReaderState::Loading(_) => h_flex()
                .size_full()
                .justify_center()
                .child(Spinner::new())
                .into_any_element(),
            ReaderState::Failed { error, .. } => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .p_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("Couldn't open the message: {error}")),
                )
                .child(
                    Button::new("retry")
                        .label("Try again")
                        .on_click(cx.listener(|this, _, window, cx| this.retry(window, cx))),
                )
                .into_any_element(),
            ReaderState::Open(open) => self.render_message(open, cx).into_any_element(),
        };
        div().size_full().bg(cx.theme().background).child(content)
    }
}

impl Reader {
    fn render_message(&self, open: &OpenMessage, cx: &mut Context<Self>) -> impl IntoElement {
        let id = open.id;
        let theme = cx.theme();
        let muted = theme.muted_foreground;

        let sender = h_flex()
            .items_start()
            .gap_3()
            .child(layout::avatar(&open.sender, &open.sender_key, cx).size_9())
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(open.sender.clone()),
                            )
                            .when_some(open.sender_email.clone(), |this, email| {
                                this.child(div().text_xs().text_color(muted).child(email))
                            })
                            .child(div().flex_1())
                            .child(div().text_xs().text_color(muted).child(open.date.clone())),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(muted)
                            .child(div().min_w_0().truncate().child(open.recipients.clone()))
                            .child(
                                Button::new("details")
                                    .ghost()
                                    .xsmall()
                                    .label(if self.show_details {
                                        "Hide details"
                                    } else {
                                        "Details"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_details = !this.show_details;
                                        cx.notify();
                                    })),
                            ),
                    ),
            );

        let details = self.show_details.then(|| {
            v_flex()
                .gap_1()
                .p_3()
                .rounded(theme.radius_lg)
                .bg(theme.muted)
                .text_xs()
                .children(
                    open.headers
                        .iter()
                        .map(|(name, value)| layout::labelled(*name, value.clone(), cx)),
                )
        });

        // Under the text, where reading it ends.
        let attachments = (!open.attachments.is_empty()).then(|| {
            h_flex()
                .flex_wrap()
                .gap_2()
                .pt_4()
                .border_t_1()
                .border_color(theme.border)
                .children(open.attachments.iter().map(|attachment| {
                    let name = attachment
                        .filename
                        .clone()
                        .unwrap_or_else(|| "Unnamed attachment".into());
                    let label = match attachment.size {
                        Some(size) => format!("{name} · {}", format::size(size.into())),
                        None => name.clone(),
                    };
                    let icon = if attachment.mime_type.starts_with("image/") {
                        Hero::Photo
                    } else {
                        Hero::Document
                    };
                    let attachment = attachment.clone();
                    Button::new(("attachment", attachment.id.0 as u64))
                        .outline()
                        .small()
                        .icon(icon)
                        .label(label)
                        .tooltip(format!("Save “{name}”"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.save_attachment(attachment.clone(), window, cx)
                        }))
                }))
        });

        // `show_page` keeps it the open message's.
        let body: AnyElement = match (&self.page, &open.text) {
            (Some(page), _) => page.view.clone().into_any_element(),
            (None, Some(text)) => {
                SelectableText::new(("message-text", id.0 as u64), text.clone()).into_any_element()
            }
            _ => div()
                .text_sm()
                .text_color(muted)
                .child("This message has no text")
                .into_any_element(),
        };

        let neutral = theme.neutral_badge();
        let title = v_flex()
            .gap_2()
            .child(layout::page_title(open.subject.clone()))
            .when(!self.labels.is_empty(), |this| {
                this.child(
                    h_flex().flex_wrap().gap_1().children(
                        self.labels
                            .iter()
                            .map(|name| layout::badge(name.clone(), neutral, cx)),
                    ),
                )
            });

        layout::page(
            ("message", id.0 as u64),
            v_flex()
                .gap_4()
                .child(title)
                .child(sender)
                .children(details)
                .child(body)
                .children(attachments),
        )
        .size_full()
    }
}
