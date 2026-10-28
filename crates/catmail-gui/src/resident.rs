//! What outlives the windows: the engine, and the main window coming and going. With
//! "Keep running after the window is closed" on, closing the last window leaves the app
//! running, disconnected from the display server, until another launch asks for the
//! window again. See docs/architecture/background.md.

use catmail_core::MailClient;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Entity, Global, GraphicalEnvironment, SharedString,
    Subscription, Task, WeakEntity, WindowId, WindowingRequest,
};

use crate::MainWindow;
use crate::instance::{Display, Requests};
use crate::mailbox::{Destination, Mailbox};
use crate::new_mail::Notifier;
use crate::settings::Preferences;

/// Whether the app drops its display connection while no window is open. macOS apps keep
/// their Dock icon instead, and get the window back through it (`on_reopen`).
const GOES_HEADLESS: bool = !cfg!(target_os = "macos");

pub enum Engine {
    Opening,
    Open(MailClient),
    /// The data directory couldn't be opened, typically because the CLI is using it.
    Failed(SharedString),
}

#[derive(Debug, PartialEq)]
enum Windowing {
    Windowed,
    /// A switch between the two is under way; windows can't open meanwhile. Holds the
    /// display a launch asked for the window on meanwhile, shown once it is done.
    Switching(Option<Display>),
    Headless,
}

/// What the app does once its last window has closed.
#[derive(Debug, PartialEq, Eq)]
enum AfterLastWindow {
    Quit,
    /// Keep running, connected to the display.
    Stay,
    GoHeadless,
}

fn after_last_window(
    keep_running: bool,
    engine: &Engine,
    quitting: bool,
    goes_headless: bool,
) -> AfterLastWindow {
    match engine {
        _ if quitting || !keep_running => AfterLastWindow::Quit,
        // Nothing would run.
        Engine::Failed(_) => AfterLastWindow::Quit,
        Engine::Opening | Engine::Open(_) if goes_headless => AfterLastWindow::GoHeadless,
        Engine::Opening | Engine::Open(_) => AfterLastWindow::Stay,
    }
}

/// What to show once reconnecting to `display` is done: the window there; or if that
/// failed, the display another launch asked for meanwhile, which was told yes and may
/// work where this one didn't.
fn after_reconnecting(
    connected: bool,
    display: Display,
    pending: Option<Display>,
) -> Option<Display> {
    if connected { Some(display) } else { pending }
}

pub struct Resident {
    engine: Engine,
    main: Option<AnyWindowHandle>,
    windowing: Windowing,
    /// Quit was asked for, and the composer windows are closing.
    quitting: bool,
    /// Where the main window was when it closed, to open it there again; or what a
    /// notification clicked meanwhile was about.
    reopen: Option<Destination>,
    /// The display the windows were last on, to show the main window there again when
    /// nothing else asks for it (a notification is clicked).
    display: Display,
    /// The main window's, once the engine is in it.
    mailbox: WeakEntity<Mailbox>,
    _mailbox_released: Option<Subscription>,
    _switch: Task<()>,
    _quit: Task<()>,
    _tasks: Vec<Task<()>>,
}

impl Global for Resident {}

impl Resident {
    /// Opens the main window, and puts the engine in it once `opening` is done. Later
    /// launches' `requests` show the window again.
    pub fn start(
        opening: Task<catmail_core::Result<MailClient>>,
        requests: Option<Requests>,
        cx: &mut App,
    ) -> anyhow::Result<()> {
        let mut tasks = vec![cx.spawn(async move |cx| {
            let opened = opening.await;
            cx.update(|cx| Self::opened(opened, cx));
        })];
        if let Some(mut requests) = requests {
            tasks.push(cx.spawn(async move |cx| {
                while let Ok(request) = requests.recv().await {
                    match request {
                        crate::instance::Request::Activate { display } => {
                            cx.update(|cx| Self::show(display, cx));
                        }
                    }
                }
            }));
        }
        cx.set_global(Self {
            engine: Engine::Opening,
            main: None,
            windowing: Windowing::Windowed,
            quitting: false,
            reopen: None,
            display: Display::current(),
            mailbox: WeakEntity::new_invalid(),
            _mailbox_released: None,
            _switch: Task::ready(()),
            _quit: Task::ready(()),
            _tasks: tasks,
        });
        cx.on_window_closed(Self::window_closed).detach();
        cx.on_action(|_: &crate::actions::Quit, cx| Self::quit(cx));
        cx.on_system_wake(Self::woke).detach();
        Self::open_main(cx)
    }

