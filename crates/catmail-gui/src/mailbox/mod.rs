//! The window: the sidebar, the list, and the message open in a web view.
//!
//! State is nsh globals (one signal per type, per surface). The client's futures are
//! awaited in nsh tasks on the UI thread; the engine does the work on its own runtime.

pub mod commands;
mod draft;
mod folder_commands;
pub mod list;
mod outbox;
pub mod panes;
mod picker;
pub mod sidebar;

use std::cell::RefCell;
use std::rc::Rc;

use catmail_core::{Event, RecvError};
use catmail_frontend::mailbox::{Place, REFRESH_INTERVAL};
use catmail_frontend::refresh::{Refresh, Throttle};
use futures_util::StreamExt as _;
use nsh_base::theme;
use nsh_core::element::Owner;
use nsh_core::prelude::*;

use crate::engine::client;
use crate::login::{self, SignIn};
use crate::shell::Ask;
use crate::tasks::{in_view, own_tasks};
use crate::{chrome, composer, form, reader, settings, shell, toast};
use catmail_frontend::list::Listing;
use list::Selected;
use sidebar::{Accounts, SelectedPlace};

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
    settings::follow();
    chrome::unified();
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
            Opening::Waiting => chrome::page(view()).into_element(),
            Opening::Open => mailbox().into_element(),
            Opening::Failed(reason) => chrome::page(failed(reason)).into_element(),
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
    // The next main window opens where this one was.
    effect(|| shell::left_at(global::<SelectedPlace>().with(|s| s.0)));
    // The tray's and the notifications': before this mailbox opened, and from now on.
    let (asked, mut asks) = shell::follow_asks();
    asked.into_iter().for_each(on_ask);
    spawn(async move {
        while let Some(ask) = asks.next().await {
            on_ask(ask);
        }
    });
    // No account: the sign-in page, which can't be left.
    effect(|| {
        let none = global::<Accounts>().with(|a| a.0.as_ref().is_some_and(Vec::is_empty));
        if none && untracked(|| global::<SignIn>().get().0.is_none()) {
            login::add_account(false);
        }
    });
    composer::bind_keys();
    settings::bind_keys(true);
    on_key("mod+shift+n", |_| sidebar::sync());
    on_key("F5", |_| sidebar::sync());
    on_key("mod+f", |_| list::focus_search());
    // Open at once, for looking at it without a mouse (screenshots).
    if std::env::var_os("CATMAIL_GUI_SETTINGS").is_some() {
        settings::open();
    }
    // Escape closes what is open.
    on_key("escape", |_| global::<Selected>().set(Selected(None)));
    view()
        .flex_1()
        .child(dynamic(move || match global::<SignIn>().get().0 {
            Some(cancellable) => chrome::page(login::sign_in_page(cancellable)).into_element(),
            None => panes::panes().into_element(),
        }))
        .child(settings::dialog())
        .child(login::identity_dialog())
        .child(form::name_dialog())
}

fn on_ask(ask: Ask) {
    match ask {
        Ask::GoTo(to) => sidebar::go_to(to),
        Ask::Compose => sidebar::compose(),
    }
}

/// Follows the engine's events (`Refresh::after`): what they change is
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
    let shown = global::<Listing>().with_untracked(|l| l.source().cloned());
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
    in_view(async {
        timer(REFRESH_INTERVAL).await;
        let held = throttle().borrow_mut().elapsed();
        read_again(held);
    });
}
