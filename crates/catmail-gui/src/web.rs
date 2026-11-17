//! The web view a message's HTML is shown in, and the images its page asks for: the
//! reading pane shows the message open in one, a composer the message its quote is of.
//! The pages are `catmail_frontend::html::BodyPages`'s (what may load, the reader's
//! image URLs); here they are shown, and their images served as they come.
//!
//! Each view (the mailbox's, each composer's) runs on its own thread, with its own web
//! view and pages: an nsh global of the view.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use catmail_core::html::{DarkPalette, Image, RemoteImage};
use catmail_frontend::html::{
    self, Appearance, BodyPages, Colors, DarkAppearance, FromPage, RemoteImages, is_openable_link,
};
use futures_util::StreamExt as _;
use nsh_base::theme;
use nsh_core::element::WebView;
use nsh_core::prelude::*;
use nsh_core::web_resource::{WebResource, WebResourceRequest};

use crate::buttons::{Size, icon_button};
use crate::engine::client;
use crate::icons::Icon;
use crate::tasks::in_view;
use crate::toast::{Level, notify};

/// The bytes of images on their way to the page at once (`BodyPages::new`; why, in
/// `catmail_frontend::html`'s pacer): WebKitGTK reads them 8 KiB at a time.
#[cfg(target_os = "linux")]
const IMAGES_IN_FLIGHT: Option<usize> = Some(1 << 20);

/// `WKURLSchemeHandler` takes each image whole: no limit.
#[cfg(not(target_os = "linux"))]
const IMAGES_IN_FLIGHT: Option<usize> = None;

/// The web view's height before the page says its own.
pub const MIN_HEIGHT: f32 = 40.0;

/// How long images may be held back with none coming before the rest go anyway.
const UNSTICK_AFTER: Duration = Duration::from_secs(3);

/// The view's pages and their images. Not reactive.
struct Body {
    /// The pages made, the images of the message shown, and the requests of the page
    /// shown.
    pages: BodyPages<WebResourceRequest>,
    /// Watching images held back ([`watch_images`]).
    watching: bool,
}

#[derive(Clone)]
struct Shared(Rc<RefCell<Body>>);

impl Default for Shared {
    fn default() -> Self {
        Shared(Rc::new(RefCell::new(Body {
            pages: BodyPages::new(IMAGES_IN_FLIGHT),
            watching: false,
        })))
    }
}

fn body() -> Rc<RefCell<Body>> {
    global::<Shared>().with_untracked(|b| b.0.clone())
}

/// The view's pages: to make the next, or ask what the one shown has.
pub fn with_pages<T>(f: impl FnOnce(&mut BodyPages<WebResourceRequest>) -> T) -> T {
    f(&mut body().borrow_mut().pages)
}

/// Whether what page `number` says is the page shown's: what other pages say, late, is
/// ignored.
fn is_shown(number: u64) -> bool {
    with_pages(|p| p.is_shown(number))
}

/// The web view of the pages `page` gives, one after another. It says how tall the page
/// is (`height`, in points) and when it has painted at the web view's height
/// (`painted`); the links clicked in it open in the system's browser.
pub fn web_view(
    page: impl Fn() -> String + 'static,
    height: impl Fn(f32) + 'static,
    painted: impl Fn(f32) + 'static,
) -> WebView {
    // The handler is called on the platform's main thread, outside the view: it hands
    // each request to the view, which answers it from the images of the message shown.
    let (requests, mut requested) = futures_channel::mpsc::unbounded();
    spawn(async move {
        while let Some(request) = requested.next().await {
            serve_image(request);
        }
    });
    let web = web_view_html(page, None);
    // The page says which images came only where they go a few at a time.
    let web = if IMAGES_IN_FLIGHT.is_some() {
        web.injected_java_script_before_content(html::WATCH_IMAGES.to_owned())
    } else {
        web
    };
    web
        // Someone else's HTML: whatever website data it leaves goes with the web view,
        // and none of it is written to the data directory.
        .incognito(true)
        .java_script_enabled(false)
        .scrollable(false)
        .injected_java_script(html::BRIDGE.to_owned())
        // Dropped unanswered if the view has gone, which fails it.
        .url_scheme(html::IMAGE_SCHEME, move |request| {
            let _ = requests.unbounded_send(request);
        })
        .on_message(move |message: &str| match FromPage::parse(message) {
            Some(FromPage::Loaded(url)) => image_loaded(url),
            Some(FromPage::Painted(number, at)) if is_shown(number) => painted(at),
            Some(FromPage::Height(number, h)) if is_shown(number) => height(h),
            Some(FromPage::Open(url)) if is_openable_link(url) => {
                spawn(open_url(url.to_owned()));
            }
            _ => {}
        })
}

/// An inline part of the message shown has loaded: in its page from now on.
pub fn add_inline(content_id: String, image: Image) {
    let answers = with_pages(|p| p.add_inline(content_id, image));
    answer(answers);
}

/// Remote images of the message shown came (fetched, or from the cache).
fn add_remote(images: Vec<(String, RemoteImage)>) {
    if images.is_empty() {
        return;
    }
    let answers = with_pages(|p| p.add_remote(images));
    answer(answers);
}

/// What came of remote images: each one, or why it couldn't be had
/// (`RemoteImages::came`).
pub type Fetched = Vec<(String, Result<RemoteImage, String>)>;

/// Those of `urls` the engine has cached: they show without asking.
pub async fn cached_remote_images(urls: Vec<String>) -> Fetched {
    if urls.is_empty() {
        return Vec::new();
    }
    match client().cached_remote_images(urls).await {
        Ok(cached) => cached
            .into_iter()
            .map(|(url, image)| (url, Ok(image)))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "couldn't read the image cache");
            Vec::new()
        }
    }
}

