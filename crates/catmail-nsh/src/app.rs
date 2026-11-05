//! The window: folders, the folder's messages, and the message open in a web view.
//!
//! State is nsh globals (one signal per type, per surface). The client's futures are
//! awaited in nsh tasks on the UI thread; the engine does the work on its own runtime.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use catmail_core::{
    Event, MailClient, MessageId, MessageSummary, RecvError,
    html::{DarkPalette, InlineImage, recolor_html},
};
use catmail_frontend::html::is_openable_link;
use catmail_frontend::mailbox::{Place, REFRESH_INTERVAL};
use catmail_frontend::refresh::{Refresh, Throttle};
use nsh_base::theme;
use nsh_core::element::Owner;
use nsh_core::prelude::*;

use crate::page::{self, Colors, FromPage};
use crate::sidebar::{self, SelectedPlace};

pub fn client() -> MailClient {
    crate::engine::client()
}

/// Messages fetched at a time.
const PAGE: u32 = 200;

#[derive(Clone, Default)]
pub struct SelectedMessage(pub Option<MessageId>);

#[derive(Clone, Default)]
struct Messages {
    list: Vec<MessageSummary>,
    /// The last page was full: there may be more.
    more: bool,
    loading: bool,
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
    // The tasks belong to the effects' runs: picking another place or message cancels
    // what is still loading for the last one.
    effect(move || {
        let place = global::<SelectedPlace>().with(|s| s.0);
        spawn(load_messages(place));
    });
    effect(move || {
        // Read here so the page is made again when the appearance changes.
        let dark = dark();
        match global::<SelectedMessage>().with(|s| s.0) {
            Some(id) => {
                spawn(open_message(id, dark));
            }
            None => global::<Reading>().set(Reading::default()),
        }
    });
    view()
        .flex_1()
        .flex_row()
        .child(sidebar::sidebar())
        .child(message_list())
        .child(reader())
}

// --- Loading ------------------------------------------------------------------------

/// A page of what `place` lists, after `after`. The drafts and the outbox are lists of
/// their own (P3).
async fn list_page(
    place: Place,
    limit: u32,
    after: Option<catmail_core::MessageCursor>,
) -> catmail_core::Result<Vec<MessageSummary>> {
    match place {
        Place::Inboxes => client().list_inboxes(limit, after).await,
        Place::Folder(folder) => client().list_messages(folder, limit, after).await,
        Place::Drafts(_) | Place::Outbox(_) => Ok(Vec::new()),
    }
}

async fn load_messages(place: Option<Place>) {
    global::<Messages>().set(Messages {
        loading: place.is_some(),
        ..Messages::default()
    });
    let Some(place) = place else {
        return;
    };
    match list_page(place, PAGE, None).await {
        Ok(list) => {
            // For looking at the reader without a mouse (screenshots of the spike): the
            // message at that index opens.
            let open = std::env::var("CATMAIL_NSH_OPEN")
                .ok()
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| list.get(i))
                .map(|m| m.id);
            global::<Messages>().set(Messages {
                more: list.len() == PAGE as usize,
                list,
                loading: false,
            });
            if open.is_some() && global::<SelectedMessage>().with_untracked(|s| s.0.is_none()) {
                global::<SelectedMessage>().set(SelectedMessage(open));
            }
        }
        Err(e) => {
            global::<Messages>().update(|m| m.loading = false);
            report("Couldn't list messages", e);
        }
    }
}

/// The next page of the open folder.
async fn load_more() {
    let messages = global::<Messages>();
    let Some(place) = global::<SelectedPlace>().with_untracked(|s| s.0) else {
        return;
    };
    let Some(after) = messages.with_untracked(|m| {
        (m.more && !m.loading)
            .then(|| m.list.last().map(MessageSummary::cursor))
            .flatten()
    }) else {
        return;
    };
    messages.update(|m| m.loading = true);
    match list_page(place, PAGE, Some(after)).await {
        Ok(page) => messages.update(|m| {
            m.more = page.len() == PAGE as usize;
            m.list.extend(page);
            m.loading = false;
        }),
        Err(e) => {
            messages.update(|m| m.loading = false);
            report("Couldn't list messages", e);
        }
    }
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
    let shown = global::<SelectedPlace>().with_untracked(|p| p.0.map(Place::source));
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
        refresh_list();
    }
    in_mailbox(async {
        timer(REFRESH_INTERVAL).await;
        let held = throttle().borrow_mut().elapsed();
        read_again(held);
    });
}

