//! The GTK application around the nsh views: its windows, what later launches ask for,
//! and the engine's opening and shutting down. nsh's `run_app` runs one view in one window
//! and ends with it; the app has its own `gtk::Application` instead, so that windows can
//! come and go while the engine stays (composer windows, running without a window).
//!
//! All of this runs on GTK's thread. Each window's view runs on a thread of its own (nsh's
//! engine thread), and reaches what is here with [`on_gtk`].

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, PoisonError};

use catmail_core::{DraftId, MailClientBuilder};
use catmail_frontend::composer::{ComposeFrom, OpenComposers, Opening, window_title};
use catmail_frontend::instance::{Request, Requests};
use catmail_frontend::settings::{self, Settings, SettingsFile};
use catmail_frontend::theme::{DEFAULT_DARK, DEFAULT_LIGHT, Themes};
use futures_channel::{mpsc, oneshot};
use futures_util::StreamExt as _;
use nsh_platform_linux::gtk4::{self as gtk, glib, prelude::*};
use nsh_platform_linux::{App, AppOptions, AppView, Appearance, Root};

use crate::engine;

/// The application id: Wayland's app id, X11's WM_CLASS, the `.desktop` file's name.
pub const APP_ID: &str = "net.yangkx.catmail.nsh";

const TITLE: &str = "catmail";

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

/// A composer's window, and how to ask its view to close it.
type ComposerWindow = (gtk::ApplicationWindow, mpsc::UnboundedSender<()>);

thread_local! {
    /// The main window, while it is open.
    static MAIN: RefCell<glib::WeakRef<gtk::ApplicationWindow>> = RefCell::default();
    static COMPOSERS: RefCell<OpenComposers<ComposerWindow>> = RefCell::default();
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
        glib::spawn_future_local(async move { engine::opened_with(builder.open_async().await) });
        if let Some(requests) = requests {
            glib::spawn_future_local(answer(requests, app.downgrade()));
        }
        install_quit(app);
        quit_on_signals(app);
    });
    app.connect_activate(|app| show(app, None));
    // GTK must not parse the app's own arguments.
    let code = app.run_with_args::<&str>(&[]);
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
}

fn main_window(app: &gtk::Application) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(TITLE)
        .default_width(1280)
        .default_height(800)
        .build();
    show_view(&window, TITLE, Root::of::<crate::app::App>());
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

/// Ctrl+Q closes every window, which ends the app (the engine shuts down after the loop).
/// The composers close first, each saving what it holds; one that can't stays, and the
/// app with it.
fn install_quit(app: &gtk::Application) {
    let quit = gtk::gio::SimpleAction::new("quit", None);
    let weak = app.downgrade();
    quit.connect_activate(move |_, _| {
        let weak = weak.clone();
        let closed = close_all();
        glib::spawn_future_local(async move {
            if closed.await != Ok(true) {
                return;
            }
            if let Some(app) = weak.upgrade() {
                for window in app.windows() {
                    window.close();
                }
            }
        });
    });
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}

/// SIGINT and SIGTERM quit as Ctrl+Q does, so the engine shuts down rather than being cut
/// off (as the GUI does, `ctrlc` there).
fn quit_on_signals(app: &gtk::Application) {
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    for signal in [SIGINT, SIGTERM] {
        let weak = app.downgrade();
        glib_unix::unix_signal_add_local(signal, move || {
            tracing::info!(signal, "quitting on a signal");
            if let Some(app) = weak.upgrade() {
                app.activate_action("quit", None);
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
