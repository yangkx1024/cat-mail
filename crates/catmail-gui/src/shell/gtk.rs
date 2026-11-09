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
        own_app_id(app.upcast_ref());
        style_menus();
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

/// Owns the app id on the session bus, where the desktop looks for the app when one of
/// its notifications is clicked: the portal (in a Flatpak) and GNOME Shell call
/// `org.freedesktop.Application.ActivateAction` there. A `NON_UNIQUE` app exports that
/// interface and its actions at its object path all the same; it only leaves the name
/// alone. Owned while the app runs. The instance of another data directory may have it
/// already: then clicks on this one's notifications reach that one, which ignores them.
fn own_app_id(app: &gtk::gio::Application) {
    let (Some(bus), Some(id)) = (app.dbus_connection(), app.application_id()) else {
        return;
    };
    gtk::gio::bus_own_name_on_connection(
        &bus,
        &id,
        gtk::gio::BusNameOwnerFlags::DO_NOT_QUEUE,
        |_, _| {},
        |_, name| tracing::info!(name, "the app id is another instance's"),
    );
}

/// ⚠️ A context menu of one item had more room below the item than above. nsh's menus are
/// `gtk::PopoverMenu`s, whose items sit in a scrolled window that is never shorter than its
/// vertical scrollbar, shown or not: GTK's theme gives the slider 40px (44 with its
/// margins), more than one item takes (40). A shorter slider keeps the scrollbar out of
/// the menu's height. Above nsh's own menu style sheet, which has no such rule yet
/// (longbridge/nsh#1359).
const MENU_CSS: &str = "popover.menu > contents > scrolledwindow > scrollbar.vertical > range \
    > trough > slider { min-height: 16px; }";

fn style_menus() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(MENU_CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::process::Command;
    use std::rc::Rc;
    use std::time::Duration;

    use nsh_platform_linux::gtk4::{self as gtk, gio, glib, prelude::*};

    use super::own_app_id;

    const ID: &str = "net.yangkx.catmail.Test";
    const CHILD: &str = "shell::toolkit::tests::clicks_reach_the_app_through_its_id";

    /// GLib reads the session bus's address once per process: the test runs again in a
    /// process of its own, on a bus of its own.
    #[test]
    fn a_notification_click_finds_the_non_unique_app() {
        let run = Command::new("dbus-run-session")
            .arg("--")
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--ignored", "--test-threads=1"])
            .output()
            .expect("dbus-run-session");
        assert!(
            run.status.success(),
            "{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }

    #[test]
    #[ignore = "run on a bus of its own by a_notification_click_finds_the_non_unique_app"]
    fn clicks_reach_the_app_through_its_id() {
        let app = gio::Application::new(Some(ID), gio::ApplicationFlags::NON_UNIQUE);
        let clicked = Rc::new(RefCell::new(None));
        let open = gio::SimpleAction::new("open-mail", Some(glib::VariantTy::STRING));
        open.connect_activate({
            let clicked = clicked.clone();
            move |_, tag| *clicked.borrow_mut() = tag.and_then(|tag| tag.str()).map(str::to_owned)
        });
        app.add_action(&open);
        app.register(gio::Cancellable::NONE).unwrap();
        own_app_id(&app);

        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
        glib::MainContext::default().block_on(async {
            // The desktop's side, on a connection of its own.
            let desktop = gio::DBusConnection::for_address_future(
                &address,
                gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                    | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                None,
            )
            .await
            .unwrap();
            let mut tries = 0;
            loop {
                let owned = desktop
                    .call_future(
                        Some("org.freedesktop.DBus"),
                        "/org/freedesktop/DBus",
                        "org.freedesktop.DBus",
                        "NameHasOwner",
                        Some(&(ID,).to_variant()),
                        Some(glib::VariantTy::new("(b)").unwrap()),
                        gio::DBusCallFlags::NONE,
                        1000,
                    )
                    .await
                    .unwrap();
                if owned.get::<(bool,)>().unwrap().0 {
                    break;
                }
                tries += 1;
                assert!(tries < 300, "nobody owns {ID}");
                glib::timeout_future(Duration::from_millis(10)).await;
            }
            // What the portal and GNOME Shell do with a click on the notification.
            let tag = "new-mail/1/8".to_variant();
            let platform_data = HashMap::<String, glib::Variant>::new();
            desktop
                .call_future(
                    Some(ID),
                    "/net/yangkx/catmail/Test",
                    "org.freedesktop.Application",
                    "ActivateAction",
                    Some(&("open-mail", vec![tag], platform_data).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    5000,
                )
                .await
                .unwrap();
        });
        assert_eq!(clicked.borrow().as_deref(), Some("new-mail/1/8"));
    }

    /// A menu of one item is as tall as the item: its scrolled window takes its items'
    /// natural height (`propagate-natural-height`) and its scrollbar asks for no more. GTK
    /// needs a display: without one (as in CI) the test does not run.
    #[test]
    fn a_menu_of_one_item_fits_it() {
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return;
        }
        gtk::init().expect("GTK opens the display");
        // The item's height in nsh's menu style sheet, which only nsh's app loads.
        let nsh = gtk::CssProvider::new();
        nsh.load_from_string("popover.menu modelbutton { min-height: 28px; }");
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &nsh,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        super::style_menus();
        let model = gio::Menu::new();
        model.append(Some("New folder"), Some("app.new-folder"));
        let menu = gtk::PopoverMenu::from_model(Some(&model));
        let scrolled = scrolled_window(menu.upcast_ref()).expect("the menu's scrolled window");
        let (_, window, ..) = scrolled.measure(gtk::Orientation::Vertical, -1);
        let (_, items, ..) = scrolled
            .child()
            .expect("the items")
            .measure(gtk::Orientation::Vertical, -1);
        assert_eq!(window, items);
    }

    fn scrolled_window(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
        if let Some(scrolled) = widget.downcast_ref::<gtk::ScrolledWindow>() {
            return Some(scrolled.clone());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(found) = scrolled_window(&widget) {
                return Some(found);
            }
            child = widget.next_sibling();
        }
        None
    }
}
