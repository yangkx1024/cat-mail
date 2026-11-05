//! The window: the sidebar, the list, and the message open in a web view.
//!
//! State is nsh globals (one signal per type, per surface). The client's futures are
//! awaited in nsh tasks on the UI thread; the engine does the work on its own runtime.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use catmail_core::{
    Event, MailClient, MessageId, RecvError,
    html::{DarkPalette, InlineImage, recolor_html},
};
use catmail_frontend::html::is_openable_link;
use catmail_frontend::list::RowId;
use catmail_frontend::mailbox::{Place, REFRESH_INTERVAL};
use catmail_frontend::refresh::{Refresh, Throttle};
use nsh_base::theme;
use nsh_core::element::Owner;
use nsh_core::prelude::*;

use crate::list::{self, Rows, Selected};
use crate::page::{self, Colors, FromPage};
use crate::sidebar::{self, SelectedPlace};

pub fn client() -> MailClient {
    crate::engine::client()
}

#[derive(Clone, Default)]
struct Reading {
    subject: String,
    from: String,
    date: String,
    page: String,
}

/// The body's height in points, as the page reports it.
#[derive(Clone, Default)]
struct BodyHeight(f32);

/// The last error, shown at the bottom of the sidebar.
#[derive(Clone, Default)]
pub struct Status(pub Option<String>);

/// `what` failed ("Couldn't load folders"), and why.
pub fn report(what: &str, error: catmail_core::Error) {
    tracing::warn!(error = format!("{error:#}"), "{what}");
    global::<Status>().set(Status(Some(format!("{what}: {error}"))));
}

/// The mailbox's owner, which the work it starts from anywhere in it belongs to: a
/// row's press, say, starts what outlives the row.
#[derive(Clone, Default)]
struct MailboxOwner(Option<Owner>);

/// Runs `future` for as long as the mailbox is shown.
pub fn in_mailbox(future: impl Future<Output = ()> + 'static) {
    let owner = global::<MailboxOwner>().with_untracked(|o| o.0);
    match owner {
        Some(owner) => {
            owner.run(|| spawn(future));
        }
        None => {
            spawn(future);
        }
    }
}

/// What the engine's events ask to read again, held back between reads. Not reactive.
#[derive(Clone, Default)]
struct Throttled(Rc<RefCell<Throttle>>);

/// Whether the engine is open yet.
#[derive(Clone, PartialEq)]
enum Opening {
    Waiting,
    Open,
    Failed(String),
}

/// The window: empty while the engine opens (usually a moment), then the mailbox, or why
/// there is none.
#[component]
pub fn App() -> impl IntoElement {
    let opening = signal(Opening::Waiting);
    spawn(async move {
        opening.set(match crate::engine::opened().await {
            Ok(_) => Opening::Open,
            Err(e) => Opening::Failed(e),
        });
    });
    view()
        .flex_1()
        .bg(move || theme().background)
        .child(dynamic(move || match opening.get() {
            Opening::Waiting => view().into_element(),
            Opening::Open => mailbox().into_element(),
            Opening::Failed(reason) => failed(reason).into_element(),
        }))
}

fn failed(reason: String) -> impl IntoElement {
    view()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(8.0))
        .p(px(24.0))
        .child(
            // The GUI's words (`open_failed`).
            text("Couldn't open the mail data")
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(move || theme().foreground),
        )
        .child(
            text(reason)
                .text_sm()
                .selectable(true)
                .text_color(move || theme().muted_foreground),
        )
}

fn mailbox() -> impl IntoElement {
    global::<MailboxOwner>().set(MailboxOwner(Some(Owner::current())));
    sidebar::load();
    spawn(watch_events());
    effect(move || {
        let place = global::<SelectedPlace>().with(|s| s.0);
        // Leaving a search for a place: the place's rows, not the results.
        untracked(|| {
            list::clear_search();
            list::show(place.map(Place::source));
        });
    });
    // The task belongs to the effect's run: opening another message cancels what is
    // still loading for the last one.
    effect(move || {
        // Read here so the page is made again when the appearance changes.
        let dark = dark();
        match global::<Selected>().with(|s| s.0) {
            Some(RowId::Message(id)) => {
                spawn(open_message(id, dark));
            }
            // Drafts and the outbox have panes of their own (P5, P6).
            _ => global::<Reading>().set(Reading::default()),
        }
    });
    // As the GUI's: Escape closes what is open.
    on_key("escape", |_| global::<Selected>().set(Selected(None)));
    view()
        .flex_1()
        .flex_row()
        .child(sidebar::sidebar())
        .child(
            // The window's width to itself until a row opens; then as wide as it was.
            view()
                .flex_row()
                .w(move || {
                    if list::reading() {
                        px(420.0).into()
                    } else {
                        Length::Auto
                    }
                })
                .flex_grow(move || if list::reading() { 0.0_f32 } else { 1.0 })
                .border_r_1()
                .border_color(move || theme().border)
                .child(list::list_column()),
        )
        .child(when(list::reading, reader))
}

