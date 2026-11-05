//! The window: the sidebar, the list, and the message open in a web view.
//!
//! State is nsh globals (one signal per type, per surface). The client's futures are
//! awaited in nsh tasks on the UI thread; the engine does the work on its own runtime.

use std::cell::RefCell;
use std::rc::Rc;

use catmail_core::{Event, MailClient, RecvError};
use catmail_frontend::list::RowId;
use catmail_frontend::mailbox::{Place, REFRESH_INTERVAL};
use catmail_frontend::refresh::{Refresh, Throttle};
use nsh_base::theme;
use nsh_core::element::Owner;
use nsh_core::prelude::*;

use crate::list::{self, Rows, Selected};
use crate::sidebar::{self, SelectedPlace};
use crate::toast::{Level, notify};
use crate::{commands, composer, draft, outbox, reader, toast};

pub fn client() -> MailClient {
    crate::engine::client()
}

/// `what` failed ("Couldn't load folders"), and why: a notification, as the GUI's.
pub fn report(what: &str, error: catmail_core::Error) {
    tracing::warn!(error = format!("{error:#}"), "{what}");
    notify(Level::Error, format!("{what}: {error}"));
}

/// The mailbox's owner, which the work it starts from anywhere in it belongs to: a
/// row's press, say, starts what outlives the row.
#[derive(Clone, Default)]
struct MailboxOwner(Option<Owner>);

/// The work started anywhere in the view from now on belongs to `owner`: the
/// mailbox's, or a composer window's (whose notifications time out with a task too).
pub fn own_tasks(owner: Owner) {
    global::<MailboxOwner>().set(MailboxOwner(Some(owner)));
}

/// Runs `future` for as long as the mailbox (or the composer, in its window) is shown.
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
        .child(toast::toasts())
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
    own_tasks(Owner::current());
    sidebar::load();
    // Those refused while no window was open.
    commands::report_refused_changes();
    spawn(watch_events());
    effect(move || {
        let place = global::<SelectedPlace>().with(|s| s.0);
        // Leaving a search for a place: the place's rows, not the results.
        untracked(|| {
            list::clear_search();
            list::show(place.map(Place::source));
        });
    });
    effect(reader::follow_selection);
    composer::bind_keys();
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
        .child(dynamic(move || match global::<Selected>().with(|s| s.0) {
            Some(RowId::Message(_)) => reader::reader().into_element(),
            Some(RowId::Draft(_)) => draft::pane().into_element(),
            Some(RowId::Outbox(_)) => outbox::pane().into_element(),
            None => view().into_element(),
        }))
        .child(commands::picker())
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
        // Missed events may have been some.
        Some(Event::ChangesRefused { .. }) | None => commands::report_refused_changes(),
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
