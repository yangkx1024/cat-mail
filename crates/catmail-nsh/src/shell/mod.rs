//! The application around the nsh views: its windows, what later launches, the tray and
//! notifications ask for, running on with no window, quitting, and the engine's opening
//! and shutting down. nsh's `run_app` runs one view in one window and ends with it; the
//! app runs the toolkit's application itself instead (`gtk::Application` on Linux,
//! `NSApplication` on macOS: [`toolkit`]), so that windows can come and go while the
//! engine stays (composer windows, running without a window).
//!
//! All of this runs on the main thread, the toolkit's. Each window's view runs on a
//! thread of its own (nsh's engine thread), and reaches what is here with [`on_main`].

#[cfg(target_os = "macos")]
#[path = "appkit.rs"]
mod toolkit;
#[cfg(target_os = "linux")]
#[path = "gtk.rs"]
mod toolkit;

#[cfg(target_os = "linux")]
pub use self::toolkit::application;
/// Runs a future on the main thread's loop, from the main thread.
pub use self::toolkit::spawn;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, PoisonError};
use std::time::Duration;

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

use self::toolkit::Window;
use crate::engine;
use crate::nsh_platform::{App, AppOptions, AppView, Appearance, Root};

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
    toolkit::invoke(restyle_views);
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
    toolkit::invoke(restyle_views);
}

/// The settings' light and dark themes as nsh's theme overrides.
fn theme_overrides(settings: &Settings) -> Option<String> {
    let themes = lock(&THEMES);
    let light = themes.pick(&settings.light_theme, DEFAULT_LIGHT)?;
    let dark = themes.pick(&settings.dark_theme, DEFAULT_DARK)?;
    Some(crate::theme::css_vars(&light.config, &dark.config))
}

thread_local! {
    /// Every window's view, for the settings to reach (the main thread).
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
    toolkit::set_appearance(appearance(&settings));
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
    show(activation_token);
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
                toolkit::spawn(async move {
                    if let Err(e) = client.sync_now().await {
                        tracing::warn!(error = %e, "couldn't sync");
                    }
                });
            }
        }
        Command::Quit => quit(QuitReason::Tray),
    }
}

/// A composer's window, and how to ask its view to close it.
type ComposerWindow = (Window, mpsc::UnboundedSender<()>);

thread_local! {
    /// The main window, while it is open.
    static MAIN: RefCell<Option<Window>> = RefCell::default();
    static COMPOSERS: RefCell<OpenComposers<ComposerWindow>> = RefCell::default();
    /// Quit was asked for, and why: the composers are closing, or the app is quitting.
    static QUITTING: Cell<Option<QuitReason>> = const { Cell::new(None) };
}

/// Runs `f` on the main thread, from a view's: at once, whether the answer is awaited or
/// not. `None` once the app has stopped.
pub fn on_main<R: Send + 'static>(
    f: impl FnOnce() -> R + Send + 'static,
) -> impl Future<Output = Option<R>> {
    let (sent, answer) = oneshot::channel();
    toolkit::invoke(move || {
        let _ = sent.send(f());
    });
    async move { answer.await.ok() }
}

/// Runs the app until it quits, then shuts the engine down: the process's exit code.
pub fn run(
    builder: MailClientBuilder,
    requests: Option<Requests>,
    file: Option<SettingsFile>,
) -> u8 {
    let file = FILE.get_or_init(|| file.map(Arc::new));
    *lock(&SETTINGS) = Some(file.as_ref().map(|file| file.load()).unwrap_or_default());
    let themes_dir = file.as_ref().map(|file| file.themes_dir());
    *lock(&THEMES) = Themes::load(themes_dir.as_deref());
    let code = toolkit::run(move || {
        toolkit::set_appearance(appearance(&settings()));
        started(builder, requests);
    });
    let reason = QUITTING.get().unwrap_or(QuitReason::Platform);
    tracing::info!(%reason, "quitting");
    if let Some(client) = engine::take() {
        client.shut_down();
        // The loop is over: nothing else runs on this thread meanwhile.
        pollster::block_on(client.closed());
    }
    code
}

/// The toolkit's application has started, before its first window: the engine opens,
/// and the app answers later launches and the signals.
fn started(builder: MailClientBuilder, requests: Option<Requests>) {
    // On the main thread's loop: the client does the work on the engine's own runtime.
    toolkit::spawn(async move {
        engine::opened_with(builder.open_async().await);
        if engine::open().is_some() {
            toolkit::engine_opened();
        } else if toolkit::windows().is_empty() {
            // The window was closed while it opened: there is nothing to run on.
            exit(QuitReason::EngineFailed);
        }
    });
    if let Some(requests) = requests {
        toolkit::spawn(answer(requests));
    }
}

/// What later launches ask for, for as long as the app runs.
async fn answer(mut requests: Requests) {
    while let Some(request) = requests.next().await {
        match request {
            Request::Activate { display } => show(display.activation_token.as_deref()),
        }
    }
}

