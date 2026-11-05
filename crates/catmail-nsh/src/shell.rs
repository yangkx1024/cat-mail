//! The GTK application around the nsh views: its windows, what later launches, the tray
//! and notifications ask for, running on with no window, quitting, and the engine's
//! opening and shutting down. nsh's `run_app` runs one view in one window and ends with
//! it; the app has its own `gtk::Application` instead, so that windows can come and go
//! while the engine stays (composer windows, running without a window).
//!
//! All of this runs on GTK's thread. Each window's view runs on a thread of its own (nsh's
//! engine thread), and reaches what is here with [`on_gtk`].

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, PoisonError};

use catmail_core::{DraftId, MailClientBuilder};
use catmail_frontend::composer::{ComposeFrom, OpenComposers, Opening, window_title};
use catmail_frontend::instance::{Request, Requests};
use catmail_frontend::mailbox::{Destination, Place};
use catmail_frontend::resident::{AfterLastWindow, QuitReason, after_last_window};
use catmail_frontend::settings::{self, Settings, SettingsFile};
use catmail_frontend::theme::{DEFAULT_DARK, DEFAULT_LIGHT, Themes};
use catmail_frontend::tray::Command;
use futures_channel::{mpsc, oneshot};
use futures_util::StreamExt as _;
use nsh_platform_linux::gtk4::{self as gtk, glib, prelude::*};
use nsh_platform_linux::{App, AppOptions, AppView, Appearance, Root};

use crate::engine;

/// The application id: Wayland's app id, X11's WM_CLASS, the `.desktop` file's name.
pub const APP_ID: &str = "net.yangkx.catmail.nsh";

/// The app's name: the main window's title, the tray item's.
pub const APP_NAME: &str = "catmail";

// --- Settings and themes ------------------------------------------------------------
//
// Process-wide: every window's view reads them, each on its own thread, and the settings
// dialog changes them from the main window's.

/// The settings in effect.
static SETTINGS: Mutex<Option<Settings>> = Mutex::new(None);

/// Where the settings are written, and the themes folder next to them; `None` without a
/// config directory, and they stay in memory.
static FILE: OnceLock<Option<Arc<SettingsFile>>> = OnceLock::new();

/// The themes on offer.
static THEMES: LazyLock<Mutex<Themes>> = LazyLock::new(Mutex::default);

/// The views following the settings ([`follow_settings`]).
static FOLLOWERS: Mutex<Vec<mpsc::UnboundedSender<Settings>>> = Mutex::new(Vec::new());

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The settings in effect.
pub fn settings() -> Settings {
    lock(&SETTINGS).clone().unwrap_or_default()
}

/// Changes the settings, from any view: they are written out in the background and take
/// effect in every window.
pub fn update_settings(change: impl FnOnce(&mut Settings)) {
    let settings = {
        let mut current = lock(&SETTINGS);
        let settings = current.get_or_insert_with(Settings::default);
        let before = settings.clone();
        change(settings);
        if *settings == before {
            return;
        }
        settings.clone()
    };
    if let Some(file) = FILE.get().cloned().flatten() {
        file.queue(settings.clone());
        // Writes take the newest settings queued, whichever thread finishes first.
        std::thread::spawn(move || file.write_pending());
    }
    lock(&FOLLOWERS).retain(|follower| follower.unbounded_send(settings.clone()).is_ok());
    glib::MainContext::default().invoke(restyle_views);
}

/// The settings each time they change, for a view to follow (its appearance, the dialog
/// that shows them).
pub fn follow_settings() -> mpsc::UnboundedReceiver<Settings> {
    let (follower, changes) = mpsc::unbounded();
    lock(&FOLLOWERS).push(follower);
    changes
}

/// The themes folder, if there is one.
pub fn themes_dir() -> Option<PathBuf> {
    FILE.get()?.as_ref().map(|file| file.themes_dir())
}

/// The themes on offer, to read or to import into. A change to them takes effect in every
/// window with [`themes_changed`].
pub fn with_themes<R>(f: impl FnOnce(&mut Themes) -> R) -> R {
    f(&mut lock(&THEMES))
}

/// The themes changed: the settings' ones may be new.
pub fn themes_changed() {
    glib::MainContext::default().invoke(restyle_views);
}

/// The settings' light and dark themes as nsh's theme overrides.
fn theme_overrides(settings: &Settings) -> Option<String> {
    let themes = lock(&THEMES);
    let light = themes.pick(&settings.light_theme, DEFAULT_LIGHT)?;
    let dark = themes.pick(&settings.dark_theme, DEFAULT_DARK)?;
    Some(crate::theme::css_vars(&light.config, &dark.config))
}

