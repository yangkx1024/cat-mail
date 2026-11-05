//! The reading pane, as the GUI's (`reader/`, the message toolbar of `mailbox/panes.rs`):
//! the commands on the message, its header, its body in a web view and its attachments.
//!
//! The body is a page in one web view (see `page.rs`): opening a message loads a new
//! page into it. The view stays while messages are opened one after another, for WebKit
//! starts a web process for each new view (100 MB, a sandbox to set up), and ends it when
//! the last goes. Remote images stay blocked until the user asks for them, except those
//! the engine has cached; those that come are sent to the page (`page::image_message`),
//! which shows them without loading again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use catmail_core::html::{
    DarkPalette, InlineImage, RemoteImage, data_url, recolor_html, remote_image_urls,
};
use catmail_core::{Attachment, MessageId, safe_filename};
use catmail_frontend::commands::MessageCommand;
use catmail_frontend::html::{browser_page, is_openable_link};
use catmail_frontend::list as rows;
use catmail_frontend::reader::{self, Header};
use nsh_base::theme;
use nsh_core::element::{ElementRef, Spawned, open_path, pick_save_path};
use nsh_core::file_dialog::SaveDialog;
use nsh_core::prelude::*;

use crate::app::{client, in_mailbox};
use crate::commands::{self, Size, icon_button, separator};
use crate::composer;
use crate::icons::{Icon, icon};
use crate::list::{self, Selected};
use crate::page::{self, Colors, FromPage};
use crate::toast::{Level, notify};

#[derive(Clone, Default, PartialEq)]
enum State {
    #[default]
    Empty,
    Loading,
    Failed(String),
    Open,
}

/// The message the pane shows.
#[derive(Clone, Default)]
struct Reading {
    id: Option<MessageId>,
    state: State,
    header: Header,
    attachments: Vec<Attachment>,
    /// The HTML as sanitized, for "Open in browser".
    html: Option<Rc<str>>,
    /// The inline parts loaded, by Content-ID: in the page, and in the browser's.
    inline: Rc<HashMap<String, InlineImage>>,
    page: String,
    /// Hidden while a message loads, past [`STAYS_SHOWN`] (or with nothing before it to
    /// show).
    hidden: bool,
    /// Whether the page has the dark theme's colors: the web view's background, which
    /// shows until the page paints, is the page's.
    page_dark: bool,
    /// The remote images the HTML refers to that aren't shown yet: the cached ones show
    /// at once, the others when the user asks.
    remote: Vec<String>,
    loading_remote: bool,
    /// Replying to all writes to more than replying does.
    can_reply_all: bool,
}

/// How long the message before stays shown while the next loads.
const STAYS_SHOWN: Duration = Duration::from_millis(250);

/// A hidden page shows once it has loaded, or after this if it doesn't say so.
const SHOWN_ANYWAY: Duration = Duration::from_secs(1);

/// The web view's height before the page says its own.
const MIN_BODY_HEIGHT: f32 = 40.0;

/// The body's height in points, as the page reports it.
#[derive(Clone, Default)]
struct BodyHeight(f32);

/// Bumped by "Try again": the message opens again.
#[derive(Clone, Default)]
struct Retry(u64);

/// Every header in full, instead of the one-line summary.
#[derive(Clone, Default)]
struct ShowDetails(bool);

/// What the page shows besides itself, and the fetch of remote images. Not reactive.
#[derive(Default)]
struct Page {
    /// The web view, for sending it images.
    view: Option<ElementRef>,
    /// The message's scroll view, taken back to the top for the next message.
    scroll: Option<ElementRef>,
    /// The message whose page was made last.
    shown: Option<MessageId>,
    /// The number of the page made last (`page::body_page`): what other pages say, late,
    /// is ignored.
    number: u64,
    /// The remote images shown, as `data:` URLs by URL: sent again when the page loads
    /// again (the appearance changed).
    remote: Vec<(String, String)>,
    /// The page loaded: images can be sent to it.
    ready: bool,
    /// Fetching the open message's remote images; dropped with the message.
    fetch: Option<Spawned<()>>,
}

#[derive(Clone, Default)]
struct PageState(Rc<RefCell<Page>>);

fn page_state() -> Rc<RefCell<Page>> {
    global::<PageState>().with_untracked(|p| p.0.clone())
}