/// Follows the engine's events, as the GUI does (`Refresh::after`): what they change is
/// read again, a burst of them (a sync sends one per batch of messages) a read per
/// interval.
async fn watch_events() {
    let mut events = client().subscribe();
    loop {
        let event = match events.recv().await {
            Ok(event) => Some(event),
            Err(RecvError::Lagged(_)) => None,
            Err(RecvError::Closed) => return,
        };
        on_event(event.as_ref());
    }
}

fn on_event(event: Option<&Event>) {
    if event.is_none() {
        // Before reading the sidebar again, so the read isn't taken for an old one.
        sidebar::missed_events();
    }
    let shown = global::<Rows>().with_untracked(|r| r.source.clone());
    let refresh = Refresh::after(event, shown.as_ref(), &sidebar::shown_folders());
    let refresh = throttle().borrow_mut().ask(refresh);
    read_again(refresh);
    match event {
        Some(&Event::AccountStatusChanged { account }) => sidebar::status_changed(account),
        Some(&Event::DraftsChanged { account }) => sidebar::count_drafts(account),
        _ => {}
    }
}

fn throttle() -> Rc<RefCell<Throttle>> {
    global::<Throttled>().with_untracked(|t| t.0.clone())
}

/// Reads the sidebar or the list again, as [`Throttle::ask`] or [`Throttle::elapsed`]
/// said, and waits out an interval after that.
fn read_again(refresh: Refresh) {
    if refresh.is_empty() {
        return;
    }
    if refresh.sidebar {
        sidebar::load();
    }
    if refresh.list {
        list::refresh();
    }
    in_mailbox(async {
        timer(REFRESH_INTERVAL).await;
        let held = throttle().borrow_mut().elapsed();
        read_again(held);
    });
}

async fn open_message(id: MessageId, dark: bool) {
    global::<BodyHeight>().set(BodyHeight(0.0));
    let message = match client().download_message(id).await {
        Ok(message) => message,
        Err(e) => return report("Couldn't open the message", e),
    };
    let summary = &message.summary;
    let body = message.body.as_ref();
    let mut inline = HashMap::new();
    if let Some(body) = body.filter(|b| b.html.is_some()) {
        for part in &body.attachments {
            let Some(content_id) = &part.content_id else {
                continue;
            };
            if !part.mime_type.starts_with("image/") {
                continue;
            }
            match client().attachment_content(part.id).await {
                Ok(bytes) => {
                    inline.insert(
                        content_id.clone(),
                        InlineImage {
                            mime_type: part.mime_type.clone(),
                            bytes,
                        },
                    );
                }
                Err(e) => report("Couldn't load an inline image", e),
            }
        }
    }
    // For checking what the page may load: this file's HTML in place of the message's.
    let probe = std::env::var_os("CATMAIL_NSH_HTML").and_then(|f| std::fs::read_to_string(f).ok());
    let html = probe.as_deref().or(body.and_then(|b| b.html.as_deref()));
    // A dark page moves the message's colors toward the theme's, as the GUI does; one
    // whose colors come from CSS variables can't be moved and stays light.
    let recolored = html
        .filter(|_| dark)
        .and_then(|html| recolor_html(html, palette()));
    let (html, dark) = match &recolored {
        Some(html) => (Some(html.as_ref()), true),
        None => (html, false),
    };
    let page = page::body_page(
        html,
        body.and_then(|b| b.text.as_deref()),
        inline,
        &colors(dark),
    );
    global::<Reading>().set(Reading {
        subject: summary.subject.clone().unwrap_or_default(),
        from: summary
            .from
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        date: summary.date.map(format_date).unwrap_or_default(),
        page,
    });
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
        link: pick(theme().primary),
    }
}

fn format_date(seconds: i64) -> String {
    jiff::Timestamp::from_second(seconds)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

// --- Panes --------------------------------------------------------------------------

fn reader() -> impl IntoElement {
    let reading = global::<Reading>();
    scroll().flex_1().child(
        view()
            .child(
                view()
                    .px(px(16.0))
                    .pt(px(16.0))
                    .gap(px(4.0))
                    .child(
                        text(move || reading.with(|r| r.subject.clone()))
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .selectable(true)
                            .text_color(move || theme().foreground),
                    )
                    .child(
                        view()
                            .flex_row()
                            .gap(px(8.0))
                            .child(
                                text(move || reading.with(|r| r.from.clone()))
                                    .flex_1()
                                    .text_sm()
                                    .selectable(true)
                                    .text_color(move || theme().foreground),
                            )
                            .child(
                                text(move || reading.with(|r| r.date.clone()))
                                    .text_xs()
                                    .text_color(move || theme().muted_foreground),
                            ),
                    ),
            )
            .child(
                // One web view for every message: a new page loads into it.
                web_view_html(move || reading.with(|r| r.page.clone()), None)
                    .java_script_enabled(false)
                    .scrollable(false)
                    .injected_java_script(page::BRIDGE.to_owned())
                    .on_message(move |message: &str| match FromPage::parse(message) {
                        Some(FromPage::Height(height)) => {
                            global::<BodyHeight>().set(BodyHeight(height));
                        }
                        Some(FromPage::Open(url)) if is_openable_link(url) => {
                            spawn(open_url(url.to_owned()));
                        }
                        _ => {}
                    })
                    .h(move || px(global::<BodyHeight>().with(|h| h.0).max(40.0))),
            ),
    )
}