    /// The engine's connections are likely dead after a sleep, and its timers didn't
    /// count it: it catches up at once rather than minutes later.
    fn woke(cx: &mut App) {
        if let Engine::Open(client) = Self::engine(cx) {
            client.resume();
        }
    }

    pub fn engine(cx: &App) -> &Engine {
        &cx.global::<Self>().engine
    }

    /// Where a new mailbox opens: where the last one was, or what a notification was
    /// about. Remembers where this one is when it goes.
    pub fn adopt(mailbox: &Entity<Mailbox>, cx: &mut App) -> Option<Destination> {
        let released = cx.observe_release(mailbox, |mailbox, cx| {
            cx.global_mut::<Self>().reopen = mailbox.place.map(Destination::Place);
        });
        let resident = cx.global_mut::<Self>();
        resident.mailbox = mailbox.downgrade();
        resident._mailbox_released = Some(released);
        resident.reopen.take()
    }

    /// Whether the main window is open and in front.
    pub fn main_is_active(cx: &mut App) -> bool {
        let Some(main) = cx.global::<Self>().main else {
            return false;
        };
        main.update(cx, |_, window, _| window.is_window_active())
            .unwrap_or(false)
    }

    /// Shows the main window on the display it was last on, taken to `to` (now, or once
    /// it opens).
    pub fn show_at(to: Option<Destination>, cx: &mut App) {
        let resident = cx.global_mut::<Self>();
        if let Some(to) = to {
            match (resident.main, resident.mailbox.upgrade()) {
                (Some(main), Some(mailbox)) => {
                    main.update(cx, |_, window, cx| {
                        mailbox.update(cx, |mailbox, cx| mailbox.go_to(to, window, cx))
                    })
                    .ok();
                }
                _ => resident.reopen = Some(to),
            }
        }
        // Its activation token was used.
        let display = Display {
            activation_token: None,
            ..cx.global::<Self>().display.clone()
        };
        Self::show(display, cx);
    }

    fn opened(opened: catmail_core::Result<MailClient>, cx: &mut App) {
        let engine = match opened {
            Ok(client) => Engine::Open(client),
            Err(e) => {
                tracing::error!(error = %e, "failed to open the data directory");
                Engine::Failed(e.to_string().into())
            }
        };
        if let Engine::Open(client) = &engine {
            Notifier::start(client.clone(), cx);
        }
        let failed = matches!(engine, Engine::Failed(_));
        cx.global_mut::<Self>().engine = engine;
        // The window was closed while it opened: there is nothing to keep running.
        if failed && cx.windows().is_empty() {
            cx.quit();
        }
    }

    /// Quits once every composer window has saved what it holds and closed. One that
    /// can't close (see [`composer::close_all`](crate::composer::close_all)) stays open,
    /// and so does the app.
    pub fn quit(cx: &mut App) {
        if cx.global::<Self>().quitting {
            return;
        }
        let closed = crate::composer::close_all(cx);
        let quit = cx.spawn(async move |cx| {
            // Dropped unanswered: nothing says they closed, so they may still be open.
            let closed = closed.await.unwrap_or(false);
            cx.update(|cx| {
                if closed {
                    cx.quit();
                } else {
                    cx.global_mut::<Self>().quitting = false;
                }
            });
        });
        let resident = cx.global_mut::<Self>();
        resident.quitting = true;
        resident._quit = quit;
    }