/// The number of a page about to be made, which is the one shown from now on.
fn next_page() -> u64 {
    let page = page_state();
    let mut page = page.borrow_mut();
    page.number += 1;
    page.number
}

/// Whether what page `number` says is the page shown's.
fn is_shown(number: u64) -> bool {
    page_state().borrow().number == number
}

/// Opens the message selected; anything else empties the pane (drafts and the outbox
/// have panes of their own). Run in an effect: the task belongs to its run,
/// so opening another message cancels what is still loading for the last one.
pub fn follow_selection() {
    // Read here so the page is made again when the appearance changes.
    let dark = dark();
    global::<Retry>().with(|r| r.0);
    let selected = list::selected_id();
    let id = untracked(|| global::<Reading>().with(|r| r.id));
    if id != selected {
        global::<ShowDetails>().set(ShowDetails(false));
        let page = page_state();
        let mut page = page.borrow_mut();
        page.remote.clear();
        if let Some(fetch) = page.fetch.take() {
            fetch.cancel();
        }
    }
    match selected {
        Some(id) => {
            spawn(open_message(id, dark));
        }
        // A blank page in the web view, which stays: its web process is running when a
        // message opens (it takes a moment to start).
        None => global::<Reading>().set(Reading {
            page: page::blank_page(next_page(), &colors(dark)),
            page_dark: dark,
            ..Reading::default()
        }),
    }
}

async fn open_message(id: MessageId, dark: bool) {
    let reading = global::<Reading>();
    if reading.with_untracked(|r| r.id != Some(id)) {
        // The message before stays shown, and its page until this one's paints (WebKit
        // keeps it), as a browser goes from page to page: emptied in between, the pane
        // would show its own background where the message's was. Only if this one takes
        // long does it go.
        reading.update(|r| {
            r.id = Some(id);
            let was = std::mem::replace(&mut r.state, State::Loading);
            r.html = None;
            // The pane was closed: nothing before it to show.
            if was == State::Empty {
                r.page_dark = dark;
                r.hidden = true;
            }
        });
        in_mailbox(async move {
            timer(STAYS_SHOWN).await;
            let reading = global::<Reading>();
            if reading.with_untracked(|r| r.id == Some(id) && r.state == State::Loading) {
                reading.update(|r| r.hidden = true);
            }
        });
    }
    let message = match client().download_message(id).await {
        Ok(message) => message,
        Err(e) => {
            tracing::warn!(message = %id, error = %e, "couldn't open the message");
            reading.update(|r| r.state = State::Failed(e.to_string()));
            return;
        }
    };
    // Offered when unsure: it only adds recipients the user can remove.
    let can_reply_all = client().can_reply_all(id).await.unwrap_or_else(|e| {
        tracing::warn!(message = %id, error = %e, "couldn't tell who replying to all writes to");
        true
    });
    let body = message.body.as_ref();
    let mut inline = HashMap::new();
    if let Some(body) = body.filter(|b| b.html.is_some()) {
        for part in &body.attachments {
            let Some(content_id) = &part.content_id else {
                continue;
            };
            if !part.inline || !part.mime_type.starts_with("image/") {
                continue;
            }
            match client().attachment_content(part.id).await {
                Ok(bytes) => {
                    let image = InlineImage {
                        mime_type: part.mime_type.clone(),
                        bytes,
                    };
                    inline.insert(content_id.clone(), image);
                }
                Err(e) => tracing::warn!(attachment = %part.id, error = %e,
                    "failed to load inline image"),
            }
        }
    }
    // For checking what the page may load: this file's HTML in place of the message's.
    let probe = std::env::var_os("CATMAIL_NSH_HTML").and_then(|f| std::fs::read_to_string(f).ok());
    let html = probe.as_deref().or(body.and_then(|b| b.html.as_deref()));
    let shown: Vec<String> = page_state()
        .borrow()
        .remote
        .iter()
        .map(|(url, _)| url.clone())
        .collect();
    let remote = html
        .map(remote_image_urls)
        .unwrap_or_default()
        .into_iter()
        .filter(|url| !shown.contains(url))
        .collect::<Vec<_>>();
    // A dark page moves the message's colors toward the theme's, as the GUI does; one
    // whose colors come from CSS variables can't be moved and stays light.
    let recolored = html
        .filter(|_| dark)
        .and_then(|html| recolor_html(html, palette()));
    let page_dark = page_is_dark(dark, html.is_some(), recolored.is_some());
    let page_html = recolored.as_deref().or(html);
    let page = page::body_page(
        next_page(),
        page_html,
        body.and_then(|b| b.text.as_deref()),
        copy_inline(&inline),
        &colors(page_dark),
    );
    {
        let page = page_state();
        let mut page = page.borrow_mut();
        page.ready = false;
        // Another message (not this one again, in a new appearance): from its top.
        if page.shown.replace(id) != Some(id)
            && let Some(scroll) = page.scroll
        {
            scroll.scroll_to(Point::new(px(0.0), px(0.0)), false);
        }
    }
    // Hidden (the first page, or one that took long): until the page has loaded, rather
    // than show the web view before it paints, in WebKit's own background.
    let hidden = reading.with_untracked(|r| r.hidden);
    if hidden {
        in_mailbox(async move {
            timer(SHOWN_ANYWAY).await;
            show_page(id);
        });
    }
    reading.set(Reading {
        id: Some(id),
        state: State::Open,
        header: Header::of(&message),
        attachments: reader::attachments(&message),
        html: html.map(Rc::from),
        inline: Rc::new(inline),
        page,
        hidden,
        page_dark,
        remote: remote.clone(),
        loading_remote: false,
        can_reply_all,
    });
    // Those cached need no request: they show without asking.
    if remote.is_empty() {
        return;
    }
    match client().cached_remote_images(remote).await {
        Ok(cached) if !cached.is_empty() => {
            add_remote_images(id, cached);
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "couldn't read the image cache"),
    }
}