thread_local! {
    /// Every window's view, for the settings to reach (GTK's thread).
    static VIEWS: RefCell<Vec<Weak<AppView>>> = RefCell::default();
}

/// Gives `view` the settings' theme and text size. Its appearance is the view's to set
/// (`set_color_scheme`), on its own thread.
fn restyle(view: &AppView, settings: &Settings) {
    view.set_font_scale(Some(settings.text_size / settings::DEFAULT_TEXT_SIZE));
    if let Err(e) = view.set_theme_overrides(theme_overrides(settings).as_deref()) {
        tracing::warn!(error = %e, "couldn't apply the theme");
    }
}

fn restyle_views() {
    let settings = settings();
    VIEWS.with_borrow_mut(|views| {
        views.retain(|view| view.upgrade().inspect(|v| restyle(v, &settings)).is_some());
    });
}

// --- The main window's mailbox, asked from outside it --------------------------------

/// What the tray or a notification asks of the main window's mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// Go there, once the folders are read.
    GoTo(Destination),
    /// A new message, from the account shown.
    Compose,
}

struct Asks {
    /// The main window's mailbox, while it follows them.
    mailbox: Option<mpsc::UnboundedSender<Ask>>,
    /// Asked while there was none: the next takes them.
    waiting: Vec<Ask>,
    /// Where the last one was, for the next to open there.
    left_at: Option<Place>,
}

static ASKS: Mutex<Asks> = Mutex::new(Asks {
    mailbox: None,
    waiting: Vec::new(),
    left_at: None,
});

fn ask(ask: Ask) {
    let mut asks = lock(&ASKS);
    let sent = asks
        .mailbox
        .as_ref()
        .is_some_and(|mailbox| mailbox.unbounded_send(ask).is_ok());
    if !sent {
        asks.mailbox = None;
        asks.waiting.push(ask);
    }
}

/// For the main window's mailbox, as it opens: what was asked of it while there was none,
/// starting with where the last one was (unless a notification asked for elsewhere), and
/// what is asked from now on.
pub fn follow_asks() -> (Vec<Ask>, mpsc::UnboundedReceiver<Ask>) {
    let (mailbox, asks_) = mpsc::unbounded();
    let mut asks = lock(&ASKS);
    asks.mailbox = Some(mailbox);
    let mut waiting = std::mem::take(&mut asks.waiting);
    if !waiting.iter().any(|ask| matches!(ask, Ask::GoTo(_)))
        && let Some(place) = asks.left_at
    {
        waiting.insert(0, Ask::GoTo(Destination::Place(place)));
    }
    (waiting, asks_)
}

/// The main window's mailbox shows `place`: the next opens there.
pub fn left_at(place: Option<Place>) {
    lock(&ASKS).left_at = place;
}

/// Shows the main window (opened if there is none), taken to `to`. `activation_token` is
/// what the tray clicked gave, if anything, to take the focus on Wayland.
pub fn show_at(to: Option<Destination>, activation_token: Option<&str>) {
    if let Some(to) = to {
        ask(Ask::GoTo(to));
    }
    if let Some(app) = application() {
        show(&app, activation_token);
    }
}

/// Does what was chosen in the tray.
pub fn chose(command: Command, activation_token: Option<String>) {
    let token = activation_token.as_deref();
    match command {
        Command::Open => show_at(None, token),
        Command::Compose => {
            ask(Ask::Compose);
            show_at(None, token);
        }
        // The engine's background sync does it, window or not: progress and failures show
        // as the accounts' status.
        Command::Sync => {
            if let Some(client) = engine::open() {
                glib::spawn_future_local(async move {
                    if let Err(e) = client.sync_now().await {
                        tracing::warn!(error = %e, "couldn't sync");
                    }
                });
            }
        }
        Command::Quit => quit(QuitReason::Tray),
    }
}

pub fn application() -> Option<gtk::Application> {
    gtk::gio::Application::default().and_downcast()
}

/// A composer's window, and how to ask its view to close it.
type ComposerWindow = (gtk::ApplicationWindow, mpsc::UnboundedSender<()>);

