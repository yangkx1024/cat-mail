//! The reading pane: one message's headers, body (plain text or HTML), and attachments.
//!
//! Opening a message downloads its body if needed, which leaves it unread on the server.
//! HTML is rendered by GPUI Kit's text view with images resolved here (see
//! [`crate::html`]): inline parts and `data:` URLs show, remote images never load.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, anyhow};
use catmail_core::html::{InlineImage, content_id_from_url};
use catmail_core::{Attachment, MailClient, Message, MessageId, safe_filename};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::SelectableText;
use gpui_kit::base::text::TextView;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, Selectable as _, Sizable as _, WindowExt as _};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, FontWeight, Image, ImageCacheError, ImageFormat,
    ImageSource, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    SharedUri, StatefulInteractiveElement as _, Styled as _, Task, Window, div,
    prelude::FluentBuilder as _,
};

use crate::format;
use crate::html;

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

/// What the pane shows of a message, prepared once: `render` runs every frame, and a
/// body handed to the views as the same `SharedString` isn't copied or compared again.
struct OpenMessage {
    id: MessageId,
    subject: SharedString,
    /// From, To, Cc and Date, those that are present.
    headers: Vec<(&'static str, SharedString)>,
    text: Option<SharedString>,
    html: Option<SharedString>,
    /// Attachments offered as files, not the inline images.
    attachments: Vec<Attachment>,
    images: Arc<Images>,
    blocked_images: bool,
}

impl OpenMessage {
    fn new(message: Message) -> Self {
        let summary = &message.summary;
        let headers = [
            ("From", format::addresses(&summary.from)),
            ("To", format::addresses(&message.to)),
            ("Cc", format::addresses(&message.cc)),
            ("Date", format::date(summary.date)),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| (name, value.into()))
        .collect();
        let body = message.body;
        Self {
            id: summary.id,
            subject: format::subject(summary).into(),
            headers,
            text: body.as_ref().and_then(|b| b.text.clone()).map(Into::into),
            html: body.as_ref().and_then(|b| b.html.clone()).map(Into::into),
            attachments: body
                .map(|b| b.attachments.into_iter().filter(|a| !a.inline).collect())
                .unwrap_or_default(),
            images: Arc::default(),
            blocked_images: false,
        }
    }
}

pub struct Reader {
    client: MailClient,
    state: ReaderState,
    /// The format last chosen; kept while moving between messages.
    preferred: BodyFormat,
    _load: Task<()>,
}

impl Reader {
    pub fn new(client: MailClient) -> Self {
        Self {
            client,
            state: ReaderState::Empty,
            preferred: BodyFormat::Text,
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
                    let mut open = OpenMessage::new(message);
                    if let Some(body) = open.html.clone() {
                        // Parses the whole HTML; not on the UI thread.
                        open.blocked_images = cx
                            .background_spawn(async move { html::has_blocked_images(&body) })
                            .await;
                    }
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
    fn open_in_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ReaderState::Open(open) = &self.state else {
            return;
        };
        let Some(body) = open.html.clone() else {
            return;
        };
        let title = open.subject.clone();
        let dir = self.client.config().page_dir();
        let inline = open
            .images
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
        cx.spawn_in(window, async move |this, cx| {
            let written = cx
                .background_spawn(async move {
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

/// The inline parts the HTML can show as images, with their formats.
fn inline_images(message: &Message) -> Vec<(Attachment, ImageFormat)> {
    let Some(body) = &message.body else {
        return Vec::new();
    };
    body.attachments
        .iter()
        .filter(|a| a.inline && a.content_id.is_some())
        .filter_map(|a| Some((a.clone(), raster_format(&a.mime_type)?)))
        .collect()
}

/// The image formats the pane renders. Not SVG: GPUI hands SVG to usvg with its default
/// options, whose resolver reads nested `<image href>` paths from the local disk, out of
/// `image_source`'s reach, so a message could show local files or read `/dev/zero`.
fn raster_format(mime_type: &str) -> Option<ImageFormat> {
    ImageFormat::from_mime_type(mime_type).filter(|f| !matches!(f, ImageFormat::Svg))
}

/// The page is left in `dir` (readable by this user only): the browser reads it after
/// this returns. [`clear_browser_pages`] removes it at the next start.
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

/// Removes the pages earlier sessions wrote, which hold whole messages. Done at start
/// rather than on quit: a browser may still be reading them then, and on macOS the app
/// exits without running destructors.
pub fn clear_browser_pages(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, dir = %dir.display(), "couldn't clear browser pages"),
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

/// The images of one open message, as the text view asks for them.
#[derive(Default)]
struct Images {
    /// Decoded inline parts, by Content-ID.
    inline: HashMap<String, Arc<Image>>,
    /// Every other URL, resolved once: the decoded `data:` image, or `None` for an image
    /// that isn't shown. The text view asks for every image on every frame.
    by_url: Mutex<HashMap<SharedUri, Option<Arc<Image>>>>,
}

impl Images {
    fn source(&self, url: &SharedUri) -> ImageSource {
        let image = match content_id_from_url(url) {
            Some(content_id) => self.inline.get(&content_id).cloned(),
            None => {
                let mut by_url = self.by_url.lock().unwrap_or_else(|e| e.into_inner());
                by_url
                    .entry(url.clone())
                    .or_insert_with(|| {
                        let (mime_type, bytes) = html::embedded_image(url)?;
                        let format = raster_format(&mime_type)?;
                        Some(Arc::new(Image::from_bytes(format, bytes)))
                    })
                    .clone()
            }
        };
        match image {
            Some(image) => ImageSource::Image(image),
            None => not_shown(),
        }
    }
}

/// An image source that fails at once, so nothing is fetched and the space stays empty.
fn not_shown() -> ImageSource {
    ImageSource::Custom(Arc::new(|_, _| {
        Some(Err(ImageCacheError::Other(Arc::new(anyhow!(
            "image not shown"
        )))))
    }))
}

impl Render for Reader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content: AnyElement = match &self.state {
            ReaderState::Empty => placeholder("No message selected", cx).into_any_element(),
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
            ReaderState::Open(open) => self.render_message(open, window, cx).into_any_element(),
        };
        div().size_full().bg(cx.theme().background).child(content)
    }
}

impl Reader {
    fn render_message(
        &self,
        open: &OpenMessage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (text, html) = (&open.text, &open.html);
        let format = match (self.preferred, text, html) {
            (BodyFormat::Html, _, Some(_)) | (_, None, Some(_)) => BodyFormat::Html,
            _ => BodyFormat::Text,
        };
        let id = open.id;
        let muted = cx.theme().muted_foreground;

        let headers = open.headers.iter().map(|(name, value)| {
            h_flex()
                .items_start()
                .gap_2()
                .text_sm()
                .child(div().w_12().flex_none().text_color(muted).child(*name))
                .child(div().flex_1().min_w_0().child(value.clone()))
        });

        let toolbar = h_flex()
            .gap_2()
            .when(text.is_some() && html.is_some(), |this| {
                this.child(
                    ButtonGroup::new("body-format")
                        .outline()
                        .small()
                        .child(
                            Button::new("format-text")
                                .label("Text")
                                .selected(format == BodyFormat::Text),
                        )
                        .child(
                            Button::new("format-html")
                                .label("HTML")
                                .selected(format == BodyFormat::Html),
                        )
                        .on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
                            match selected.first() {
                                Some(0) => this.set_format(BodyFormat::Text, cx),
                                Some(1) => this.set_format(BodyFormat::Html, cx),
                                _ => {}
                            }
                        })),
                )
            })
            .when(html.is_some(), |this| {
                this.child(
                    Button::new("open-in-browser")
                        .ghost()
                        .small()
                        .icon(Lucide::ExternalLink)
                        .label("Open in browser")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_in_browser(window, cx)),
                        ),
                )
            });

        let header_region = v_flex()
            .flex_none()
            .gap_2()
            .p_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(open.subject.clone()),
            )
            .child(v_flex().gap_1().children(headers))
            .when(!open.attachments.is_empty(), |this| {
                this.child(
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .children(open.attachments.iter().map(|attachment| {
                            let label = format!(
                                "{}{}",
                                attachment
                                    .filename
                                    .as_deref()
                                    .unwrap_or("Unnamed attachment"),
                                attachment
                                    .size
                                    .map(|s| format!(" · {}", format::size(s.into())))
                                    .unwrap_or_default()
                            );
                            let attachment = attachment.clone();
                            Button::new(("attachment", attachment.id.0 as u64))
                                .outline()
                                .small()
                                .icon(Lucide::Paperclip)
                                .label(label)
                                .tooltip("Save…")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.save_attachment(attachment.clone(), window, cx)
                                }))
                        })),
                )
            })
            .child(toolbar)
            .when(format == BodyFormat::Html && open.blocked_images, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(muted)
                        .child(Icon::new(Lucide::ImageOff).xsmall())
                        .child("Remote images are not loaded"),
                )
            });

        let body: AnyElement = match (format, text, html) {
            (BodyFormat::Html, _, Some(html)) => {
                let images = open.images.clone();
                TextView::html(("message-html", id.0 as u64), html.clone())
                    .selectable(true)
                    .image_source(move |url| images.source(url))
                    .on_link_click(|url, _, _, cx| {
                        if html::is_openable_link(url) {
                            cx.open_url(url);
                        }
                    })
                    .into_any_element()
            }
            (_, Some(text), _) => {
                SelectableText::new(("message-text", id.0 as u64), text.clone()).into_any_element()
            }
            _ => div()
                .text_sm()
                .text_color(muted)
                .child("This message has no text")
                .into_any_element(),
        };

        v_flex().size_full().child(header_region).child(
            div()
                .id(("message-body", id.0 as u64))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_4()
                .child(body),
        )
    }
}

