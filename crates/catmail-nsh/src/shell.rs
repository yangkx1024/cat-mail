//! The GTK application around the nsh views: its windows, what later launches ask for,
//! and the engine's opening and shutting down. nsh's `run_app` runs one view in one window
//! and ends with it; the app has its own `gtk::Application` instead, so that windows can
//! come and go while the engine stays (composer windows, running without a window).

use std::cell::RefCell;
use std::rc::Rc;

use catmail_core::MailClientBuilder;
use catmail_frontend::instance::{Request, Requests};
use catmail_frontend::settings::{self, Settings};
use futures_util::StreamExt as _;
use nsh_platform_linux::gtk4::{self as gtk, glib, prelude::*};
use nsh_platform_linux::{App, AppOptions, AppView, Appearance, Root};

use crate::engine;

/// The application id: Wayland's app id, X11's WM_CLASS, the `.desktop` file's name.
pub const APP_ID: &str = "net.yangkx.catmail.nsh";

const TITLE: &str = "catmail";

thread_local! {
    /// The settings read at launch; the windows are on this thread.
    static SETTINGS: RefCell<Settings> = RefCell::new(Settings::default());
}

/// The settings read at launch. Writing them comes with the settings dialog (P7).
pub fn settings() -> Settings {
    SETTINGS.with_borrow(Clone::clone)
}

/// Runs the app until it quits, then shuts the engine down.
pub fn run(
    builder: MailClientBuilder,
    requests: Option<Requests>,
    settings: Settings,
) -> glib::ExitCode {
    glib::set_prgname(Some(APP_ID));
    // Not unique through D-Bus: one app per data directory is the instance socket's job,
    // shared with catmail-gui (`catmail_frontend::instance`).
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    SETTINGS.set(settings.clone());
    let settings = Rc::new(settings);
    let starting = RefCell::new(Some((builder, requests)));
    let shown = settings.clone();
    app.connect_startup(move |app| {
        let Some((builder, requests)) = starting.borrow_mut().take() else {
            return;
        };
        // On GTK's loop: the client does the work on the engine's own runtime.
        glib::spawn_future_local(async move { engine::opened_with(builder.open_async().await) });
        if let Some(requests) = requests {
            glib::spawn_future_local(answer(requests, app.downgrade(), shown.clone()));
        }
        install_quit(app);
        quit_on_signals(app);
    });
    app.connect_activate(move |app| show(app, &settings, None));
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
async fn answer(
    mut requests: Requests,
    app: glib::WeakRef<gtk::Application>,
    settings: Rc<Settings>,
) {
    while let Some(request) = requests.next().await {
        let Some(app) = app.upgrade() else {
            return;
        };
        match request {
            Request::Activate { display } => {
                show(&app, &settings, display.activation_token.as_deref());
            }
        }
    }
}

/// The main window in front, opened if there is none. `activation_token` is the asking
/// launch's, which lets the window take the focus on Wayland.
fn show(app: &gtk::Application, settings: &Settings, activation_token: Option<&str>) {
    let window = match app.windows().into_iter().next() {
        Some(window) => window,
        None => main_window(app, settings).upcast(),
    };
    if let Some(token) = activation_token {
        window.set_startup_id(token);
    }
    window.present();
}

fn main_window(app: &gtk::Application, settings: &Settings) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(TITLE)
        .default_width(1280)
        .default_height(800)
        .build();
    let view = AppView::new(AppOptions {
        title: TITLE.to_owned(),
        app_id: APP_ID.to_owned(),
        appearance: appearance(settings),
        ..Default::default()
    });
    view.set_font_scale(Some(settings.text_size / settings::DEFAULT_TEXT_SIZE));
    window.set_child(Some(&view.widget()));
    view.load(App::Rust(Root::of::<crate::app::App>()));
    close_on_ctrl_w(&window);
    blur_on_press_outside(&window);
    // The window keeps the view and its nsh engine; closing it stops them.
    let kept = RefCell::new(Some(view));
    window.connect_destroy(move |_| {
        if let Some(view) = kept.borrow_mut().take() {
            view.stop();
        }
    });
    window
}

/// The settings' appearance; `CATMAIL_NSH_APPEARANCE` (`dark`, `light`) overrides it, for
/// screenshots of the spike that leave the settings alone.
fn appearance(settings: &Settings) -> Appearance {
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
fn install_quit(app: &gtk::Application) {
    let quit = gtk::gio::SimpleAction::new("quit", None);
    let weak = app.downgrade();
    quit.connect_activate(move |_, _| {
        if let Some(app) = weak.upgrade() {
            for window in app.windows() {
                window.close();
            }
        }
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