    /// Shows the main window on `display`, opening it if it was closed.
    pub fn show(display: Display, cx: &mut App) {
        let resident = cx.global_mut::<Self>();
        if let Windowing::Switching(pending) = &mut resident.windowing {
            *pending = Some(display);
            return;
        }
        if resident.windowing == Windowing::Headless {
            let request = WindowingRequest::Windowed(environment(&display));
            Self::switch(request, cx, move |switched, pending, cx| {
                if let Err(e) = &switched {
                    tracing::error!(error = %e, "can't connect to the display");
                }
                let resident = cx.global_mut::<Self>();
                resident.windowing = match switched {
                    Ok(()) => Windowing::Windowed,
                    Err(_) => Windowing::Headless,
                };
                if switched.is_ok() {
                    resident.display = display.clone();
                }
                if let Some(display) = after_reconnecting(switched.is_ok(), display, pending) {
                    Self::show(display, cx);
                }
            });
            return;
        }
        match resident.main {
            // Wayland compositors may only mark it as wanting attention: GPUI can't pass
            // the asking launch's activation token to a window already open.
            Some(main) => {
                main.update(cx, |_, window, _| window.activate_window())
                    .ok();
            }
            None => {
                if let Err(e) = Self::open_main(cx) {
                    tracing::error!(error = %e, "couldn't open the main window");
                }
            }
        }
    }

    fn open_main(cx: &mut App) -> anyhow::Result<()> {
        let (window, _) = gpui_kit::open_window(crate::window_options(cx), cx, |window, cx| {
            cx.new(|cx| MainWindow::new(window, cx))
        })?;
        cx.global_mut::<Self>().main = Some(window);
        Ok(())
    }

    fn window_closed(cx: &mut App, id: WindowId) {
        // `global_mut` tells the global's observers; most windows closing are composers.
        if cx
            .global::<Self>()
            .main
            .is_some_and(|main| main.window_id() == id)
        {
            cx.global_mut::<Self>().main = None;
        }
        if !cx.windows().is_empty() {
            return;
        }
        let keep_running = Preferences::get(cx).keep_running;
        let resident = cx.global::<Self>();
        match after_last_window(
            keep_running,
            &resident.engine,
            resident.quitting,
            GOES_HEADLESS,
        ) {
            AfterLastWindow::Quit => cx.quit(),
            AfterLastWindow::Stay => {}
            AfterLastWindow::GoHeadless => {
                Self::switch(WindowingRequest::Headless, cx, |switched, pending, cx| {
                    cx.global_mut::<Self>().windowing = match switched {
                        Ok(()) => Windowing::Headless,
                        Err(e) => {
                            // Still connected, so still able to show the window.
                            tracing::warn!(error = %e, "can't drop the display connection");
                            Windowing::Windowed
                        }
                    };
                    if let Some(display) = pending {
                        Self::show(display, cx);
                    }
                })
            }
        }
    }

    /// Asks for `request`, then hands `done` the outcome and the display a launch asked for
    /// meanwhile. `done` says which mode the app is in.
    fn switch(
        request: WindowingRequest,
        cx: &mut App,
        done: impl FnOnce(anyhow::Result<()>, Option<Display>, &mut App) + 'static,
    ) {
        let switched = cx.request_windowing(request);
        let task = cx.spawn(async move |cx| {
            let switched = switched.await;
            cx.update(|cx| {
                let windowing = &mut cx.global_mut::<Self>().windowing;
                let pending = match std::mem::replace(windowing, Windowing::Windowed) {
                    Windowing::Switching(pending) => pending,
                    Windowing::Windowed | Windowing::Headless => None,
                };
                done(switched, pending, cx);
            });
        });
        let resident = cx.global_mut::<Self>();
        resident.windowing = Windowing::Switching(None);
        resident._switch = task;
    }
}

/// `display`'s session; elsewhere than Linux, this process's own.
fn environment(display: &Display) -> GraphicalEnvironment {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        GraphicalEnvironment {
            wayland_display: display.wayland_display.clone().map(Into::into),
            x11_display: display.x11_display.clone().map(Into::into),
            xdg_runtime_dir: display.xdg_runtime_dir.clone().map(Into::into),
            activation_token: display.activation_token.clone(),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        let _ = display;
        GraphicalEnvironment::detect()
    }
}

