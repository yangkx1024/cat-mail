//! The reading pane: the commands on the message, its header, its body in a web view and
//! its attachments.
//!
//! The body is a page in one web view (`catmail_frontend::html::BodyPages::page`): opening
//! a message loads a new page into it. The view stays while messages are opened one after another, for WebKit
//! starts a web process for each new view (100 MB, a sandbox to set up), and ends it when
//! the last goes. The page's images are served as they come (`crate::web`): the
//! inline parts as they load, and the remote images the engine has cached or the user
//! asks for; the others wait, unanswered, and the page shows them once they come,
//! without loading again.

mod view;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use catmail_core::html::Image;
use catmail_core::{Attachment, MessageId, safe_filename};
use catmail_frontend::html::{RemoteImages, browser_page};
use catmail_frontend::mailbox::reader::{self, Header};
use nsh_core::element::{ElementRef, Spawned, open_path, pick_save_path};
use nsh_core::file_dialog::SaveDialog;
use nsh_core::prelude::*;

use crate::engine::client;
use crate::mailbox::listing;
use crate::shell::tasks::{in_view, off_thread};
use crate::toast::{Level, notify, report};
use crate::web;

pub use view::reader;

#[derive(Clone, Default, PartialEq)]
enum State {
    #[default]
    Empty,
    Loading,
    Failed(String),
    Open,
}

#[derive(Clone, Default)]
struct Reading {
    id: Option<MessageId>,
    state: State,
    header: Header,
    /// The header shown isn't the message's: it is loading, and the list didn't have
    /// it to say what it is.
    header_hidden: bool,
    attachments: Vec<Attachment>,
    /// The HTML as sanitized, for "Open in browser".
    html: Option<Rc<str>>,
    page: Rc<str>,
    /// The page (and the attachments) hidden while a message loads, past [`STAYS_SHOWN`]
    /// (or with nothing before it to show): its header shows then, as the list has it,
    /// over a spinner.
    hidden: bool,
    /// Whether the page has the dark theme's colors: the web view's background, which
    /// shows until the page paints, is the page's.
    page_dark: bool,
    /// The remote images the HTML refers to that aren't shown yet: the cached ones show
    /// at once, the others when the user asks.
    remote: RemoteImages,
    /// Replying to all writes to more than replying does.
    can_reply_all: bool,
}

/// How long the message before stays shown while the next loads.
const STAYS_SHOWN: Duration = Duration::from_millis(250);

/// A hidden page shows once it has loaded, or after this if it doesn't say so.
const SHOWN_ANYWAY: Duration = Duration::from_secs(1);

/// The body's height in points, as the page reports it.
#[derive(Clone, Default)]
struct BodyHeight(f32);

/// Bumped by "Try again": the message opens again.
#[derive(Clone, Default)]
struct Retry(u64);

/// Every header in full, instead of the one-line summary.
#[derive(Clone, Default)]
struct ShowDetails(bool);

/// What the page shows besides itself, and the fetch of remote images; the pages are
/// the view's ([`web`]). Not reactive.
#[derive(Default)]
struct Page {
    /// The message's scroll view, taken back to the top for the next message.
    scroll: Option<ElementRef>,
    /// The message whose page was made last.
    shown: Option<MessageId>,
    /// Fetching the open message's remote images; dropped with the message.
    fetch: Option<Spawned<()>>,
}

#[derive(Clone, Default)]
struct PageState(Rc<RefCell<Page>>);

fn page_state() -> Rc<RefCell<Page>> {
    global::<PageState>().with_untracked(|p| p.0.clone())
}

/// Opens the message selected; anything else empties the pane (drafts and the outbox
/// have panes of their own). Run in an effect: the task belongs to its run,
/// so opening another message cancels what is still loading for the last one.
pub fn follow_selection() {
    // Read here so the page is made again when the appearance changes.
    let dark = dark();
    global::<Retry>().with(|r| r.0);
    let selected = listing::selected_id();
    let id = untracked(|| global::<Reading>().with(|r| r.id));
    if id != selected {
        global::<ShowDetails>().set(ShowDetails(false));
        web::with_pages(|p| p.forget_message());
        if let Some(fetch) = page_state().borrow_mut().fetch.take() {
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
            page: web::with_pages(|p| p.blank(&web::appearance(dark))).into(),
            page_dark: dark,
            ..Reading::default()
        }),
    }
}

