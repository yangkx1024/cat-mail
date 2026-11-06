//! The shell on GTK 4 (Linux): a `gtk::Application`, its windows, and GLib's loop for the
//! main thread's work.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use catmail_frontend::resident::QuitReason;
use nsh_platform_linux::gtk4::{self as gtk, glib, prelude::*};
use nsh_platform_linux::{AppOptions, AppView, Appearance};

use super::{APP_ID, Hooks, Spec};

/// Runs the application until it quits. `started` runs once, before its first window.
pub fn run(started: impl FnOnce() + 'static) -> u8 {
    glib::set_prgname(Some(APP_ID));
    // Not unique through D-Bus: one app per data directory is the instance socket's job
    // (`catmail_frontend::instance`).
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let started = RefCell::new(Some(started));
    app.connect_startup(move |app| {
        if let Some(started) = started.borrow_mut().take() {
            started();
        }
        install_quit(app);
        quit_on_signals();
    });
    app.connect_activate(|_| super::show(None));
    app.connect_window_removed(|_, _| super::window_removed());
    // Held for as long as it runs: GTK's app would end with its last window, and the
    // shell decides that ([`end`]).
    let _hold = app.hold();
    // GTK must not parse the app's own arguments.
    let code = app.run_with_args::<&str>(&[]);
    code.get()
}

pub fn application() -> Option<gtk::Application> {
    gtk::gio::Application::default().and_downcast()
}

/// The engine is open: the tray and the notifications follow it.
pub fn engine_opened() {
    crate::new_mail::start();
    crate::tray::start();
}

/// Runs `future` on the main thread's loop.
pub fn spawn(future: impl Future<Output = ()> + 'static) {
    glib::spawn_future_local(future);
}

/// Runs `f` on the main thread, from any.
pub fn invoke(f: impl FnOnce() + Send + 'static) {
    glib::MainContext::default().invoke(f);
}

/// Runs `f` on the main thread once `delay` is over.
pub fn after(delay: Duration, f: impl FnOnce() + 'static) {
    glib::timeout_add_local_once(delay, f);
}

/// The app's windows.
pub fn windows() -> Vec<Window> {
    let Some(app) = application() else {
        return Vec::new();
    };
    app.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<gtk::ApplicationWindow>().ok())
        .map(|window| Window(window.downgrade()))
        .collect()
}

/// The app ends, with whatever windows it has.
pub fn end() {
    if let Some(app) = application() {
        app.quit();
    }
}

/// Each view sets its window's appearance itself.
pub fn set_appearance(_appearance: Appearance) {}

pub fn new_view(options: AppOptions) -> Rc<AppView> {
    AppView::new(options)
}

/// Weak: GTK keeps a window while it is open, and it is disposed of (its view stopped) as
/// soon as it is closed, not when the last handle goes (maybe as the thread ends, after
/// nsh's own thread locals).
#[derive(Clone)]
pub struct Window(glib::WeakRef<gtk::ApplicationWindow>);

impl Window {
    /// A window showing `view`, which it keeps while it is open and stops when it is gone.
    /// Not shown yet.
    pub fn open(spec: &Spec, view: &Rc<AppView>, hooks: Hooks) -> Self {
        let window = gtk::ApplicationWindow::builder()
            .title(spec.title)
            .default_width(spec.size.0)
            .default_height(spec.size.1)
            .build();
        if let Some(app) = application() {
            window.set_application(Some(&app));
        }
        if let Some((width, height)) = spec.min_size {
            window.set_size_request(width, height);
        }
        window.set_child(Some(&view.widget()));
        close_on_ctrl_w(&window);
        blur_on_press_outside(&window);
        if let Some(close_requested) = hooks.close_requested {
            window.connect_close_request(move |_| {
                if close_requested() {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
        }
        // The window keeps the view and its nsh engine; closing it stops them.
        let kept = RefCell::new(Some((view.clone(), hooks.gone)));
        window.connect_destroy(move |_| {
            if let Some((view, gone)) = kept.borrow_mut().take() {
                view.stop();
                gone();
            }
        });
        Self(window.downgrade())
    }

    /// Shows the window in front. `activation_token` is what asked for it gave, if
    /// anything, to take the focus on Wayland.
    pub fn present(&self, activation_token: Option<&str>) {
        let Some(window) = self.0.upgrade() else {
            return;
        };
        if let Some(token) = activation_token {
            window.set_startup_id(token);
        }
        window.present();
    }

    /// Closes the window as the user would: [`Hooks::close_requested`] may keep it.
    pub fn close(&self) {
        if let Some(window) = self.0.upgrade() {
            window.close();
        }
    }

    /// Closes the window, without asking.
    pub fn destroy(&self) {
        if let Some(window) = self.0.upgrade() {
            window.destroy();
        }
    }
}

/// Ctrl+Q quits as the tray's Quit does.
fn install_quit(app: &gtk::Application) {
    let quit_ = gtk::gio::SimpleAction::new("quit", None);
    quit_.connect_activate(|_, _| super::quit(QuitReason::Shortcut));
    app.add_action(&quit_);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}

/// SIGHUP, SIGINT and SIGTERM, on GTK's loop.
fn quit_on_signals() {
    const SIGNALS: [(i32, &str); 3] = [(1, "SIGHUP"), (2, "SIGINT"), (15, "SIGTERM")];
    for (signal, name) in SIGNALS {
        glib_unix::unix_signal_add_local(signal, move || {
            super::signalled(name);
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
