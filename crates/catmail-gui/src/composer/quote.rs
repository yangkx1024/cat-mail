//! A reply's or forward's quote, under the text: a bar that says whose message it is,
//! and the message itself once shown, as the reader shows it (`crate::web`: its page, its
//! images, remote ones only when cached or asked for). It isn't in the editor: the
//! engine puts it under the text when the message goes. The user can take it out, or
//! answer in it, which moves what of it the editor can hold into the text.

use std::path::PathBuf;
use std::rc::Rc;

use catmail_core::html::{Image, cid_image_ids};
use catmail_core::{ForwardField, Quote, QuoteHead};
use catmail_frontend::composer::{Opening, quote_heading};
use catmail_frontend::html::RemoteImages;
use nsh_base::theme;
use nsh_core::prelude::*;

use crate::buttons::{Size, icon_button};
use crate::icons::Icon;
use crate::mailbox::panes::fact;
use crate::tasks::off_thread;
use crate::web;

use super::{changed, ending, in_window, rich, text_changed};

/// The quote, and how it is shown.
#[derive(Clone, Default)]
struct Quoting {
    /// The draft's, until the user takes it out (or answers in it).
    quote: Option<Rc<Quote>>,
    /// The message is shown, not only its bar.
    expanded: bool,
    /// It has been: its web view is made the first time and kept, hidden while the
    /// message isn't shown (each new one starts a web process).
    made: bool,
    /// The images its HTML shows: the draft's, which it was copied with. By Content-ID,
    /// with the file and the type of each.
    images: Rc<[(String, PathBuf, String)]>,
    remote: RemoteImages,
}

/// The quote of the draft opened.
pub(super) fn start(opening: &Opening) {
    let quote = opening.draft.content.quote.clone().map(Rc::new);
    let shown = quote
        .as_ref()
        .and_then(|q| q.html.as_deref())
        .map(cid_image_ids)
        .unwrap_or_default();
    let images = opening
        .draft
        .attachments
        .iter()
        .filter_map(|a| {
            let id = a.content_id.as_ref().filter(|id| shown.contains(*id))?;
            let path = opening.images.path(id)?;
            Some((id.clone(), path.to_path_buf(), a.mime_type.clone()))
        })
        .collect();
    global::<Quoting>().set(Quoting {
        quote,
        images,
        ..Quoting::default()
    });
}

/// The draft keeps its quote: it hasn't been taken out.
pub(super) fn quoted() -> bool {
    global::<Quoting>().with_untracked(|q| q.quote.is_some())
}

fn toggle() {
    global::<Quoting>().update(|q| {
        q.expanded = !q.expanded;
        q.made |= q.expanded;
    });
}

/// Takes the quote out of the draft, saved like a change of the text. Its images go once
/// the window closes, as those taken out of the text do.
fn remove() {
    if untracked(ending) {
        return;
    }
    global::<Quoting>().update(|q| q.quote = None);
    changed();
}

/// Moves the quote into the text, at its end, for the user to answer in; saved, with the
/// quote out, in one change.
fn answer_in() {
    if untracked(ending) {
        return;
    }
    let Some(quote) = global::<Quoting>().with_untracked(|q| q.quote.clone()) else {
        return;
    };
    let rich = rich();
    rich.answer_in(&quote);
    global::<Quoting>().update(|q| q.quote = None);
    text_changed();
    rich.focus();
}

/// The message's page in the appearance `dark`, and whether it is dark; its images go
/// to it as they come.
fn make_page(quote: &Quote, dark: bool) -> (Rc<str>, bool) {
    let made = web::with_pages(|p| {
        p.page(
            quote.html.as_deref(),
            Some(&quote.text),
            &web::appearance(dark),
        )
    });
    let remote = made.remote_urls;
    // Made again in another appearance: a fetch on its way goes on.
    global::<Quoting>().update(|q| q.remote.again(remote.clone()));
    load_images();
    in_window(async move {
        web::add_remote_images(Quoted, web::cached_remote_images(remote).await);
    });
    (made.html.into(), made.dark)
}

/// Puts the quote's images in its page, those not there yet: read from their files.
fn load_images() {
    let images: Vec<_> = global::<Quoting>().with_untracked(|q| {
        (q.images.iter())
            .filter(|(id, ..)| !web::with_pages(|p| p.has_inline(id)))
            .cloned()
            .collect()
    });
    if images.is_empty() {
        return;
    }
    in_window(async move {
        let read = off_thread(move || {
            images
                .into_iter()
                .filter_map(|(id, path, mime_type)| match std::fs::read(&path) {
                    Ok(bytes) => Some((id, Image::new(&mime_type, bytes))),
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e,
                            "couldn't read a quoted image");
                        None
                    }
                })
                .collect::<Vec<_>>()
        });
        for (id, image) in read.await.unwrap_or_default() {
            web::add_inline(id, image);
        }
    });
}

