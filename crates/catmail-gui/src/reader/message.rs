//! A message prepared for the pane.

use std::sync::Arc;

use catmail_core::html::remote_image_urls;
use catmail_core::{Attachment, Message, MessageId};
use gpui_kit::SharedString;

use crate::format;

use super::*;

/// What the pane shows of a message, prepared once: `render` runs every frame, and plain
/// text handed to the view as the same `SharedString` isn't copied or compared again.
pub(super) struct OpenMessage {
    pub(super) id: MessageId,
    pub(super) subject: SharedString,
    /// The sender's name, or address when there is no name.
    pub(super) sender: SharedString,
    /// The address, when the name took the first place.
    pub(super) sender_email: Option<SharedString>,
    /// What the avatar's color is picked by.
    pub(super) sender_key: SharedString,
    /// `To Bob, Carol · Cc Dan`, short names only.
    pub(super) recipients: SharedString,
    pub(super) date: SharedString,
    /// From, To, Cc, Reply-To and Date in full, those that are present, for "Details".
    pub(super) headers: Vec<(&'static str, SharedString)>,
    /// The plain text, kept only when there is no HTML: the pane shows the HTML instead.
    pub(super) text: Option<SharedString>,
    /// The HTML as sanitized, for the page and the browser.
    pub(super) html: Option<Arc<str>>,
    /// Attachments offered as files, not the inline images.
    pub(super) attachments: Vec<Attachment>,
    pub(super) images: Arc<Images>,
    /// The remote images the HTML refers to (`remote_image_urls`) that aren't shown yet:
    /// the cached ones show at once, the others when the user asks.
    pub(super) remote_images: Vec<String>,
    pub(super) loading_remote: bool,
}

impl OpenMessage {
    /// Parses the whole HTML; not for the UI thread.
    pub(super) fn new(message: Message) -> Self {
        let summary = &message.summary;
        let headers = [
            ("From", format::addresses(&summary.from)),
            ("To", format::addresses(&message.to)),
            ("Cc", format::addresses(&message.cc)),
            ("Reply-To", format::addresses(&message.reply_to)),
            ("Date", format::date(summary.date)),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| (name, value.into()))
        .collect();
        let from = summary.from.first();
        let sender = format::sender(&summary.from);
        let sender_email = from
            .filter(|a| a.name.is_some())
            .map(|a| SharedString::from(a.email.clone()));
        let recipients = [("To", &message.to), ("Cc", &message.cc)]
            .into_iter()
            .filter(|(_, list)| !list.is_empty())
            .map(|(name, list)| format!("{name} {}", format::names(list)))
            .collect::<Vec<_>>()
            .join(" · ");
        let (text, html, attachments) = match message.body {
            Some(body) => (
                body.text
                    .filter(|_| body.html.is_none())
                    .map(SharedString::from),
                body.html.map(Arc::from),
                body.attachments.into_iter().filter(|a| !a.inline).collect(),
            ),
            None => (None, None, Vec::new()),
        };
        Self {
            id: summary.id,
            subject: format::subject(summary.subject.as_deref()).into(),
            sender_key: from.map(|a| a.email.clone()).unwrap_or_default().into(),
            sender: sender.into(),
            sender_email,
            recipients: recipients.into(),
            date: format::date(summary.date).into(),
            headers,
            text,
            remote_images: html.as_deref().map(remote_image_urls).unwrap_or_default(),
            loading_remote: false,
            html,
            attachments,
            images: Arc::default(),
        }
    }
}