thread_local! {
    /// The main window, while it is open.
    static MAIN: RefCell<glib::WeakRef<gtk::ApplicationWindow>> = RefCell::default();
    static COMPOSERS: RefCell<OpenComposers<ComposerWindow>> = RefCell::default();
    /// Held while the app runs with no window: GTK's app ends with its last window
    /// otherwise.
    static RESIDENT: RefCell<Option<gtk::gio::ApplicationHoldGuard>> = RefCell::default();
    /// Quit was asked for, and why: the composers are closing, or the app is quitting.
    static QUITTING: Cell<Option<QuitReason>> = const { Cell::new(None) };
}

/// Runs `f` on GTK's thread, from a view's: at once, whether the answer is awaited or
/// not. `None` once the app has stopped.
pub fn on_gtk<R: Send + 'static>(
    f: impl FnOnce() -> R + Send + 'static,
) -> impl Future<Output = Option<R>> {
    let (sent, answer) = oneshot::channel();
    glib::MainContext::default().invoke(move || {
        let _ = sent.send(f());
    });
    async move { answer.await.ok() }
}

/// Runs the app until it quits, then shuts the engine down.
pub fn run(
    builder: MailClientBuilder,
    requests: Option<Requests>,
    file: Option<SettingsFile>,
) -> glib::ExitCode {
    glib::set_prgname(Some(APP_ID));
    // Not unique through D-Bus: one app per data directory is the instance socket's job,
    // shared with catmail-gui (`catmail_frontend::instance`).
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let file = FILE.get_or_init(|| file.map(Arc::new));
    *lock(&SETTINGS) = Some(file.as_ref().map(|file| file.load()).unwrap_or_default());
    let themes_dir = file.as_ref().map(|file| file.themes_dir());
    *lock(&THEMES) = Themes::load(themes_dir.as_deref());
    let starting = RefCell::new(Some((builder, requests)));
    app.connect_startup(move |app| {
        let Some((builder, requests)) = starting.borrow_mut().take() else {
            return;
        };
        // On GTK's loop: the client does the work on the engine's own runtime.
        let weak = app.downgrade();
        glib::spawn_future_local(async move {
            engine::opened_with(builder.open_async().await);
            let Some(app) = weak.upgrade() else {
                return;
            };
            if engine::open().is_some() {
                crate::new_mail::start(&app);
                crate::tray::start();
            } else if app.windows().is_empty() {
                // The window was closed while it opened: there is nothing to run on.
                exit(QuitReason::EngineFailed);
            }
        });
        if let Some(requests) = requests {
            glib::spawn_future_local(answer(requests, app.downgrade()));
        }
        install_quit(app);
        quit_on_signals(app);
    });
    app.connect_activate(|app| show(app, None));
    app.connect_window_removed(|app, _| window_removed(app));
    // GTK must not parse the app's own arguments.
    let code = app.run_with_args::<&str>(&[]);
    let reason = QUITTING.get().unwrap_or(QuitReason::Platform);
    tracing::info!(%reason, "quitting");
    if let Some(client) = engine::take() {
        client.shut_down();
        // The loop is over: nothing else runs on this thread meanwhile.
        glib::MainContext::default().block_on(client.closed());
    }
    code
}

/// What later launches ask for, for as long as the app runs.
async fn answer(mut requests: Requests, app: glib::WeakRef<gtk::Application>) {
    while let Some(request) = requests.next().await {
        let Some(app) = app.upgrade() else {
            return;
        };
        match request {
            Request::Activate { display } => {
                show(&app, display.activation_token.as_deref());
            }
        }
    }
}

/// The main window in front, opened if there is none. `activation_token` is the asking
/// launch's, which lets the window take the focus on Wayland.
fn show(app: &gtk::Application, activation_token: Option<&str>) {
    let window = match MAIN.with_borrow(|main| main.upgrade()) {
        Some(window) => window,
        None => {
            let window = main_window(app);
            MAIN.with_borrow_mut(|main| main.set(Some(&window)));
            window
        }
    };
    if let Some(token) = activation_token {
        window.set_startup_id(token);
    }
    window.present();
    // The window holds the app now.
    RESIDENT.take();
}

fn main_window(app: &gtk::Application) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(APP_NAME)
        .default_width(1280)
        .default_height(800)
        .build();
    show_view(&window, APP_NAME, Root::of::<crate::app::App>());
    tracing::info!("main window opened");
    // Its mailbox goes with it: what is asked from now on waits for the next.
    window.connect_destroy(|_| {
        tracing::info!("main window closed");
        lock(&ASKS).mailbox = None;
    });
    window
}