/// Fetches `urls`, the remote images the user asked for, handing them to `add` as they
/// come (those that finished together together). False as soon as `add` says they are no
/// longer wanted (false), true once all came or failed ([`fetch_ended`]).
async fn fetch_remote_images(urls: Vec<String>, mut add: impl FnMut(Fetched) -> bool) -> bool {
    let mut stream = match client().fetch_remote_images(urls.clone()).await {
        Ok(stream) => stream,
        Err(e) => {
            let e = e.to_string();
            return add(urls.into_iter().map(|url| (url, Err(e.clone()))).collect());
        }
    };
    while let Some(first) = stream.next().await {
        let ready = std::iter::from_fn(|| stream.next_ready());
        let fetched = std::iter::once(first)
            .chain(ready)
            .map(|(url, image)| {
                let image = image.map_err(|e| {
                    tracing::debug!(%url, error = %e, "remote image not loaded");
                    e.to_string()
                });
                (url, image)
            })
            .collect();
        if !add(fetched) {
            return false;
        }
    }
    true
}

/// The fetch the user asked for is over (`RemoteImages::ended`): says why, if none could
/// be had.
fn fetch_ended(result: Result<(), String>) {
    if let Err(error) = result {
        notify(
            Level::Error,
            format!("Couldn't load remote images: {error}"),
        );
    }
}

/// Where a view keeps the [`RemoteImages`] of the page it shows: the reading pane's of
/// the message open, a composer's of its quote.
pub trait RemoteImagesAt: Copy + 'static {
    /// Whether the page they are for is still shown.
    fn wanted(self) -> bool;
    /// Runs `f` on them; none if it couldn't.
    fn update<T>(self, f: impl FnOnce(&mut RemoteImages) -> T) -> Option<T>;
}

/// Fetches the page's blocked remote images, because the user asked, showing each as it
/// comes: the work, for the view to run while the page is shown; none if there is nothing
/// to fetch. If none could be had, it says why and leaves them blocked, to try again.
pub fn show_remote_images(at: impl RemoteImagesAt) -> Option<impl Future<Output = ()>> {
    let urls = at.update(RemoteImages::start).flatten()?;
    Some(async move {
        if fetch_remote_images(urls, |fetched| add_remote_images(at, fetched)).await
            && let Some(result) = at.update(RemoteImages::ended)
        {
            fetch_ended(result);
        }
    })
}

/// Shows remote images that came (fetched, or from the cache); false once the page isn't.
pub fn add_remote_images(at: impl RemoteImagesAt, fetched: Fetched) -> bool {
    if !at.wanted() {
        return false;
    }
    if fetched.is_empty() {
        return true;
    }
    add_remote(at.update(|r| r.came(fetched)).unwrap_or_default());
    true
}

/// "Show remote images", while some are `offered` (`RemoteImages::offered`): dimmed while
/// they come.
pub fn remote_images_button(
    offered: impl Fn() -> Option<bool> + Copy + 'static,
    size: Size,
    press: impl Fn() + Copy + 'static,
) -> impl IntoElement {
    when(
        move || offered().is_some(),
        move || {
            icon_button(
                || Icon::Photo,
                || "Show remote images".to_owned(),
                move || offered() == Some(false),
                size,
                press,
            )
        },
    )
}

/// The page asks for one of its images (`html::IMAGE_SCHEME`): answered once it is here
/// and the page has taken those before it (`BodyPages::request`).
fn serve_image(request: WebResourceRequest) {
    let url = request.url().to_owned();
    let answers = with_pages(|p| p.request(&url, request));
    answer(answers);
}

/// The page says one of its images has come (or failed): the next go.
fn image_loaded(url: &str) {
    let answers = with_pages(|p| p.loaded(url));
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

/// While images are held back, checks every [`UNSTICK_AFTER`] that one has come since: a
/// page that never says so would get no more (`BodyPages::unstick`). One watch at a time.
fn watch_images() {
    {
        let body = body();
        let mut body = body.borrow_mut();
        if body.watching || !body.pages.holds_back() {
            return;
        }
        body.watching = true;
    }
    in_view(async {
        loop {
            let progress = with_pages(|p| p.progress());
            timer(UNSTICK_AFTER).await;
            let answers = {
                let body = body();
                let mut body = body.borrow_mut();
                if !body.pages.holds_back() {
                    body.watching = false;
                    return;
                }
                body.pages.unstick(progress)
            };
            if !answers.is_empty() {
                tracing::warn!("the page didn't say its images came; sending the rest");
            }
            answer(answers);
        }
    });
}

/// The page's background, which shows in the web view until the page paints (WebKit's
/// own is white): the theme's, of the page's appearance (`dark`).
pub fn background(dark: bool) -> AdaptiveColor {
    let background = theme().background;
    let color = if dark {
        background.dark
    } else {
        background.light
    };
    AdaptiveColor {
        light: color,
        dark: color,
    }
}

/// What a page is shown in: a dark one moves the message's colors toward the theme's.
pub fn appearance(dark: bool) -> Appearance {
    Appearance {
        light: colors(false),
        dark: dark.then(|| DarkAppearance {
            palette: palette(),
            colors: colors(true),
        }),
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

fn colors(dark: bool) -> Colors {
    let pick = |c: AdaptiveColor| if dark { c.dark } else { c.light }.to_hex();
    Colors {
        foreground: pick(theme().foreground),
        background: pick(theme().background),
        link: pick(crate::theme::link()),
        selection: pick(crate::theme::selection()),
    }
}
