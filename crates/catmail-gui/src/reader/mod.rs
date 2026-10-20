//! The reading pane: one message's headers, body (plain text or HTML), and attachments.
//!
//! Opening a message downloads its body if needed, which leaves it unread on the server.
//! HTML is rendered by GPUI Kit's text view with images resolved here (see
//! [`crate::html`]): inline parts and `data:` URLs show, remote images never load.

mod images;
mod message;
mod view;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use catmail_core::html::InlineImage;
use catmail_core::{Attachment, MailClient, MessageId, safe_filename};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AppContext as _, Context, Image, SharedString, Task, Window};

use crate::html;

use images::{Images, inline_images};
use message::OpenMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyFormat {
    Text,
    Html,
}

enum ReaderState {
    Empty,
    Loading(MessageId),
    Failed { id: MessageId, error: SharedString },
    Open(Box<OpenMessage>),
}

pub struct Reader {
    client: MailClient,
    state: ReaderState,
    /// The format last chosen; kept while moving between messages.
    preferred: BodyFormat,
    /// Every header in full, instead of the one-line summary.
    show_details: bool,
    _load: Task<()>,
}

impl Reader {
    pub fn new(client: MailClient) -> Self {
        Self {
            client,
            state: ReaderState::Empty,
            preferred: BodyFormat::Text,
            show_details: false,
            _load: Task::ready(()),
        }
    }

    /// The message whose body is shown, once it has loaded.
    #[cfg(test)]
    pub fn open_message_id(&self) -> Option<MessageId> {
        match &self.state {
            ReaderState::Open(open) => Some(open.id),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn prefers_html(&self) -> bool {
        self.preferred == BodyFormat::Html
    }

    /// Whether the open message has HTML, which "Open in browser" needs.
    pub fn has_html(&self) -> bool {
        matches!(&self.state, ReaderState::Open(open) if open.browser_html.is_some())
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.state = ReaderState::Empty;
        self._load = Task::ready(());
        cx.notify();
    }

    /// Shows the message, downloading its body first if needed. Does nothing if it is
    /// already shown or loading.
    pub fn open(&mut self, id: MessageId, window: &mut Window, cx: &mut Context<Self>) {
        let shown = match &self.state {
            ReaderState::Loading(loading) => *loading == id,
            ReaderState::Open(open) => open.id == id,
            ReaderState::Empty | ReaderState::Failed { .. } => false,
        };
        if shown {
            return;
        }
        self.state = ReaderState::Loading(id);
        cx.notify();
        let client = self.client.clone();
        // Replacing the task cancels the load of the message shown before.
        self._load = cx.spawn_in(window, async move |this, cx| {
            let (state, inline) = match client.download_message(id).await {
                Ok(message) => {
                    let inline = inline_images(&message);
                    let open = cx
                        .background_spawn(async move { OpenMessage::new(message) })
                        .await;
                    (ReaderState::Open(Box::new(open)), inline)
                }
                Err(e) => {
                    let error = e.to_string().into();
                    (ReaderState::Failed { id, error }, Vec::new())
                }
            };
            this.update(cx, |this, cx| {
                this.state = state;
                cx.notify();
            })
            .ok();

            // The text shows first; inline images follow (a large message downloads them
            // part by part).
            let mut images = HashMap::new();
            for (attachment, format) in inline {
                let Some(content_id) = attachment.content_id.clone() else {
                    continue;
                };
                match client.attachment_content(attachment.id).await {
                    Ok(bytes) => {
                        images.insert(content_id, Arc::new(Image::from_bytes(format, bytes)));
                    }
                    Err(e) => tracing::warn!(attachment = %attachment.id, error = %e,
                        "failed to load inline image"),
                }
            }
            if images.is_empty() {
                return;
            }
            this.update(cx, |this, cx| {
                if let ReaderState::Open(open) = &mut this.state
                    && open.id == id
                {
                    open.images = Arc::new(Images {
                        inline: images,
                        by_url: Mutex::default(),
                    });
                    cx.notify();
                }
            })
            .ok();
        });
    }

    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let ReaderState::Failed { id, .. } = self.state {
            self.open(id, window, cx);
        }
    }

    fn set_format(&mut self, format: BodyFormat, cx: &mut Context<Self>) {
        self.preferred = format;
        cx.notify();
    }

    /// Asks where to save the attachment, then writes it there (downloading it first if
    /// it isn't stored yet).
    fn save_attachment(
        &mut self,
        attachment: Attachment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = safe_filename(attachment.filename.as_deref(), "attachment");
        let path = cx.prompt_for_new_path(&download_dir(), Some(&name));
        let client = self.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let saved = async {
                let Some(path) = path.await.context("the save dialog closed")?? else {
                    return Ok(None);
                };
                let bytes = client.attachment_content(attachment.id).await?;
                cx.background_spawn({
                    let path = path.clone();
                    async move { std::fs::write(&path, bytes) }
                })
                .await
                .with_context(|| format!("couldn't write {}", path.display()))?;
                anyhow::Ok(Some(path))
            }
            .await;
            this.update_in(cx, |_, window, cx| match saved {
                Ok(Some(path)) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    window.push_notification(Notification::info(format!("Saved “{name}”")), cx);
                }
                Ok(None) => {}
                Err(e) => window.push_notification(
                    Notification::error(format!("Couldn't save “{name}”: {e:#}")),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    /// Writes the message as a standalone page and hands it to the system browser, for
    /// HTML the text view can't lay out. Inline images go in as loaded for the pane.
    pub(crate) fn open_in_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ReaderState::Open(open) = &self.state else {
            return;
        };
        let Some(body) = open.browser_html.clone() else {
            return;
        };
        let title = open.subject.clone();
        let dir = self.client.config().handoff_dir();
        let images = open.images.clone();
        cx.spawn_in(window, async move |this, cx| {
            let written = cx
                .background_spawn(async move {
                    // Copied here, off the UI thread: they can be large.
                    let inline = images
                        .inline
                        .iter()
                        .map(|(content_id, image)| {
                            let image = InlineImage {
                                mime_type: image.format().mime_type().to_owned(),
                                bytes: image.bytes().to_vec(),
                            };
                            (content_id.clone(), image)
                        })
                        .collect();
                    write_browser_page(&dir, &html::browser_page(&title, &body, inline))
                })
                .await;
            this.update_in(cx, |_, window, cx| match written {
                Ok(path) => cx.open_with_system(&path),
                Err(e) => window.push_notification(
                    Notification::error(format!("Couldn't open the message in a browser: {e:#}")),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }
}

/// The page is left in `dir` (readable by this user only): the browser reads it after
/// this returns. The engine empties the directory when it next opens.
fn write_browser_page(dir: &Path, page: &str) -> anyhow::Result<PathBuf> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)?;
    let mut file = tempfile::Builder::new()
        .prefix("message-")
        .suffix(".html")
        .tempfile_in(dir)?;
    file.write_all(page.as_bytes())?;
    let (_, path) = file.keep()?;
    Ok(path)
}

fn download_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| {
            dirs.download_dir()
                .map(|d| d.to_path_buf())
                .or_else(|| Some(dirs.home_dir().to_path_buf()))
        })
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_pages_go_to_the_handoff_dir() {
        let data = tempfile::tempdir().unwrap();
        let dir = catmail_core::Config::with_data_dir(data.path()).handoff_dir();
        let first = write_browser_page(&dir, "<p>one</p>").unwrap();
        let second = write_browser_page(&dir, "<p>two</p>").unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with(&dir));
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "<p>two</p>");
    }
}