/// Fetches the open message's remote images and shows each as it comes, because the user
/// asked. If none could be had, says why and leaves them blocked, to try again.
fn show_remote_images() {
    let reading = global::<Reading>();
    let Some((id, urls)) = reading.with_untracked(|r| {
        (!r.loading_remote && !r.remote.is_empty()).then(|| (r.id, r.remote.clone()))
    }) else {
        return;
    };
    let Some(id) = id else {
        return;
    };
    reading.update(|r| r.loading_remote = true);
    let task = spawn(async move {
        let (mut shown, mut error) = (false, None);
        match client().fetch_remote_images(urls).await {
            Ok(mut stream) => {
                while let Some(first) = stream.next().await {
                    // Those that finished together go to the page together.
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
                    if !add_remote_images(id, fetched) {
                        return;
                    }
                }
            }
            Err(e) => error = Some(e.to_string()),
        }
        if global::<Reading>().with_untracked(|r| r.id != Some(id)) {
            return;
        }
        let keep = !shown && error.is_some();
        global::<Reading>().update(|r| {
            r.loading_remote = false;
            // Those that failed stay blank; with none shown, the button comes back.
            if !keep {
                r.remote.clear();
            }
        });
        if keep && let Some(error) = error {
            notify(
                Level::Error,
                format!("Couldn't load remote images: {error}"),
            );
        }
    });
    // Under the mailbox: not the button's, which goes when the images are all shown.
    if let Some(old) = page_state().borrow_mut().fetch.replace(task) {
        old.cancel();
    }
}

/// Shows remote images that came for message `id` (fetched, or from the cache); false if
/// it is no longer open.
fn add_remote_images(id: MessageId, fetched: Vec<(String, RemoteImage)>) -> bool {
    let reading = global::<Reading>();
    if reading.with_untracked(|r| r.id != Some(id)) {
        return false;
    }
    let images: Vec<(String, String)> = fetched
        .into_iter()
        .map(|(url, image)| {
            let data = data_url(&image.mime_type, &image.bytes);
            (url, data)
        })
        .collect();
    reading.update(|r| {
        r.remote
            .retain(|url| !images.iter().any(|(had, _)| had == url));
    });
    let page = page_state();
    let mut page = page.borrow_mut();
    if page.ready
        && let Some(view) = page.view
    {
        for (url, data) in &images {
            view.post_message(page::image_message(url, data));
        }
    }
    page.remote.extend(images);
    true
}

/// The page loaded: it gets the remote images shown so far.
fn page_ready() {
    let page = page_state();
    let mut page = page.borrow_mut();
    page.ready = true;
    if let Some(view) = page.view {
        for (url, data) in &page.remote {
            view.post_message(page::image_message(url, data));
        }
    }
}

