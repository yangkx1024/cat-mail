//! The reading pane: one message's headers, body (plain text or HTML), and attachments.
//!
//! Opening a message downloads its body if needed, which leaves it unread on the server.
//! HTML, when the message has it, is laid out as a page by Blitz (`catmail-render`), with
//! images resolved here (see [`crate::html`]): inline parts and `data:` URLs show, remote
//! images only when the user asks for them, one message at a time.

mod images;
mod message;
mod page;
mod view;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context as _;
use catmail_core::html::{InlineImage, RemoteImage};
use catmail_core::{Attachment, MailClient, MessageId, safe_filename};
use catmail_render::{Engine, Resolver, Source};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AppContext as _, Context, Entity, SharedString, Task, Window};

use crate::html;

use images::{ImageData, Images, inline_images};
use message::OpenMessage;
use page::PageView;

/// The page shown, and the message whose HTML it shows.
struct ShownPage {
    message: MessageId,
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
    page: Option<ShownPage>,
    /// Every header in full, instead of the one-line summary.
    show_details: bool,
    /// The names of the open message's user labels. The mailbox keeps them as the list
    /// has the message now: they change while it is open.
    labels: Vec<SharedString>,
    _load: Task<()>,
    /// Fetching the open message's remote images; replaced with the message.
    _remote: Task<()>,
}