fn placeholder(text: &'static str, cx: &Context<Reader>) -> impl IntoElement + use<> {
    h_flex()
        .size_full()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_is_never_rendered() {
        assert!(raster_format("image/svg+xml").is_none());
        assert!(matches!(raster_format("image/png"), Some(ImageFormat::Png)));
        assert!(matches!(
            raster_format("image/jpeg"),
            Some(ImageFormat::Jpeg)
        ));

        let images = Images::default();
        let svg = "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%3E\
                   %3Cimage%20href='/dev/zero'/%3E%3C/svg%3E";
        assert!(matches!(images.source(&svg.into()), ImageSource::Custom(_)));
        let png = "data:image/png;base64,iVBORw0KGgo=";
        assert!(matches!(images.source(&png.into()), ImageSource::Image(_)));
    }

    #[test]
    fn browser_pages_stay_in_the_data_dir_until_the_next_start() {
        let data = tempfile::tempdir().unwrap();
        let dir = catmail_core::Config::with_data_dir(data.path()).page_dir();
        let first = write_browser_page(&dir, "<p>one</p>").unwrap();
        let second = write_browser_page(&dir, "<p>two</p>").unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with(&dir));
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "<p>two</p>");

        clear_browser_pages(&dir);
        assert!(!first.exists() && !second.exists());
        // Nothing to clear is not an error, and pages can be written again.
        clear_browser_pages(&dir);
        assert!(write_browser_page(&dir, "<p>three</p>").unwrap().exists());
    }
}
