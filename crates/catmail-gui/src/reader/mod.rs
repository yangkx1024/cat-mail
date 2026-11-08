//! The reading pane: the commands on the message, its header, its body in a web view and
//! its attachments.
//!
//! The body is a page in one web view (`catmail_frontend::html::BodyPages::page`): opening
//! a message loads a new page into it. The view stays while messages are opened one after another, for WebKit
//! starts a web process for each new view (100 MB, a sandbox to set up), and ends it when
//! the last goes. The page's images are the reader's to serve (`html::IMAGE_SCHEME`): the
//! inline parts as they load, and the remote images the engine has cached or the user
//! asks for; the others wait, unanswered, and the page shows them once they come,
//! without loading again.

mod view;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use catmail_core::html::{DarkPalette, Image, RemoteImage, recolor_html};
use catmail_core::{Attachment, MessageId, safe_filename};
use catmail_frontend::html::{BodyPages, Colors, browser_page};
use catmail_frontend::reader::{self, Header};
use nsh_base::theme;
use nsh_core::element::{ElementRef, Spawned, open_path, pick_save_path};
use nsh_core::file_dialog::SaveDialog;
use nsh_core::prelude::*;
use nsh_core::web_resource::{WebResource, WebResourceRequest};

use crate::engine::client;
use crate::mailbox::list;
use crate::tasks::{in_view, off_thread};
use crate::toast::{Level, notify, report};

pub use view::reader;

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
    page: Rc<str>,
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
struct Page {
    /// The message's scroll view, taken back to the top for the next message.
    scroll: Option<ElementRef>,
    /// The message whose page was made last.
    shown: Option<MessageId>,
    /// The pages made, the open message's images, and the requests of the page shown.
    pages: BodyPages<WebResourceRequest>,
    /// Fetching the open message's remote images; dropped with the message.
    fetch: Option<Spawned<()>>,
    /// Watching images held back ([`watch_images`]).
    watching: bool,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            scroll: None,
            shown: None,
            pages: BodyPages::new(IMAGES_IN_FLIGHT),
            fetch: None,
            watching: false,
        }
    }
}

/// The bytes of images on their way to the page at once (`BodyPages::new`; why, in
/// `catmail_frontend::html`'s pacer): WebKitGTK reads them 8 KiB at a time.
#[cfg(target_os = "linux")]
pub(super) const IMAGES_IN_FLIGHT: Option<usize> = Some(1 << 20);

/// `WKURLSchemeHandler` takes each image whole: no limit.
#[cfg(not(target_os = "linux"))]
pub(super) const IMAGES_IN_FLIGHT: Option<usize> = None;

#[derive(Clone, Default)]
struct PageState(Rc<RefCell<Page>>);

fn page_state() -> Rc<RefCell<Page>> {
    global::<PageState>().with_untracked(|p| p.0.clone())
}