/// Puts a view running `root` in `window`, which keeps it while it is open.
fn show_view(window: &gtk::ApplicationWindow, title: &str, root: Root) {
    let settings = settings();
    let view = AppView::new(AppOptions {
        title: title.to_owned(),
        app_id: APP_ID.to_owned(),
        appearance: appearance(&settings),
        ..Default::default()
    });
    // Before it loads: the app starts with them.
    restyle(&view, &settings);
    VIEWS.with_borrow_mut(|views| views.push(Rc::downgrade(&view)));
    window.set_child(Some(&view.widget()));
    view.load(App::Rust(root));
    close_on_ctrl_w(window);
    blur_on_press_outside(window);
    // The window keeps the view and its nsh engine; closing it stops them.
    let kept = RefCell::new(Some(view));
    window.connect_destroy(move |_| {
        if let Some(view) = kept.borrow_mut().take() {
            view.stop();
        }
    });
}

// --- Composer windows ---------------------------------------------------------------

/// Brings the draft's window forward, if it has one.
fn activate(draft: DraftId) -> bool {
    let window = COMPOSERS.with_borrow(|open| open.window(draft));
    window.inspect(|(window, _)| window.present()).is_some()
}

/// Whether to read `from`'s draft for a window: not when it has one (brought forward
/// instead) or is on its way.
pub fn begin_opening(from: ComposeFrom) -> bool {
    match from {
        ComposeFrom::Draft(id) => {
            !activate(id) && COMPOSERS.with_borrow_mut(|open| open.begin_opening(id))
        }
        _ => true,
    }
}

