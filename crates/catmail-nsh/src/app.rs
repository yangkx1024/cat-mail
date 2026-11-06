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
use futures_util::StreamExt as _;
use nsh_base::theme;
use nsh_base::{HandleEdge, resize_handle};
use nsh_core::element::Owner;
use nsh_core::prelude::*;

use crate::list::{self, Rows, Selected};
use crate::login::{self, SignIn};
use crate::shell::Ask;
use crate::sidebar::{self, Accounts, SelectedPlace};
use crate::toast::{Level, notify};
use crate::{commands, composer, draft, form, outbox, reader, settings, shell, toast};

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

/// Runs `f` on a thread of its own (reading a file, say), for its result; `None` if the
/// thread panicked.
pub async fn off_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (done, result) = futures_channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = done.send(f());
    });
    result.await.ok()
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
    settings::follow();
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
    // No account: the sign-in page, which can't be left (as the GUI's `set_accounts`).
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
    // Open at once, for looking at it without a mouse (screenshots of the spike).
    if std::env::var_os("CATMAIL_NSH_SETTINGS").is_some() {
        settings::open();
    }
    // As the GUI's: Escape closes what is open.
    on_key("escape", |_| global::<Selected>().set(Selected(None)));
    view()
        .flex_1()
        .child(dynamic(move || match global::<SignIn>().get().0 {
            Some(cancellable) => login::sign_in_page(cancellable).into_element(),
            None => panes().into_element(),
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

/// Which pane the row selected opens.
#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Reader,
    Draft,
    Outbox,
}

/// How wide the sidebar and the list are, as the user dragged them: the GUI's sizes, for
/// as long as the app runs (the GUI doesn't keep them either).
#[derive(Clone, Copy, PartialEq)]
struct PaneWidths {
    sidebar: f32,
    list: f32,
}

impl Default for PaneWidths {
    fn default() -> Self {
        Self {
            sidebar: 240.0,
            list: 368.0,
        }
    }
}

/// The sidebar's range, and the list's while a pane is open beside it (15, 12 to 24 and
/// 23, 16 to 40 rem in the GUI).
const SIDEBAR_WIDTHS: (f32, f32) = (192.0, 384.0);
const LIST_WIDTHS: (f32, f32) = (256.0, 640.0);

/// The handle on a pane's trailing edge, which drags the width `field` picks within
/// `range`. Each move goes from the width when the drag began, so a drag whose end goes
/// missing leaves nothing behind: the next starts from what is shown.
fn edge_handle(field: fn(&mut PaneWidths) -> &mut f32, range: (f32, f32)) -> impl IntoElement {
    let widths = global::<PaneWidths>();
    let from = stored(0.0_f32);
    resize_handle()
        .edge(HandleEdge::Trailing)
        // As GPUI Kit's: a grip on the line while the pointer is there.
        .thumb_class(
            "h-5 w-[3px] rounded-full bg-muted-foreground opacity-0 \
             group-hover:opacity-35 group-active:h-11 group-active:opacity-90 transition",
        )
        .on_drag_start(move || {
            from.replace(*field(&mut widths.get_untracked()));
        })
        .on_drag_move(move |by: &Points| {
            let mut next = widths.get();
            *field(&mut next) = (from.get() + by.0).clamp(range.0, range.1);
            // `set` skips an unchanged width: one held at its bound restyles nothing.
            widths.set(next);
        })
}

fn panes() -> impl IntoElement {
    // By the kind of row alone: going from one message to the next doesn't make the panes
    // again.
    let pane = computed(|| {
        global::<Selected>().with(|s| {
            s.0.map(|row| match row {
                RowId::Message(_) => Pane::Reader,
                RowId::Draft(_) => Pane::Draft,
                RowId::Outbox(_) => Pane::Outbox,
            })
        })
    });
    let open = move || pane.get().is_some();
    let reading = move || pane.get() == Some(Pane::Reader);
    let widths = global::<PaneWidths>();
    // Apart, so a drag of one edge restyles only what that width places.
    let sidebar_width = computed(move || widths.with(|w| w.sidebar));
    let list_width = computed(move || widths.with(|w| w.list));
    let sidebar_width = move || sidebar_width.get();
    let list_width = move || list_width.get();
    view()
        .flex_1()
        .flex_row()
        // Always there, and out of sight while no message is selected: its web view keeps
        // its web process (100 MB, and a moment to start), so the first message opened
        // shows as quickly as the next. Under the list then, which covers it, rather than
        // not drawn (`invisible`): a web view that isn't drawn for a while shows a black
        // frame when it is drawn again, before WebKit paints. Not left out of the layout
        // either: nsh shows a native view only with a size, and WebKit loads a page only
        // into one shown. Placed by the widths, so it stays one view whatever pane shows;
        // first, so everything else is drawn over it.
        .child(
            reader::reader()
                .absolute()
                .top(px(0.0))
                .bottom(px(0.0))
                .left(move || px(sidebar_width() + if reading() { list_width() } else { 0.0 }))
                .style_when(reading, |s| s.right(px(0.0)))
                // Under the list: as wide as the list can be narrow, so dragging the
                // list's edge doesn't resize a web view no one sees.
                .style_when(move || !reading(), |s| s.w(px(LIST_WIDTHS.0))),
        )
        .child(
            view()
                .relative()
                .w(move || px(sidebar_width()))
                .flex_shrink_0()
                .border_r_1()
                .border_color(move || theme().border)
                .child(sidebar::sidebar())
                .child(edge_handle(|w| &mut w.sidebar, SIDEBAR_WIDTHS)),
        )
        // The window's width to the list until a row opens; then as wide as it was.
        .child(
            view()
                .relative()
                .flex_row()
                .w(move || {
                    if open() {
                        px(list_width()).into()
                    } else {
                        Length::Auto
                    }
                })
                .flex_grow(move || if open() { 0.0_f32 } else { 1.0 })
                .flex_shrink_0()
                // Opaque: the reader may be under it.
                .bg(move || theme().background)
                .style_when(open, |s| {
                    s.border_r_1().border_color(move || theme().border)
                })
                .child(list::list_column())
                .child(when(open, || edge_handle(|w| &mut w.list, LIST_WIDTHS))),
        )
        .child(dynamic(move || match pane.get() {
            Some(Pane::Reader) => view().into_element(),
            Some(Pane::Draft) => draft::pane().into_element(),
            Some(Pane::Outbox) => outbox::pane().into_element(),
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