#[cfg(test)]
mod tests {
    use catmail_core::AccountId;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{SystemNotificationResponse, TestAppContext};

    use super::*;
    use crate::composer::ComposeFrom;
    use crate::instance::Request;
    use crate::mailbox::Place;
    use crate::new_mail::GATHER;
    use crate::test_support::{TestMail, wait_until};

    /// The main window's mailbox, once the engine is in it and it has loaded.
    fn main_mailbox(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Mailbox>) {
        let mut found = None;
        wait_until(|| {
            cx.run_until_parked();
            found = cx.read(|cx| {
                let resident = cx.global::<Resident>();
                let mailbox = resident.mailbox.upgrade()?;
                let loaded = mailbox.read(cx).accounts.is_some();
                loaded.then_some((resident.main?, mailbox))
            });
            found.is_some()
        });
        found.unwrap()
    }

    #[gpui_kit::test]
    fn the_main_window_closes_and_comes_back_where_it_was(cx: &mut TestAppContext) {
        let (requests, received) = futures_channel::mpsc::unbounded();
        let mail = start(Some(received), cx);
        let account: AccountId = mail.account();
        let (window, mailbox) = main_mailbox(cx);
        let drafts = Place::Drafts(account);
        cx.update_window(window, |_, window, cx| {
            mailbox.update(cx, |mailbox, cx| {
                mailbox.select_place(Some(drafts), window, cx)
            })
        })
        .unwrap();

        // Only the window holds it now, as in the app.
        let released = mailbox.downgrade();
        drop(mailbox);
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        assert!(cx.read(|cx| cx.windows().is_empty()));
        assert!(
            released.upgrade().is_none(),
            "the mailbox outlived its window"
        );
        assert!(cx.read(|cx| matches!(Resident::engine(cx), Engine::Open(_))));

        requests
            .unbounded_send(Request::Activate {
                display: Display::default(),
            })
            .unwrap();
        let (_, mailbox) = main_mailbox(cx);
        assert_eq!(cx.read(|cx| mailbox.read(cx).place), Some(drafts));
    }

    /// Lets the engine's announcements of new mail through [`GATHER`] until `done`.
    fn gather_until(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
        wait_until(|| {
            cx.executor().advance_clock(GATHER);
            cx.run_until_parked();
            done(cx)
        });
    }

    #[gpui_kit::test]
    fn new_mail_while_no_window_is_open_shows_a_notification_that_opens_it(
        cx: &mut TestAppContext,
    ) {
        let mail = start(None, cx);
        let (window, mailbox) = main_mailbox(cx);
        drop(mailbox);
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();

        mail.deliver_and_sync("Lunch?");
        clicking_the_notification_opens_it(&mail, cx);
    }

    #[gpui_kit::test]
    fn a_notification_clicked_with_the_window_behind_opens_the_message_there(
        cx: &mut TestAppContext,
    ) {
        let mail = start(None, cx);
        main_mailbox(cx);
        mail.deliver_and_sync("Lunch?");
        clicking_the_notification_opens_it(&mail, cx);
    }

    /// The app with its main window and the engine `TestMail::start` has, taking
    /// `requests` from later launches.
    fn start(requests: Option<Requests>, cx: &mut TestAppContext) -> TestMail {
        let mail = TestMail::start();
        cx.executor().allow_parking();
        let client = mail.client.clone();
        cx.update(|cx| {
            crate::init_app(Preferences::load_nowhere(), cx);
            Resident::start(Task::ready(Ok(client)), requests, cx).unwrap();
        });
        mail
    }