/// The page has painted with the web view `height` points tall: a hidden one shows
/// once that is the page's own height. The first page grows the web view, and WebKit
/// shows the part grown blank until it paints there.
fn page_painted(height: f32) {
    let page_height = global::<BodyHeight>().with_untracked(|h| h.0);
    if !tall_enough(height, page_height) {
        return;
    }
    let shown = page_state().borrow().shown;
    if let Some(id) = shown {
        show_page(id);
    }
}

/// Whether a web view `height` points tall shows all of a page `page_height` points tall
/// (as tall as it is made, [`MIN_BODY_HEIGHT`] at least; to the pixel).
fn tall_enough(height: f32, page_height: f32) -> bool {
    height + 1.0 >= page_height.max(MIN_BODY_HEIGHT)
}

/// Shows message `id`'s page if it is the one open and still hidden.
fn show_page(id: MessageId) {
    let reading = global::<Reading>();
    if reading.with_untracked(|r| r.hidden && r.state == State::Open && r.id == Some(id)) {
        reading.update(|r| r.hidden = false);
    }
}

/// The inline parts, for a page of their own.
fn copy_inline(inline: &HashMap<String, InlineImage>) -> HashMap<String, InlineImage> {
    inline
        .iter()
        .map(|(id, image)| {
            let image = InlineImage {
                mime_type: image.mime_type.clone(),
                bytes: image.bytes.clone(),
            };
            (id.clone(), image)
        })
        .collect()
}

fn palette() -> DarkPalette {
    let rgb = |c: AdaptiveColor| {
        let c = c.dark.to_rgb();
        [c.r, c.g, c.b].map(|v| (v * 255.0).round() as u8)
    };
    DarkPalette {
        background: rgb(theme().background),
        foreground: rgb(theme().foreground),
    }
}

/// Whether a message's page takes the dark theme's colors: when the reader is dark and
/// the message's HTML could be moved toward them, or it is plain text, which has no
/// colors of its own. HTML that couldn't be moved stays light.
fn page_is_dark(dark: bool, html: bool, recolored: bool) -> bool {
    dark && (recolored || !html)
}

fn colors(dark: bool) -> Colors {
    let pick = |c: AdaptiveColor| if dark { c.dark } else { c.light }.to_hex();
    Colors {
        foreground: pick(theme().foreground),
        background: pick(theme().background),
        link: pick(crate::theme::link()),
    }
}

/// Asks where to save the attachment, then writes it there (downloading it first if it
/// isn't stored yet).
fn save_attachment(attachment: Attachment) {
    let name = safe_filename(attachment.filename.as_deref(), "attachment");
    let dialog = SaveDialog::new()
        .file_name(name.clone())
        .directory(reader::download_dir());
    let path = pick_save_path(dialog);
    in_mailbox(async move {
        let saved = async {
            let Some(path) = path.await? else {
                return Ok(None);
            };
            let bytes = client()
                .attachment_content(attachment.id)
                .await
                .map_err(|e| e.to_string())?;
            write_off_thread(path.clone(), bytes)
                .await
                .map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
            Ok::<_, String>(Some(path))
        }
        .await;
        match saved {
            Ok(Some(path)) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                notify(Level::Info, format!("Saved “{name}”"));
            }
            Ok(None) => {}
            Err(e) => notify(Level::Error, format!("Couldn't save “{name}”: {e}")),
        }
    });
}

/// Writes the file on a thread of its own: an attachment can be large.
async fn write_off_thread(path: std::path::PathBuf, bytes: Vec<u8>) -> std::io::Result<()> {
    let (done, written) = futures_channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = done.send(std::fs::write(&path, bytes));
    });
    written
        .await
        .unwrap_or_else(|_| Err(std::io::Error::other("the writer went away")))
}

/// Writes the message as a standalone page and hands it to the system browser. The
/// inline parts go in as loaded; remote images stay blocked there.
fn open_in_browser() {
    let Some((title, html, inline)) = global::<Reading>().with_untracked(|r| {
        let html = r.html.clone()?;
        Some((r.header.subject.clone(), html, r.inline.clone()))
    }) else {
        return;
    };
    in_mailbox(async move {
        let page = browser_page(&title, &html, copy_inline(&inline));
        match client().hand_off("html", page.into_bytes()).await {
            Ok(path) => {
                if let Err(e) = open_path(path).await {
                    notify(
                        Level::Error,
                        format!("Couldn't open the message in a browser: {e}"),
                    );
                }
            }
            Err(e) => notify(
                Level::Error,
                format!("Couldn't open the message in a browser: {e:#}"),
            ),
        }
    });
}