/// The list's first rows again, as many as are shown, keeping the place.
pub fn refresh_list() {
    let Some(place) = global::<SelectedPlace>().with_untracked(|p| p.0) else {
        return;
    };
    in_mailbox(async move {
        let shown = global::<Messages>().with_untracked(|m| m.list.len()) as u32;
        let limit = shown.max(PAGE);
        match list_page(place, limit, None).await {
            Ok(list) if global::<SelectedPlace>().with_untracked(|p| p.0) == Some(place) => {
                global::<Messages>().update(|m| {
                    m.more = list.len() as u32 == limit;
                    m.list = list;
                });
            }
            Ok(_) => {}
            Err(e) => report("Couldn't list messages", e),
        }
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

fn message_list() -> impl IntoElement {
    view()
        .w(px(380.0))
        .border_r_1()
        .border_color(move || theme().border)
        .child(
            virtual_list(
                move || global::<Messages>().with(|m| m.list.len()),
                message_row,
            )
            .keys(move || {
                global::<Messages>()
                    .with(|m| m.list.iter().map(|m| m.id.to_string()).collect::<Vec<_>>())
            })
            .on_load_more(move || {
                spawn(load_more());
            })
            .estimated_item_size(px(76.0))
            .flex_1(),
        )
}

fn message_row(at: Position) -> impl IntoElement {
    let message = computed(move || global::<Messages>().with(|m| m.list.get(at.index()).cloned()));
    let field = move |f: fn(&MessageSummary) -> String| {
        move || message.with(|m| m.as_ref().map(f).unwrap_or_default())
    };
    let id = move || message.with(|m| m.as_ref().map(|m| m.id));
    let unread = move || {
        message.with(|m| {
            m.as_ref()
                .is_some_and(|m| !m.flags.contains(catmail_core::Flags::SEEN))
        })
    };
    let selected = move || id().is_some() && id() == global::<SelectedMessage>().with(|s| s.0);
    view()
        .px(px(14.0))
        .py(px(10.0))
        .gap(px(2.0))
        .border_b_1()
        .border_color(move || theme().border)
        .style_when(selected, |s| s.bg(move || theme().accent))
        .on_press(move || {
            if let Some(id) = id() {
                global::<SelectedMessage>().set(SelectedMessage(Some(id)));
            }
        })
        .child(
            view()
                .flex_row()
                .gap(px(8.0))
                .child(
                    text(field(|m| {
                        m.from
                            .first()
                            .map(|a| a.name.clone().unwrap_or_else(|| a.email.clone()))
                            .unwrap_or_default()
                    }))
                    .number_of_lines(1)
                    .flex_1()
                    .text_sm()
                    .style_when(unread, |s| s.font_weight(FontWeight::SEMIBOLD))
                    .text_color(move || theme().foreground),
                )
                .child(
                    text(field(|m| m.date.map(format_date).unwrap_or_default()))
                        .text_xs()
                        .text_color(move || theme().muted_foreground),
                ),
        )
        .child(
            text(field(|m| m.subject.clone().unwrap_or_default()))
                .number_of_lines(1)
                .text_sm()
                .style_when(unread, |s| s.font_weight(FontWeight::SEMIBOLD))
                .text_color(move || theme().foreground),
        )
        .child(
            text(field(|m| m.snippet.clone().unwrap_or_default()))
                .number_of_lines(1)
                .text_xs()
                .text_color(move || theme().muted_foreground),
        )
}

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