async fn open_message(id: MessageId, dark: bool) {
    let reading = global::<Reading>();
    // Open already, made again (in another appearance): its fetch of remote images, which
    // only another message cancels, goes on.
    let again = reading.with_untracked(|r| r.id == Some(id));
    if !again {
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
                show_loading(r, id);
            }
        });
        in_view(async move {
            timer(STAYS_SHOWN).await;
            let reading = global::<Reading>();
            if reading.with_untracked(|r| r.id == Some(id) && r.state == State::Loading) {
                tracing::debug!(message = %id, "still loading; showing that it is");
                reading.update(|r| show_loading(r, id));
            }
        });
    }
    let began = Instant::now();
    let message = match client().download_message(id).await {
        Ok(message) => message,
        Err(e) => {
            tracing::warn!(message = %id, error = %e, took = ?began.elapsed(),
                "couldn't open the message");
            reading.update(|r| r.state = State::Failed(e.to_string()));
            return;
        }
    };
    let took = began.elapsed();
    if took >= STAYS_SHOWN {
        tracing::info!(message = %id, ?took, "opened a message that took a while");
    }
    // Offered when unsure: it only adds recipients the user can remove.
    let can_reply_all = client().can_reply_all(id).await.unwrap_or_else(|e| {
        tracing::warn!(message = %id, error = %e, "couldn't tell who replying to all writes to");
        true
    });
    let body = message.body.as_ref();
    // For checking what the page may load: this file's HTML in place of the message's.
    let probe = std::env::var_os("CATMAIL_GUI_HTML").and_then(|f| std::fs::read_to_string(f).ok());
    let html = probe.as_deref().or(body.and_then(|b| b.html.as_deref()));
    let (made, page_dark, remote) = {
        let text = body.and_then(|b| b.text.as_deref());
        let made = web::with_pages(|p| p.page(html, text, &web::appearance(dark)));
        let page = page_state();
        let mut page = page.borrow_mut();
        // Another message (not this one again, in a new appearance): from its top.
        if page.shown.replace(id) != Some(id)
            && let Some(scroll) = page.scroll
        {
            scroll.scroll_to(Point::new(px(0.0), px(0.0)), false);
        }
        (made.html, made.dark, made.remote_urls)
    };
    let mut blocked = RemoteImages::default();
    if again {
        blocked = reading.with_untracked(|r| r.remote.clone());
    }
    blocked.again(remote.clone());
    // Hidden (the first page, or one that took long): until the page has loaded, rather
    // than show the web view before it paints, in WebKit's own background.
    let hidden = reading.with_untracked(|r| r.hidden);
    if hidden {
        in_view(async move {
            timer(SHOWN_ANYWAY).await;
            show_page(id, "the page didn't say it painted; shown anyway");
        });
    }
    reading.set(Reading {
        id: Some(id),
        state: State::Open,
        header: Header::of(&message),
        header_hidden: false,
        attachments: reader::attachments(&message),
        html: html.map(Rc::from),
        page: made.into(),
        hidden,
        page_dark,
        remote: blocked,
        can_reply_all,
    });
    web::add_remote_images(Open(id), web::cached_remote_images(remote).await);
    // Only HTML shows inline parts. One at a time; this task goes
    // with the message.
    let Some(body) = body.filter(|b| b.html.is_some()) else {
        return;
    };
    for part in body.inline_parts() {
        let Some(content_id) = &part.content_id else {
            continue;
        };
        if web::with_pages(|p| p.has_inline(content_id)) {
            continue;
        }
        match client().attachment_content(part.id).await {
            Ok(bytes) => {
                let image = Image::new(&part.mime_type, bytes);
                web::add_inline(content_id.clone(), image);
            }
            Err(e) => tracing::warn!(attachment = %part.id, error = %e,
                "failed to load inline image"),
        }
    }
}

/// Message `id` is loading: its page and attachments are hidden (with a spinner in
/// their place), and its header shows as the list has it.
fn show_loading(r: &mut Reading, id: MessageId) {
    r.hidden = true;
    match untracked(|| listing::message(id)) {
        Some(summary) => {
            r.header = Header::of_summary(&summary);
            r.header_hidden = false;
        }
        None => r.header_hidden = true,
    }
}

/// Fetches the open message's remote images, because the user asked (`web`).
fn show_remote_images() {
    let Some(id) = global::<Reading>().with_untracked(|r| r.id) else {
        return;
    };
    let Some(fetch) = web::show_remote_images(Open(id)) else {
        return;
    };
    // Under the mailbox: not the button's, which goes when the images are all shown.
    if let Some(old) = page_state().borrow_mut().fetch.replace(spawn(fetch)) {
        old.cancel();
    }
}

/// The remote images of message `id`, while it is the one open.
#[derive(Clone, Copy)]
struct Open(MessageId);

impl web::RemoteImagesAt for Open {
    fn wanted(self) -> bool {
        global::<Reading>().with_untracked(|r| r.id == Some(self.0))
    }

    fn update<T>(self, f: impl FnOnce(&mut RemoteImages) -> T) -> Option<T> {
        let mut f = Some(f);
        let mut out = None;
        global::<Reading>().update(|r| out = f.take().map(|f| f(&mut r.remote)));
        out
    }
}

/// The page has painted with the web view `height` points tall: a hidden one shows
/// once that is the page's own height. The first page grows the web view, and WebKit
/// shows the part grown blank until it paints there.
fn page_painted(height: f32) {
    let page_height = global::<BodyHeight>().with_untracked(|h| h.0);
    if !catmail_frontend::html::tall_enough(height, page_height) {
        return;
    }
    let shown = page_state().borrow().shown;
    if let Some(id) = shown {
        show_page(id, "page painted; shown");
    }
}

/// Shows message `id`'s page if it is the one open and still hidden, logging `why`.
fn show_page(id: MessageId, why: &'static str) {
    let reading = global::<Reading>();
    if reading.with_untracked(|r| r.hidden && r.state == State::Open && r.id == Some(id)) {
        tracing::info!(message = %id, "{why}");
        reading.update(|r| r.hidden = false);
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
    let inline = web::with_pages(|p| p.inline().clone());
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