/// Whether what page `number` says is the page shown's: what other pages say, late, is
/// ignored.
fn is_shown(number: u64) -> bool {
    page_state().borrow().pages.is_shown(number)
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
        page.pages.forget_message();
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
            page: page_state().borrow_mut().pages.blank(&colors(dark)).into(),
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
        in_view(async move {
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
    // For checking what the page may load: this file's HTML in place of the message's.
    let probe = std::env::var_os("CATMAIL_GUI_HTML").and_then(|f| std::fs::read_to_string(f).ok());
    let html = probe.as_deref().or(body.and_then(|b| b.html.as_deref()));
    // A dark page moves the message's colors toward the theme's; one
    // whose colors come from CSS variables can't be moved and stays light.
    let recolored = html
        .filter(|_| dark)
        .and_then(|html| recolor_html(html, palette()));
    let page_dark = page_is_dark(dark, html.is_some(), recolored.is_some());
    let page_html = recolored.as_deref().or(html);
    let (made, remote) = {
        let page = page_state();
        let mut page = page.borrow_mut();
        let made = page.pages.page(
            page_html,
            body.and_then(|b| b.text.as_deref()),
            &colors(page_dark),
        );
        // Those not shown yet: the cached ones show at once, the others when the user asks.
        let remote = (made.remote_urls.into_iter())
            .filter(|url| !page.pages.shows_remote(url))
            .collect::<Vec<_>>();
        // Another message (not this one again, in a new appearance): from its top.
        if page.shown.replace(id) != Some(id)
            && let Some(scroll) = page.scroll
        {
            scroll.scroll_to(Point::new(px(0.0), px(0.0)), false);
        }
        (made.html, remote)
    };
    // Hidden (the first page, or one that took long): until the page has loaded, rather
    // than show the web view before it paints, in WebKit's own background.
    let hidden = reading.with_untracked(|r| r.hidden);
    if hidden {
        in_view(async move {
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
        page: made.into(),
        hidden,
        page_dark,
        remote: remote.clone(),
        loading_remote: false,
        can_reply_all,
    });
    // Those cached need no request: they show without asking.
    if !remote.is_empty() {
        match client().cached_remote_images(remote).await {
            Ok(cached) if !cached.is_empty() => {
                add_remote_images(id, cached);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "couldn't read the image cache"),
        }
    }
    // Only HTML shows inline parts. One at a time; this task goes
    // with the message.
    let Some(body) = body.filter(|b| b.html.is_some()) else {
        return;
    };
    for part in body.inline_parts() {
        let Some(content_id) = &part.content_id else {
            continue;
        };
        if page_state().borrow().pages.has_inline(content_id) {
            continue;
        }
        match client().attachment_content(part.id).await {
            Ok(bytes) => {
                let image = Image::new(&part.mime_type, bytes);
                add_inline_image(content_id.clone(), image);
            }
            Err(e) => tracing::warn!(attachment = %part.id, error = %e,
                "failed to load inline image"),
        }
    }
}

/// Puts an inline part of the open message in its page.
fn add_inline_image(content_id: String, image: Image) {
    let answers = page_state()
        .borrow_mut()
        .pages
        .add_inline(content_id, image);
    answer(answers);
}

/// The page asks for one of its images (`html::IMAGE_SCHEME`): answered once it is here
/// and the page has taken those before it (`BodyPages::request`).
fn serve_image(request: WebResourceRequest) {
    let url = request.url().to_owned();
    let answers = page_state().borrow_mut().pages.request(&url, request);
    answer(answers);
}

/// The page says one of its images has come (or failed): the next go.
fn image_loaded(url: &str) {
    let answers = page_state().borrow_mut().pages.loaded(url);
    answer(answers);
}

/// Gives the page the images it asked for, and watches those held back.
fn answer(answers: Vec<(WebResourceRequest, Image)>) {
    for (request, image) in answers {
        let resource = WebResource::new(image.mime_type().clone(), image.bytes().clone());
        request.respond(resource);
    }
    watch_images();
}

/// How long images may be held back with none coming before the rest go anyway.
const UNSTICK_AFTER: Duration = Duration::from_secs(3);

/// While images are held back, checks every [`UNSTICK_AFTER`] that one has come since: a
/// page that never says so would get no more (`BodyPages::unstick`). One watch at a time.
fn watch_images() {
    {
        let page = page_state();
        let mut page = page.borrow_mut();
        if page.watching || !page.pages.holds_back() {
            return;
        }
        page.watching = true;
    }
    in_view(async {
        loop {
            let progress = page_state().borrow().pages.progress();
            timer(UNSTICK_AFTER).await;
            let answers = {
                let page = page_state();
                let mut page = page.borrow_mut();
                if !page.pages.holds_back() {
                    page.watching = false;
                    return;
                }
                page.pages.unstick(progress)
            };
            if !answers.is_empty() {
                tracing::warn!("the page didn't say its images came; sending the rest");
            }
            answer(answers);
        }
    });
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
    reading.update(|r| {
        r.remote
            .retain(|url| !fetched.iter().any(|(had, _)| had == url));
    });
    let answers = page_state().borrow_mut().pages.add_remote(fetched);
    answer(answers);
    true
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
    in_view(async move {
        let saved = async {
            let Some(path) = path.await? else {
                return Ok(None);
            };
            let bytes = client()
                .attachment_content(attachment.id)
                .await
                .map_err(|e| e.to_string())?;
            let to = path.clone();
            off_thread(move || std::fs::write(to, bytes))
                .await
                .unwrap_or_else(|| Err(std::io::Error::other("the writer went away")))
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
            Err(e) => report(&format!("Couldn't save “{name}”"), e),
        }
    });
}

/// Writes the message as a standalone page and hands it to the system browser. The
/// inline parts go in as loaded; remote images stay blocked there.
fn open_in_browser() {
    let Some((title, html)) = global::<Reading>().with_untracked(|r| {
        let html = r.html.clone()?;
        Some((r.header.subject.clone(), html))
    }) else {
        return;
    };
    let inline = page_state().borrow().pages.inline().clone();
    in_view(async move {
        let page = browser_page(&title, &html, inline);
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