/// The main window in front, opened if there is none. `activation_token` is the asking
/// launch's, which lets the window take the focus on Wayland.
fn show(activation_token: Option<&str>) {
    let window = match MAIN.with_borrow(Clone::clone) {
        Some(window) => window,
        None => {
            let window = main_window();
            MAIN.set(Some(window.clone()));
            window
        }
    };
    window.present(activation_token);
}

fn main_window() -> Window {
    let spec = Spec {
        title: APP_NAME,
        size: (1280, 800),
        min_size: None,
    };
    // Its mailbox goes with it: what is asked from now on waits for the next.
    let hooks = Hooks {
        close_requested: None,
        gone: Box::new(|| {
            tracing::info!("main window closed");
            MAIN.take();
            lock(&ASKS).mailbox = None;
        }),
    };
    let window = open_window(&spec, Root::of::<crate::app::App>(), hooks);
    tracing::info!("main window opened");
    window
}

/// What a window opens with.
pub struct Spec<'a> {
    pub title: &'a str,
    pub size: (i32, i32),
    pub min_size: Option<(i32, i32)>,
}

/// What the app does when a window is asked to close and when it is gone.
pub struct Hooks {
    /// The user asked to close it: true if the app sees to it, and the window stays for
    /// now.
    pub close_requested: Option<Box<dyn Fn() -> bool>>,
    /// The window is gone.
    pub gone: Box<dyn FnOnce()>,
}

/// Opens a window running `root` in its view, which the window keeps while it is open.
fn open_window(spec: &Spec, root: Root, hooks: Hooks) -> Window {
    let settings = settings();
    let view = toolkit::new_view(AppOptions {
        title: spec.title.to_owned(),
        app_id: APP_ID.to_owned(),
        appearance: appearance(&settings),
        ..Default::default()
    });
    // Before it loads: the app starts with them.
    restyle(&view, &settings);
    VIEWS.with_borrow_mut(|views| views.push(Rc::downgrade(&view)));
    let window = Window::open(spec, &view, hooks);
    view.load(App::Rust(root));
    window
}

// --- Composer windows ---------------------------------------------------------------

/// Brings the draft's window forward, if it has one.
fn activate(draft: DraftId) -> bool {
    let window = COMPOSERS.with_borrow(|open| open.window(draft));
    window.inspect(|(window, _)| window.present(None)).is_some()
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
    let title = window_title(&opening.draft.content.subject);
    let spec = Spec {
        title: &title,
        size: (760, 680),
        min_size: Some((480, 360)),
    };
    let (asks, close_requests) = mpsc::unbounded();
    let start = Arc::new(crate::composer::Start {
        opening,
        close_requests: Mutex::new(Some(close_requests)),
    });
    // The view decides: it saves first, and closes the window itself (`composer_closed`).
    // One that is gone can't, and the window just goes.
    let ask = asks.clone();
    let hooks = Hooks {
        close_requested: Some(Box::new(move || ask.unbounded_send(()).is_ok())),
        gone: Box::new(move || COMPOSERS.with_borrow_mut(|open| open.remove(id))),
    };
    let window = open_window(&spec, crate::composer::root(start), hooks);
    COMPOSERS.with_borrow_mut(|open| open.insert(id, (window.clone(), asks)));
    window.present(None);
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

/// A window is gone. The last: the app quits, or with "Keep running after the window is
/// closed" on, runs on until the tray, a notification or another launch shows the window
/// again (on macOS, the Dock too). ⚠️ On Linux, GTK stays connected to the display
/// meanwhile (the GUI lets go of it).
fn window_removed() {
    if !toolkit::windows().is_empty() {
        return;
    }
    let quitting = QUITTING.get();
    match after_last_window(
        settings().keep_running,
        engine::usable(),
        quitting.is_some(),
    ) {
        AfterLastWindow::Quit => {
            QUITTING.set(Some(quitting.unwrap_or(QuitReason::LastWindowClosed)));
            toolkit::end();
        }
        AfterLastWindow::KeepRunning => {
            tracing::info!("no window open; running on");
            // Once the views' threads have let go of what they held.
            toolkit::after(Duration::from_secs(2), || {
                if toolkit::windows().is_empty() && QUITTING.get().is_none() {
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
    toolkit::spawn(async move {
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
    let windows = toolkit::windows();
    if windows.is_empty() {
        toolkit::end();
    }
    for window in windows {
        window.close();
    }
}

/// SIGHUP, SIGINT or SIGTERM: quits as Ctrl+Q does, so that the composers save first (a
/// session ending sends SIGTERM) and the engine shuts down rather than being cut off (as
/// the GUI does, `ctrlc` there). A second signal doesn't wait for the composers.
fn signalled(signal: &str) {
    if QUITTING.get().is_none() {
        tracing::info!(signal, "quitting on a signal");
        quit(QuitReason::Signal);
    } else {
        tracing::info!(signal, "quitting at once, on another signal");
        QUITTING.set(Some(QuitReason::Signal));
        // The views stop with their windows, while the thread's locals they use are there.
        for window in toolkit::windows() {
            window.destroy();
        }
        toolkit::end();
    }
}
