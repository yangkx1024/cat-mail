//! A message prepared for the pane.

use std::sync::Arc;

use catmail_core::html::remote_image_urls;
use catmail_core::{Attachment, Message, MessageId};
use catmail_frontend::reader::{self, Header};
use gpui_kit::SharedString;

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
    /// Whether replying to all writes to more than replying does.
    pub(super) can_reply_all: bool,
}

impl OpenMessage {
    /// Parses the whole HTML; not for the UI thread.
    pub(super) fn new(message: Message, can_reply_all: bool) -> Self {
        let header = Header::of(&message);
        let attachments = reader::attachments(&message);
        let (text, html) = match message.body {
            Some(body) => (
                body.text
                    .filter(|_| body.html.is_none())
                    .map(SharedString::from),
                body.html.map(Arc::from),
            ),
            None => (None, None),
        };
        Self {
            id: message.summary.id,
            subject: header.subject.into(),
            sender_key: header.sender_key.into(),
            sender: header.sender.into(),
            sender_email: header.sender_email.map(SharedString::from),
            recipients: header.recipients.into(),
            date: header.date.into(),
            headers: header
                .details
                .into_iter()
                .map(|(name, value)| (name, value.into()))
                .collect(),
            text,
            remote_images: html.as_deref().map(remote_image_urls).unwrap_or_default(),
            loading_remote: false,
            can_reply_all,
            html,
            attachments,
            images: Arc::default(),
        }
    }
}