impl Reader {
    pub fn new(client: MailClient) -> Self {
        Self {
            client,
            state: ReaderState::Empty,
            page: None,
            show_details: false,
            labels: Vec::new(),
            _load: Task::ready(()),
            _remote: Task::ready(()),
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

    /// The strips of the page Blitz has painted so far, if it shows one.
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
        matches!(&self.state, ReaderState::Open(open) if open.html.is_some())
    }

    /// Whether replying to all the open message writes to more than replying does.
    pub fn can_reply_all(&self) -> bool {
        matches!(&self.state, ReaderState::Open(open) if open.can_reply_all)
    }

    /// The open message's remote images still blocked, if it has any: whether they are
    /// being fetched.
    pub fn blocked_remote_images(&self) -> Option<bool> {
        match &self.state {
            ReaderState::Open(open) if !open.remote_images.is_empty() => Some(open.loading_remote),
            _ => None,
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.state = ReaderState::Empty;
        self.page = None;
        self.labels.clear();
        self._load = Task::ready(());
        self._remote = Task::ready(());
        cx.notify();
    }

    pub fn set_labels(&mut self, labels: Vec<SharedString>, cx: &mut Context<Self>) {
        if self.labels != labels {
            self.labels = labels;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn labels(&self) -> &[SharedString] {
        &self.labels
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
        self._remote = Task::ready(());
        cx.notify();
        let client = self.client.clone();
        // Replacing the task cancels the load of the message shown before.
        self._load = cx.spawn_in(window, async move |this, cx| {
            let began = Instant::now();
            let (state, inline) = match client.download_message(id).await {
                Ok(message) => {
                    let downloaded = began.elapsed();
                    // Offered when unsure: it only adds recipients the user can remove.
                    let can_reply_all = client.can_reply_all(id).await.unwrap_or_else(|e| {
                        tracing::warn!(message = %id, error = %e, "couldn't tell who replying to all writes to");
                        true
                    });
                    let inline = inline_images(&message);
                    let prepared = Instant::now();
                    let open = cx
                        .background_spawn(async move { OpenMessage::new(message, can_reply_all) })
                        .await;
                    tracing::info!(message = %id, ?downloaded, prepare = ?prepared.elapsed(),
                        "message opened");
                    (ReaderState::Open(Box::new(open)), inline)
                }
                Err(e) => {
                    tracing::warn!(message = %id, error = %e, "couldn't open the message");
                    let error = e.to_string().into();
                    (ReaderState::Failed { id, error }, Vec::new())
                }
            };
            this.update_in(cx, |this, window, cx| {
                this.state = state;
                this.show_page(window, cx);
                this.show_cached_remote_images(window, cx);
                cx.notify();
            })
            .ok();

            // The text shows first; inline images follow (a large message downloads them
            // part by part).
            let mut images = HashMap::new();
            for attachment in inline {
                let Some(content_id) = attachment.content_id.clone() else {
                    continue;
                };
                match client.attachment_content(attachment.id).await {
                    Ok(bytes) => {
                        let image = ImageData {
                            mime_type: attachment.mime_type,
                            bytes: bytes.into(),
                        };
                        images.insert(content_id, image);
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
                    // Remote images may have come first.
                    open.images = Arc::new(open.images.with_inline(images));
                    this.show_page(window, cx);
                    cx.notify();
                }
            })
            .ok();
        });
    }

    /// Shows the open message's remote images that are cached, alongside the inline parts
    /// loading: they need no request, so they show without asking.
    fn show_cached_remote_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ReaderState::Open(open) = &self.state else {
            return;
        };
        if open.remote_images.is_empty() {
            return;
        }
        let (id, urls) = (open.id, open.remote_images.clone());
        let client = self.client.clone();
        // Asking for them all replaces this: the fetch comes from the cache first anyway.
        self._remote = cx.spawn_in(window, async move |this, cx| {
            match client.cached_remote_images(urls).await {
                Ok(cached) if !cached.is_empty() => {
                    this.update_in(cx, |this, window, cx| {
                        this.add_remote_images(id, cached, window, cx)
                    })
                    .ok();
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "couldn't read the image cache"),
            }
        });
    }

    /// Fetches the open message's remote images and shows each as it comes, because the
    /// user asked. If none could be had, says why and leaves them blocked, to try again.
    /// Moving to another message drops the task, and with it the fetches still going.
    pub(crate) fn show_remote_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ReaderState::Open(open) = &mut self.state else {
            return;
        };
        if open.loading_remote || open.remote_images.is_empty() {
            return;
        }
        open.loading_remote = true;
        cx.notify();
        let (id, urls) = (open.id, open.remote_images.clone());
        let client = self.client.clone();
        self._remote = cx.spawn_in(window, async move |this, cx| {
            let (mut shown, mut error) = (false, None);
            match client.fetch_remote_images(urls).await {
                Ok(mut stream) => {
                    while let Some(first) = stream.next().await {
                        // Those that finished together go to the page together: one layout.
                        let ready = std::iter::from_fn(|| stream.next_ready());
                        let mut fetched = Vec::new();
                        for (url, image) in std::iter::once(first).chain(ready) {
                            match image {
                                Ok(image) => fetched.push((url, image)),
                                Err(e) => {
                                    tracing::debug!(%url, error = %e, "remote image not loaded");
                                    error.get_or_insert_with(|| e.to_string());
                                }
                            }
                        }
                        if fetched.is_empty() {
                            continue;
                        }
                        shown = true;
                        let added = this.update_in(cx, |this, window, cx| {
                            this.add_remote_images(id, fetched, window, cx)
                        });
                        if !matches!(added, Ok(true)) {
                            return;
                        }
                    }
                }
                Err(e) => error = Some(e.to_string()),
            }
            this.update_in(cx, |this, window, cx| {
                let ReaderState::Open(open) = &mut this.state else {
                    return;
                };
                if open.id != id {
                    return;
                }
                open.loading_remote = false;
                if !shown && let Some(error) = &error {
                    // Still to show: the button comes back, to try again.
                    window.push_notification(
                        Notification::error(format!("Couldn't load remote images: {error}")),
                        cx,
                    );
                } else {
                    // Those that failed stay blank.
                    open.remote_images.clear();
                }
                cx.notify();
            })
            .ok();
        });
    }

    /// Shows remote images that came for message `id` (fetched, or from the cache), with
    /// those shown already; false if it is no longer open.
    fn add_remote_images(
        &mut self,
        id: MessageId,
        fetched: Vec<(String, RemoteImage)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let ReaderState::Open(open) = &mut self.state else {
            return false;
        };
        if open.id != id {
            return false;
        }
        open.remote_images
            .retain(|url| !fetched.iter().any(|(had, _)| had == url));
        open.images = Arc::new(open.images.with_remote(fetched));
        self.show_page(window, cx);
        cx.notify();
        true
    }

    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let ReaderState::Failed { id, .. } = self.state {
            self.open(id, window, cx);
        }
    }

    /// Makes the page match what the pane shows: the open message's HTML, or no page. A
    /// new message makes a new one; otherwise the page gets the images that arrived.
    fn show_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = match &self.state {
            ReaderState::Open(open) => open.html.as_ref().map(|html| (open, html)),
            _ => None,
        };
        let Some((open, html)) = wanted else {
            self.page = None;
            return;
        };
        let message = open.id;
        let images = open.images.clone();
        let resolver: Resolver = Arc::new(move |url| images.bytes(url));
        if let Some(page) = &self.page
            && page.message == message
        {
            page.view
                .update(cx, |view, cx| view.set_images(resolver, cx));
            return;
        }
        let source = Source {
            html: html.clone(),
            images: resolver,
        };
        let view = cx.new(|cx| PageView::new(Engine::Blitz, source, window, cx));
        self.page = Some(ShownPage { message, view });
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
    /// HTML the pane can't lay out. Inline images go in as loaded for the pane.
    pub(crate) fn open_in_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ReaderState::Open(open) = &self.state else {
            return;
        };
        let Some(body) = open.html.clone() else {
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
                        .inline()
                        .map(|(content_id, image)| {
                            let image = InlineImage {
                                mime_type: image.mime_type.clone(),
                                bytes: image.bytes.to_vec(),
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
