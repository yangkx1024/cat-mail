//! The reading pane: one message's headers, body (plain text or HTML), and attachments.
//!
//! Opening a message downloads its body if needed, which leaves it unread on the server.
//! HTML is rendered by GPUI Kit's text view, or laid out as a page by one of the engines
//! of `catmail-render` ([`HtmlEngine`]), with images resolved here (see [`crate::html`]):
//! inline parts and `data:` URLs show, remote images never load.

mod images;
mod message;
mod page;
mod view;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use catmail_core::html::InlineImage;
use catmail_core::{Attachment, MailClient, MessageId, safe_filename};
use catmail_render::{Engine, Source};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AppContext as _, Context, Entity, Image, SharedString, Task, Window};

use crate::html;

use images::{Images, inline_images};
use message::OpenMessage;
use page::PageView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyFormat {
    Text,
    Html,
}

/// What draws the HTML: an engine laying it out as a browser would (Blitz, the default),
/// or GPUI Kit's text view, which lays out less but lets the text be selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HtmlEngine {
    Kit,
    Page(Engine),
}

impl HtmlEngine {
    /// In the order the format bar offers them.
    const ALL: [HtmlEngine; 2] = [HtmlEngine::Page(Engine::Blitz), HtmlEngine::Kit];

    fn name(self) -> &'static str {
        match self {
            HtmlEngine::Kit => "GPUI Kit",
            HtmlEngine::Page(engine) => engine.name(),
        }
    }
}

/// The page shown, with what it shows: a message's HTML by an engine, with the inline
/// images as they were when it was made.
struct ShownPage {
    message: MessageId,
    engine: Engine,
    images: Arc<Images>,
    view: Entity<PageView>,
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
    /// The engine last chosen for HTML; kept the same way.
    engine: HtmlEngine,
    page: Option<ShownPage>,
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
            engine: HtmlEngine::Page(Engine::Blitz),
            page: None,
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

    /// The strips of the page an engine has painted so far, if it shows one.
    #[cfg(test)]
    pub fn painted_strips(&self, cx: &gpui_kit::App) -> Option<usize> {
        let page = self.page.as_ref()?;
        Some(page.view.read(cx).painted_strips())
    }

    /// A pixel of the page's top strip, once painted.
    #[cfg(test)]
    pub fn page_pixel(&self, x: u32, y: u32, cx: &gpui_kit::App) -> Option<[u8; 4]> {
        self.page.as_ref()?.view.read(cx).top_pixel(x, y)
    }

    /// Whether the open message has HTML, which "Open in browser" needs.
    pub fn has_html(&self) -> bool {
        matches!(&self.state, ReaderState::Open(open) if open.browser_html.is_some())
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.state = ReaderState::Empty;
        self.page = None;
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
            this.update_in(cx, |this, window, cx| {
                this.state = state;
                this.show_page(window, cx);
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
            this.update_in(cx, |this, window, cx| {
                if let ReaderState::Open(open) = &mut this.state
                    && open.id == id
                {
                    open.images = Arc::new(Images {
                        inline: images,
                        by_url: Mutex::default(),
                    });
                    this.show_page(window, cx);
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

    fn set_format(&mut self, format: BodyFormat, window: &mut Window, cx: &mut Context<Self>) {
        self.preferred = format;
        self.show_page(window, cx);
        cx.notify();
    }

    fn set_engine(&mut self, engine: HtmlEngine, window: &mut Window, cx: &mut Context<Self>) {
        self.engine = engine;
        self.show_page(window, cx);
        cx.notify();
    }

    /// The format shown for `open`: the one chosen, if the message has it.
    fn format(&self, open: &OpenMessage) -> BodyFormat {
        match (self.preferred, &open.text, &open.html) {
            (BodyFormat::Html, _, Some(_)) | (_, None, Some(_)) => BodyFormat::Html,
            _ => BodyFormat::Text,
        }
    }

    /// Makes the page match what the pane shows: the open message's HTML by the chosen
    /// engine, or no page. A new message or engine makes a new one; inline images that
    /// arrived since it was made go to the page it has.
    fn show_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = match (&self.state, self.engine) {
            (ReaderState::Open(open), HtmlEngine::Page(engine))
                if self.format(open) == BodyFormat::Html =>
            {
                open.browser_html.as_ref().map(|html| (open, engine, html))
            }
            _ => None,
        };
        let Some((open, engine, html)) = wanted else {
            self.page = None;
            return;
        };
        let (message, images, colors) = (open.id, open.images.clone(), open.colors);
        let source = || Source {
            html: html.clone(),
            images: {
                let images = images.clone();
                Arc::new(move |url| images.bytes(url))
            },
        };
        if let Some(page) = &mut self.page
            && page.message == message
            && page.engine == engine
        {
            if !Arc::ptr_eq(&page.images, &images) {
                page.images = images.clone();
                page.view
                    .update(cx, |view, cx| view.set_source(source(), cx));
            }
            return;
        }
        let view = cx.new(|cx| PageView::new(engine, source(), colors, window, cx));
        self.page = Some(ShownPage {
            message,
            engine,
            images,
            view,
        });
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
        let client = self.client.clone();
        let images = open.images.clone();
        cx.spawn_in(window, async move |this, cx| {
            let page = cx
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
                    html::browser_page(&title, &body, inline)
                })
                .await;
            let written = client.hand_off("html", page.into_bytes()).await;
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

fn download_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| {
            dirs.download_dir()
                .map(|d| d.to_path_buf())
                .or_else(|| Some(dirs.home_dir().to_path_buf()))
        })
        .unwrap_or_else(std::env::temp_dir)
}
