//! A message prepared for the pane.

use std::borrow::Cow;
use std::sync::Arc;

use catmail_core::html::{ColorUsage, color_usage, unwrap_layout_tables};
use catmail_core::{Attachment, Message, MessageId};
use gpui_kit::SharedString;

use crate::format;
use crate::html;

use super::*;

/// What the pane shows of a message, prepared once: `render` runs every frame, and a
/// body handed to the views as the same `SharedString` isn't copied or compared again.
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
    pub(super) text: Option<SharedString>,
    /// The HTML as the text view gets it: layout tables unwrapped, since it lays a table
    /// cell out as inline text.
    pub(super) html: Option<SharedString>,
    /// The HTML as sanitized, for the browser and Blitz, which lay out tables themselves.
    /// Shared with the text view's when there was no table to unwrap, and with pages.
    pub(super) browser_html: Option<Arc<str>>,
    /// How the HTML colors itself: whether Blitz may show it dark.
    pub(super) colors: ColorUsage,
    /// Attachments offered as files, not the inline images.
    pub(super) attachments: Vec<Attachment>,
    pub(super) images: Arc<Images>,
    pub(super) blocked_images: bool,
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
        let (text, browser_html, attachments) = match message.body {
            Some(body) => (
                body.text.map(SharedString::from),
                body.html.map(Arc::from),
                body.attachments.into_iter().filter(|a| !a.inline).collect(),
            ),
            None => (None, None, Vec::new()),
        };
        // Most HTML has no layout to unwrap; the view then shares the browser's string.
        let html = browser_html
            .as_ref()
            .map(|source| match unwrap_layout_tables(source) {
                Cow::Borrowed(_) => SharedString::from(source.clone()),
                Cow::Owned(unwrapped) => unwrapped.into(),
            });
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
            html,
            blocked_images: browser_html
                .as_deref()
                .is_some_and(html::has_blocked_images),
            colors: browser_html
                .as_deref()
                .map_or(ColorUsage::Unset, color_usage),
            browser_html,
            attachments,
            images: Arc::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_view_gets_layout_tables_unwrapped() {
        let mail = crate::test_support::TestMail::start();
        let open = OpenMessage::new(mail.download("十月产品更新：新功能一览"));
        let (view, browser) = (open.html.unwrap(), open.browser_html.unwrap());
        // The newsletter nests its body in two layout tables around one data table.
        assert_eq!(browser.matches("<table").count(), 3, "{browser}");
        assert_eq!(view.matches("<table").count(), 1, "{view}");
        assert!(view.contains("<th align=\"left\">功能</th>"), "{view}");
        assert!(view.contains("十月产品更新</h1>"), "{view}");
        assert!(open.blocked_images, "the tracking pixel is still found");
    }
}