    /// The one notification, for "Lunch?", shows that message in INBOX once clicked.
    fn clicking_the_notification_opens_it(mail: &TestMail, cx: &mut TestAppContext) {
        gather_until(cx, |cx| !cx.shown_system_notifications().is_empty());
        let shown = cx.shown_system_notifications();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].body.as_ref(), "Lunch?");

        cx.simulate_system_notification_response(SystemNotificationResponse {
            tag: shown[0].tag.clone(),
            action_id: None,
        });
        let (_, mailbox) = main_mailbox(cx);
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| {
                let list = mailbox.read(cx).messages.read(cx);
                let opened = list.delegate().selected_message();
                opened.and_then(|m| m.subject.clone()).as_deref() == Some("Lunch?")
            })
        });
        let inbox = pollster::block_on(mail.client.find_folder(mail.account(), "INBOX"))
            .unwrap()
            .unwrap();
        assert_eq!(
            cx.read(|cx| mailbox.read(cx).place),
            Some(Place::Folder(inbox.id))
        );
    }

    #[gpui_kit::test]
    fn new_mail_isnt_notified_while_the_main_window_is_in_front(cx: &mut TestAppContext) {
        let mail = start(None, cx);
        let (window, _) = main_mailbox(cx);
        cx.update_window(window, |_, window, _| window.activate_window())
            .unwrap();
        cx.run_until_parked();
        assert!(cx.update(Resident::main_is_active));

        mail.deliver_and_sync("Lunch?");
        wait_until(|| {
            cx.run_until_parked();
            cx.read(Notifier::is_gathering)
        });
        // It decides whether to tell as soon as the gathering is over.
        cx.executor().advance_clock(GATHER);
        cx.run_until_parked();
        assert!(!cx.read(Notifier::is_gathering));
        assert!(cx.shown_system_notifications().is_empty());
    }

    #[gpui_kit::test]
    fn a_composer_that_cant_close_calls_off_quitting(cx: &mut TestAppContext) {
        let mail = start(None, cx);
        let client = mail.client.clone();
        let (main, _) = main_mailbox(cx);
        cx.update_window(main, |_, window, cx| {
            crate::composer::open(client, ComposeFrom::New(mail.account()), window, cx)
        })
        .unwrap();
        let mut composer = None;
        wait_until(|| {
            cx.run_until_parked();
            composer = cx.update(|cx| cx.windows().into_iter().find(|w| *w != main));
            composer.is_some()
        });
        cx.update_window(composer.unwrap(), |_, window, cx| {
            window.render_frame(cx);
            window.click("composer-to", cx);
            window.input("carol@", cx);
        })
        .unwrap();

        cx.update(Resident::quit);
        assert!(cx.read(|cx| cx.global::<Resident>().quitting));
        cx.run_until_parked();
        assert!(!cx.read(|cx| cx.global::<Resident>().quitting));
        assert_eq!(cx.read(|cx| cx.windows().len()), 2);
    }

    #[test]
    fn a_launch_asking_while_reconnecting_isnt_lost_if_that_fails() {
        let display = |name: &str| Display {
            wayland_display: Some(name.into()),
            ..Display::default()
        };
        // Connected: the window opens there, which answers the launch that asked meanwhile.
        assert_eq!(
            after_reconnecting(true, display("first"), Some(display("second"))),
            Some(display("first"))
        );
        assert_eq!(
            after_reconnecting(false, display("first"), Some(display("second"))),
            Some(display("second"))
        );
        assert_eq!(after_reconnecting(false, display("first"), None), None);
    }

    #[test]
    fn closing_the_last_window_keeps_the_engine_running_if_asked() {
        let open = Engine::Opening;
        assert_eq!(
            after_last_window(true, &open, false, true),
            AfterLastWindow::GoHeadless
        );
        assert_eq!(
            after_last_window(true, &open, false, false),
            AfterLastWindow::Stay
        );
        assert_eq!(
            after_last_window(false, &open, false, true),
            AfterLastWindow::Quit
        );
    }

    #[test]
    fn quitting_or_a_failed_engine_ends_the_app_with_its_last_window() {
        assert_eq!(
            after_last_window(true, &Engine::Opening, true, true),
            AfterLastWindow::Quit
        );
        let failed = Engine::Failed("in use".into());
        assert_eq!(
            after_last_window(true, &failed, false, true),
            AfterLastWindow::Quit
        );
        assert_eq!(
            after_last_window(true, &failed, false, false),
            AfterLastWindow::Quit
        );
    }
}