// --- The pane -----------------------------------------------------------------------

/// The pane, while a message is selected: its toolbar over its page.
/// The pane: its toolbar over the message. It stays while no message is selected
/// (`app::panes` keeps it out of sight), and so does its web view.
pub fn reader() -> View {
    let failed_ = computed(|| {
        global::<Reading>().with(|r| match &r.state {
            State::Failed(error) => Some(error.clone()),
            _ => None,
        })
    });
    view()
        .flex_1()
        .min_w(px(0.0))
        .child(toolbar())
        .child(dynamic(move || match failed_.get() {
            Some(error) => failed(error).into_element(),
            None => view().into_element(),
        }))
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .style_when(move || failed_.with(Option::is_some), |s| s.hidden())
                .child(message()),
        )
}

fn failed(error: String) -> impl IntoElement {
    view()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(12.0))
        .p(px(16.0))
        .child(
            text(format!("Couldn't open the message: {error}"))
                .text_sm()
                .selectable(true)
                .text_color(move || theme().muted_foreground),
        )
        .child(
            view()
                .h(px(32.0))
                .px(px(14.0))
                .rounded(px(16.0))
                .justify_center()
                .bg(move || theme().secondary)
                .on_press(|| {
                    // Opened again from the top.
                    global::<Reading>().update(|r| r.id = None);
                    global::<Retry>().update(|r| r.0 += 1);
                })
                .child(
                    text("Try again")
                        .text_sm()
                        .text_color(move || theme().secondary_foreground),
                ),
        )
}

/// The commands on the message open, in groups: where it is, its marks, its content,
/// answering it.
fn toolbar() -> impl IntoElement {
    let selected = move || list::selected_message().into_iter().collect::<Vec<_>>();
    let can_edit_labels =
        move || list::selected_message().is_some_and(|m| commands::can_edit_labels(&m));
    let blocked = move || {
        global::<Reading>()
            .with(|r| (r.state == State::Open && !r.remote.is_empty()).then_some(r.loading_remote))
    };
    let has_html = move || global::<Reading>().with(|r| r.html.is_some());
    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .h(px(48.0))
        .px(px(12.0))
        .gap(px(2.0))
        .border_b_1()
        .border_color(move || theme().border)
        .child(icon_button(
            || Icon::ArrowLeft,
            || "Back".to_owned(),
            || true,
            Size::Bar,
            || global::<Selected>().set(Selected(None)),
        ))
        .child(separator(Size::Bar))
        .child(commands::group(&MessageCommand::PLACE, selected, Size::Bar))
        .child(when(can_edit_labels, || {
            view().flex_row().child(icon_button(
                || Icon::Tag,
                || "Labels".to_owned(),
                || true,
                Size::Bar,
                commands::edit_labels,
            ))
        }))
        .child(separator(Size::Bar))
        .child(commands::group(&MessageCommand::MARKS, selected, Size::Bar))
        .child(separator(Size::Bar))
        .child(when(
            move || blocked().is_some(),
            move || {
                // Dimmed while they come.
                icon_button(
                    || Icon::Photo,
                    || "Show remote images".to_owned(),
                    move || blocked() == Some(false),
                    Size::Bar,
                    show_remote_images,
                )
            },
        ))
        .child(icon_button(
            || Icon::ArrowTopRightOnSquare,
            || "Open in browser".to_owned(),
            has_html,
            Size::Bar,
            open_in_browser,
        ))
        .child(separator(Size::Bar))
        .child(icon_button(
            || Icon::ArrowUturnLeft,
            || "Reply".to_owned(),
            || true,
            Size::Bar,
            || composer::reply(false),
        ))
        .child(when(
            move || global::<Reading>().with(|r| r.can_reply_all),
            || {
                icon_button(
                    || Icon::ArrowUturnLeftDouble,
                    || "Reply all".to_owned(),
                    || true,
                    Size::Bar,
                    || composer::reply(true),
                )
            },
        ))
        .child(icon_button(
            || Icon::ArrowUturnRight,
            || "Forward".to_owned(),
            || true,
            Size::Bar,
            composer::forward,
        ))
}