/// `from`'s draft was made or read (or couldn't be, `None`): opens a window on it, or
/// brings forward the one it has (a reply or forward made it, and it was opened from the
/// drafts before its window came up).
pub fn opened(from: ComposeFrom, opening: Option<Opening>) {
    if let ComposeFrom::Draft(id) = from {
        COMPOSERS.with_borrow_mut(|open| open.end_opening(id));
    }
    let Some(opening) = opening else {
        return;
    };
    let id = opening.draft.id;
    if activate(id) {
        return;
    }
    let Some(app) = gtk::gio::Application::default().and_downcast::<gtk::Application>() else {
        return;
    };
    let title = window_title(&opening.draft.content.subject);
    let window = gtk::ApplicationWindow::builder()
        .application(&app)
        .title(title.as_str())
        .default_width(760)
        .default_height(680)
        .build();
    window.set_size_request(480, 360);
    let (asks, close_requests) = mpsc::unbounded();
    let start = Arc::new(crate::composer::Start {
        opening,
        close_requests: Mutex::new(Some(close_requests)),
    });
    show_view(&window, &title, crate::composer::root(start));
    // The view decides: it saves first, and closes the window itself (`composer_closed`).
    // One that is gone can't, and the window just goes.
    let ask = asks.clone();
    window.connect_close_request(move |_| {
        if ask.unbounded_send(()).is_ok() {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.connect_destroy(move |_| COMPOSERS.with_borrow_mut(|open| open.remove(id)));
    COMPOSERS.with_borrow_mut(|open| open.insert(id, (window.clone(), asks)));
    window.present();
}

/// The draft's composer is done: its window goes.
pub fn composer_closed(draft: DraftId) {
    let window = COMPOSERS.with_borrow_mut(|open| {
        let window = open.window(draft);
        open.remove(draft);
        window
    });
    if let Some((window, _)) = window {
        window.destroy();
    }
}

/// A composer stays open, to be dealt with: closing them all failed.
pub fn stop_closing_all() {
    COMPOSERS.with_borrow_mut(|open| open.stop_closing_all());
}

/// Asks every composer to close, once it has saved what it holds: true once the last
/// has, false as soon as one can't (an address to correct, a save that failed).
fn close_all() -> oneshot::Receiver<bool> {
    let (windows, answer) = COMPOSERS.with_borrow_mut(|open| open.close_all());
    for (_, asks) in windows {
        let _ = asks.unbounded_send(());
    }
    answer
}

/// The settings' appearance; `CATMAIL_NSH_APPEARANCE` (`dark`, `light`) overrides it, for
/// screenshots of the spike that leave the settings alone.
pub fn appearance(settings: &Settings) -> Appearance {
    match std::env::var("CATMAIL_NSH_APPEARANCE").as_deref() {
        Ok("dark") => return Appearance::Dark,
        Ok("light") => return Appearance::Light,
        _ => {}
    }
    match settings.appearance {
        settings::Appearance::System => Appearance::System,
        settings::Appearance::Light => Appearance::Light,
        settings::Appearance::Dark => Appearance::Dark,
    }
}

// --- Running without a window, and quitting -----------------------------------------

/// The last window is gone: the app quits, or with "Keep running after the window is
/// closed" on, runs on until the tray, a notification or another launch shows the window
/// again. ⚠️ GTK stays connected to the display meanwhile (the GUI lets go of it).
fn window_removed(app: &gtk::Application) {
    if !app.windows().is_empty() {
        return;
    }
    let quitting = QUITTING.get();
    match after_last_window(
        settings().keep_running,
        engine::usable(),
        quitting.is_some(),
    ) {
        // The app ends with its last window.
        AfterLastWindow::Quit => {
            QUITTING.set(Some(quitting.unwrap_or(QuitReason::LastWindowClosed)))
        }
        AfterLastWindow::KeepRunning => {
            RESIDENT.set(Some(app.hold()));
            tracing::info!("no window open; running on");
            // Once the views' threads have let go of what they held.
            glib::timeout_add_seconds_local_once(2, || {
                if RESIDENT.with_borrow(Option::is_some) {
                    std::thread::spawn(catmail_frontend::memory::release_freed);
                }
            });
        }
    }
}

/// Quits once every composer has saved what it holds and closed. One that can't close
/// (an address to correct, a save that failed) stays open, and so does the app.
pub fn quit(reason: QuitReason) {
    if QUITTING.get().is_some() {
        return;
    }
    QUITTING.set(Some(reason));
    let closed = close_all();
    glib::spawn_future_local(async move {
        if closed.await == Ok(true) {
            exit(reason);
        } else {
            tracing::info!(%reason, "quit called off: a composer stayed open");
            QUITTING.set(None);
        }
    });
}

/// Quits now, for `reason`: every window closes, and the app ends with the last (at once
/// if it has none). The engine shuts down after the loop.
fn exit(reason: QuitReason) {
    QUITTING.set(Some(reason));
    RESIDENT.take();
    if let Some(app) = application() {
        for window in app.windows() {
            window.close();
        }
    }
}

/// Ctrl+Q quits as the tray's Quit does.
fn install_quit(app: &gtk::Application) {
    let quit_ = gtk::gio::SimpleAction::new("quit", None);
    quit_.connect_activate(|_, _| quit(QuitReason::Shortcut));
    app.add_action(&quit_);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}

/// SIGHUP, SIGINT and SIGTERM quit as Ctrl+Q does, so that the composers save first (a
/// session ending sends SIGTERM) and the engine shuts down rather than being cut off (as
/// the GUI does, `ctrlc` there). A second signal doesn't wait for the composers.
fn quit_on_signals(app: &gtk::Application) {
    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    for signal in [SIGHUP, SIGINT, SIGTERM] {
        let weak = app.downgrade();
        glib_unix::unix_signal_add_local(signal, move || {
            if QUITTING.get().is_none() {
                tracing::info!(signal, "quitting on a signal");
                quit(QuitReason::Signal);
            } else if let Some(app) = weak.upgrade() {
                tracing::info!(signal, "quitting at once, on another signal");
                QUITTING.set(Some(QuitReason::Signal));
                app.quit();
            }
            glib::ControlFlow::Continue
        });
    }
}

/// GTK's window shortcut, as nsh's own windows have it. Bubbling, so a view that binds
/// Ctrl+W itself comes first.
fn close_on_ctrl_w(window: &gtk::ApplicationWindow) {
    let close = gtk::ShortcutController::new();
    close.set_propagation_phase(gtk::PropagationPhase::Bubble);
    close.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>w"),
        Some(gtk::NamedAction::new("window.close")),
    ));
    window.add_controller(close);
}

/// A press outside the text field that has the focus takes the focus off it, as in a
/// browser: typing in the search field, then clicking a row, gives the keys back to the
/// window (E, X, L act on messages only while no field takes them). nsh's backend only
/// sends a press to the innermost view that handles it, so no view of the app sees them
/// all; the window does, before its widgets (capture), and leaves the press to them.
fn blur_on_press_outside(window: &gtk::ApplicationWindow) {
    let press = gtk::GestureClick::new();
    press.set_button(0);
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = window.downgrade();
    press.connect_pressed(move |_, _, x, y| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let Some(focus) = GtkWindowExt::focus(&window) else {
            return;
        };
        if !(focus.is::<gtk::Text>() || focus.is::<gtk::TextView>()) {
            return;
        }
        let on_it = window
            .pick(x, y, gtk::PickFlags::DEFAULT)
            .is_some_and(|picked| picked == focus || picked.is_ancestor(&focus));
        if !on_it {
            GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        }
    });
    window.add_controller(press);
}