/// Fetches the remote images of the message quoted, because the user asked (`web`).
fn show_remote_images() {
    if let Some(fetch) = web::show_remote_images(Quoted) {
        in_window(fetch);
    }
}

/// The remote images of the message quoted, while the draft has the quote.
#[derive(Clone, Copy)]
struct Quoted;

impl web::RemoteImagesAt for Quoted {
    fn wanted(self) -> bool {
        quoted()
    }

    fn update<T>(self, f: impl FnOnce(&mut RemoteImages) -> T) -> Option<T> {
        let mut f = Some(f);
        let mut out = None;
        global::<Quoting>().update(|q| out = f.take().map(|f| f(&mut q.remote)));
        out
    }
}

// --- How it looks -------------------------------------------------------------------

/// Under the text, while the draft has a quote: its bar, and the message once shown,
/// which shares the window's height with the text.
pub(super) fn quote() -> impl IntoElement {
    when(|| global::<Quoting>().with(|q| q.quote.is_some()), section)
}

fn expanded() -> bool {
    global::<Quoting>().with(|q| q.expanded)
}

fn section() -> View {
    // The quote stays as it is while this is shown: taken out, this goes.
    let quote = global::<Quoting>()
        .with_untracked(|q| q.quote.clone())
        .expect("shown with a quote");
    view()
        .flex_shrink_0()
        .border_t_1()
        .border_color(move || theme().border)
        .style_when(expanded, |s| s.flex_1().min_h(px(0.0)))
        .child(bar(quote_heading(&quote)))
        .child(when(
            || global::<Quoting>().with(|q| q.made),
            move || message(quote.clone()),
        ))
}

fn bar(heading: String) -> impl IntoElement {
    let offered = move || global::<Quoting>().with(|q| q.remote.offered().filter(|_| q.expanded));
    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .h(px(40.0))
        // The chevron's box in the editor's margin: the heading starts where the text does.
        .pl(px(14.0))
        .pr(px(16.0))
        .gap(px(4.0))
        .child(icon_button(
            || {
                if expanded() {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                }
            },
            || {
                if expanded() {
                    "Hide quoted text"
                } else {
                    "Show quoted text"
                }
                .to_owned()
            },
            || true,
            Size::Row,
            toggle,
        ))
        .child(
            text(heading)
                .number_of_lines(1)
                .flex_1()
                .min_w(px(0.0))
                .text_sm()
                .text_color(move || theme().muted_foreground)
                .on_press(toggle),
        )
        .child(web::remote_images_button(
            offered,
            Size::Row,
            show_remote_images,
        ))
        .child(icon_button(
            || Icon::PencilSquare,
            || "Respond inline".to_owned(),
            move || !ending(),
            Size::Row,
            answer_in,
        ))
        .child(icon_button(
            || Icon::XMark,
            || "Remove quoted text".to_owned(),
            move || !ending(),
            Size::Row,
            remove,
        ))
}

/// The message quoted, as the reader shows it, a forward's headers over it; hidden while
/// the bar is folded.
fn message(quote: Rc<Quote>) -> impl IntoElement {
    let page = signal((Rc::<str>::from(""), false));
    let height = signal(0.0_f32);
    let headers = match &quote.head {
        QuoteHead::Forward(headers) if !headers.is_empty() => Some(headers.clone()),
        _ => None,
    };
    // Made again when the appearance changes.
    effect(move || {
        let dark = dark();
        page.set(untracked(|| make_page(&quote, dark)));
    });
    let web = web::web_view(
        move || page.with(|p| p.0.clone()),
        move |h| height.set(h),
        |_| {},
    )
    .bg(move || web::background(page.with(|p| p.1)))
    .h(move || px(height.get().max(web::MIN_HEIGHT)));
    view()
        .flex_1()
        .min_h(px(0.0))
        .style_when(move || !expanded(), |s| s.hidden())
        .children(headers.map(forward_headers))
        // The page pads itself, a little less than the text is.
        .child(
            scroll()
                .flex_1()
                .min_h(px(0.0))
                .child(view().px(px(4.0)).child(web)),
        )
}

/// A forward's headers over the message, a row each, as the draft's pane shows its
/// recipients.
fn forward_headers(headers: Vec<(ForwardField, String)>) -> View {
    view().gap(px(4.0)).px(px(20.0)).pb(px(8.0)).children(
        headers
            .into_iter()
            .map(|(field, value)| fact(field.label(), value)),
    )
}