/// The message: its title, sender, the body and the attachments, scrolling as one.
fn message() -> impl IntoElement {
    let reading = global::<Reading>();
    let header = computed(move || reading.with(|r| r.header.clone()));
    let field = move |f: fn(&Header) -> String| move || header.with(f);
    let labels = computed(move || {
        list::selected_message()
            .map(|m| list::user_label_names(&m))
            .unwrap_or_default()
    });
    let details = move || global::<ShowDetails>().with(|d| d.0);
    let muted = move || theme().muted_foreground;
    let title = view()
        .gap(px(8.0))
        .child(
            text(field(|h| h.subject.clone()))
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .selectable(true)
                .text_color(move || theme().foreground),
        )
        .child(when(
            move || labels.with(|l| !l.is_empty()),
            move || {
                view().flex_row().flex_wrap().gap(px(4.0)).child(each(
                    move || labels.get(),
                    |name| name.clone(),
                    |name, _| {
                        text(move || name.get())
                            .px(px(6.0))
                            .rounded(px(4.0))
                            .text_xs()
                            .bg(move || theme().muted)
                            .text_color(move || theme().muted_foreground)
                    },
                ))
            },
        ));
    let sender = view()
        .flex_row()
        .items_start()
        .gap(px(12.0))
        .child(
            view()
                .w(px(36.0))
                .h(px(36.0))
                .flex_shrink_0()
                .rounded(px(18.0))
                .items_center()
                .justify_center()
                .bg(move || header.with(|h| list::avatar_color(&h.sender_key)))
                .child(
                    text(move || header.with(|h| rows::initial(&h.sender)))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(move || theme().foreground),
                ),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(2.0))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .flex_wrap()
                        .gap(px(8.0))
                        .child(
                            text(field(|h| h.sender.clone()))
                                .font_weight(FontWeight::SEMIBOLD)
                                .selectable(true)
                                .text_color(move || theme().foreground),
                        )
                        .child(
                            text(field(|h| h.sender_email.clone().unwrap_or_default()))
                                .text_xs()
                                .selectable(true)
                                .text_color(muted),
                        )
                        .child(view().flex_1())
                        .child(text(field(|h| h.date.clone())).text_xs().text_color(muted)),
                )
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(4.0))
                        .child(
                            text(field(|h| h.recipients.clone()))
                                .number_of_lines(1)
                                .flex_shrink(1.0)
                                .min_w(px(0.0))
                                .text_xs()
                                .text_color(muted),
                        )
                        .child(
                            view()
                                .px(px(6.0))
                                .py(px(2.0))
                                .rounded(px(4.0))
                                .on_press(move || global::<ShowDetails>().update(|d| d.0 = !d.0))
                                .child(
                                    text(
                                        move || {
                                            if details() { "Hide details" } else { "Details" }
                                        },
                                    )
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(move || theme().foreground),
                                ),
                        ),
                ),
        );
    let full_header = when(details, move || {
        view()
            .gap(px(4.0))
            .p(px(12.0))
            .rounded(px(10.0))
            .bg(move || theme().muted)
            .child(each(
                move || header.with(|h| h.details.clone()),
                |(name, _)| *name,
                move |detail, _| {
                    view()
                        .flex_row()
                        .items_start()
                        .gap(px(8.0))
                        .child(
                            text(move || detail.get().0.to_owned())
                                .w(px(64.0))
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(muted),
                        )
                        .child(
                            text(move || detail.get().1)
                                .flex_1()
                                .text_xs()
                                .selectable(true)
                                .text_color(move || theme().foreground),
                        )
                },
            ))
    });
    let scroll_ref = element_ref();
    page_state().borrow_mut().scroll = Some(scroll_ref);
    // Not the web view's: one not drawn may not paint, and would show what it last had
    // once it is; [`body`] covers it instead.
    let shown = move || {
        if reading.with(|r| r.hidden) {
            0.0_f32
        } else {
            1.0
        }
    };
    scroll().flex_1().element_ref(&scroll_ref).child(
        view()
            .px(px(24.0))
            .pt(px(20.0))
            .pb(px(32.0))
            .gap(px(16.0))
            .child(
                view()
                    .gap(px(16.0))
                    .opacity(shown)
                    .child(title)
                    .child(sender)
                    .child(full_header),
            )
            .child(body())
            .child(view().opacity(shown).child(attachments())),
    )
}

fn body() -> impl IntoElement {
    let reading = global::<Reading>();
    let view_ref = element_ref();
    page_state().borrow_mut().view = Some(view_ref);
    // One web view for every message, made with the pane: a new page loads into it.
    let hidden = move || reading.with(|r| r.hidden);
    // The page pads itself.
    view()
        .mx(px(-16.0))
        .child(web_view(view_ref))
        // Over the web view while the page is hidden: WebKit keeps painting under it.
        .child(when(hidden, || {
            view()
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .right(px(0.0))
                .bottom(px(0.0))
                .z_index(1)
                .bg(page_background)
        }))
}

/// The page's background, which shows in the web view until the page paints: WebKit's
/// own is white.
fn page_background() -> AdaptiveColor {
    let background = theme().background;
    let color = if global::<Reading>().with(|r| r.page_dark) {
        background.dark
    } else {
        background.light
    };
    AdaptiveColor {
        light: color,
        dark: color,
    }
}

fn web_view(view_ref: ElementRef) -> impl IntoElement {
    let reading = global::<Reading>();
    web_view_html(move || reading.with(|r| r.page.clone()), None)
        .java_script_enabled(false)
        .scrollable(false)
        .injected_java_script(page::BRIDGE.to_owned())
        .element_ref(&view_ref)
        .on_message(move |message: &str| match FromPage::parse(message) {
            Some(FromPage::Ready(number)) if is_shown(number) => page_ready(),
            Some(FromPage::Painted(number, height)) if is_shown(number) => page_painted(height),
            Some(FromPage::Height(number, height)) if is_shown(number) => {
                global::<BodyHeight>().set(BodyHeight(height));
            }
            Some(FromPage::Open(url)) if is_openable_link(url) => {
                spawn(open_url(url.to_owned()));
            }
            _ => {}
        })
        .bg(page_background)
        .h(move || px(global::<BodyHeight>().with(|h| h.0).max(MIN_BODY_HEIGHT)))
}

/// Under the text, where reading it ends.
fn attachments() -> impl IntoElement {
    let attachments = move || global::<Reading>().with(|r| r.attachments.clone());
    when(
        move || global::<Reading>().with(|r| !r.attachments.is_empty()),
        move || {
            view()
                .flex_row()
                .flex_wrap()
                .gap(px(8.0))
                .pt(px(16.0))
                .border_t_1()
                .border_color(move || theme().border)
                .child(each(
                    attachments,
                    |a| a.id,
                    |attachment, _| {
                        let image = move || attachment.with(|a| a.mime_type.starts_with("image/"));
                        let hovered = signal(false);
                        let names = move || attachment.with(reader::attachment_label);
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .h(px(30.0))
                            .px(px(10.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(move || theme().border)
                            .bg(move || {
                                if hovered.get() {
                                    theme().accent
                                } else {
                                    AdaptiveColor::TRANSPARENT
                                }
                            })
                            .on_mouse_enter(move |_: &MouseEvent| hovered.set(true))
                            .on_mouse_leave(move |_: &MouseEvent| hovered.set(false))
                            .on_press(move || save_attachment(attachment.get_untracked()))
                            .tooltip(commands::tooltip(move || format!("Save “{}”", names().0)))
                            .child(
                                icon(
                                    move || if image() { Icon::Photo } else { Icon::Document },
                                    16.0,
                                )
                                .text_color(move || theme().muted_foreground),
                            )
                            .child(
                                text(move || names().1)
                                    .text_sm()
                                    .text_color(move || theme().foreground),
                            )
                    },
                ))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_shows_once_the_web_view_has_grown_to_it() {
        assert!(!tall_enough(40.0, 620.0), "not grown yet");
        assert!(tall_enough(620.0, 620.0));
        assert!(tall_enough(619.5, 620.2), "rounded");
        assert!(tall_enough(40.0, 12.0), "shorter than the least height");
    }

    #[test]
    fn plain_text_takes_the_dark_theme() {
        assert!(page_is_dark(true, false, false), "plain text");
        assert!(page_is_dark(true, true, true), "recolored HTML");
        assert!(
            !page_is_dark(true, true, false),
            "HTML that couldn't be recolored"
        );
        assert!(!page_is_dark(false, false, false), "light");
    }
}
