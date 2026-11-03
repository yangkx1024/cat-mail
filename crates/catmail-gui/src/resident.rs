//! What outlives the windows: the engine, and the main window coming and going. With
//! "Keep running after the window is closed" on, closing the last window leaves the app
//! running, disconnected from the display server, until another launch asks for the
//! window again. See docs/architecture/background.md.

use std::fmt;
use std::time::Duration;

use catmail_core::MailClient;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, FutureExt as _, Global,
    GraphicalEnvironment, SHUTDOWN_TIMEOUT, SharedString, Subscription, Task, WeakEntity, Window,
    WindowId, WindowingRequest,
};

use crate::MainWindow;
use crate::instance::{Display, Requests};
use crate::mailbox::{Destination, Mailbox};
use crate::new_mail::Notifier;
use crate::settings::Preferences;
use crate::tray::Tray;

/// Whether the app drops its display connection while no window is open. macOS apps keep
/// their Dock icon instead, and get the window back through it (`on_reopen`).
const GOES_HEADLESS: bool = !cfg!(target_os = "macos");

pub enum Engine {
    Opening,
    Open(MailClient),
    /// The data directory couldn't be opened, typically because the CLI is using it.
    Failed(SharedString),
    /// The app is quitting and has shut it down.
    Closed,
}

/// Why the app quits. Said in the log, where an app that was killed or crashed just stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitReason {
    /// The Quit action (Ctrl Q).
    Shortcut,
    Tray,
    /// SIGINT, SIGTERM or SIGHUP; on Windows, a console event.
    Signal,
    LastWindowClosed,
    /// The data directory couldn't be opened, and no window is left to say so.
    EngineFailed,
    /// The main window couldn't be opened at launch.
    NoWindow,
    /// Not by the app: the platform's own Quit (macOS's app menu or Dock).
    Platform,
}

impl fmt::Display for QuitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Shortcut => "shortcut",
            Self::Tray => "tray",
            Self::Signal => "signal",
            Self::LastWindowClosed => "last_window_closed",
            Self::EngineFailed => "engine_failed",
            Self::NoWindow => "no_window",
            Self::Platform => "platform",
        })
    }
}

/// Quits now, for `reason`, which [`Resident::shut_down`] logs.
pub fn exit(reason: QuitReason, cx: &mut App) {
    cx.global_mut::<Resident>().quitting = Some(reason);
    cx.quit();
}

/// How long quitting waits for the engine to close, within the time GPUI gives it.
const ENGINE_CLOSING: Duration = SHUTDOWN_TIMEOUT.saturating_sub(Duration::from_millis(50));

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
        Engine::Failed(_) | Engine::Closed => AfterLastWindow::Quit,
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
    /// Quit was asked for, and why: the composer windows are closing, or the app is
    /// quitting already.
    quitting: Option<QuitReason>,
    /// Where the main window was when it closed, to open it there again; or what a
    /// notification clicked meanwhile was about.
    reopen: Option<Destination>,
    /// A new message was asked for from the tray while the main window was closed: it
    /// opens with a composer.
    compose: bool,
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
        cx.set_global(Self::new(Engine::Opening, tasks));
        cx.on_window_closed(Self::window_closed).detach();
        cx.on_action(|_: &crate::actions::Quit, cx| Self::quit(QuitReason::Shortcut, cx));
        cx.on_app_quit(Self::shut_down).detach();
        cx.on_system_wake(Self::woke).detach();
        Self::open_main(cx)
    }

    fn new(engine: Engine, tasks: Vec<Task<()>>) -> Self {
        Self {
            engine,
            main: None,
            windowing: Windowing::Windowed,
            quitting: None,
            reopen: None,
            compose: false,
            display: Display::current(),
            mailbox: WeakEntity::new_invalid(),
            _mailbox_released: None,
            _switch: Task::ready(()),
            _quit: Task::ready(()),
            _tasks: tasks,
        }
    }

    /// Only the engine, open, without the window or what starts with it: for tests of
    /// those that take it from here.
    #[cfg(test)]
    pub fn hold(client: MailClient, cx: &mut App) {
        cx.set_global(Self::new(Engine::Open(client), Vec::new()));
    }

    /// Quits on SIGINT, SIGTERM or SIGHUP (on Windows, the console's events) as the tray's
    /// Quit does, so what the composers hold is saved: ending the session sends SIGTERM. A
    /// second one quits at once, without waiting for a composer that can't close. Only
    /// once per process.
    pub fn quit_on_signals(cx: &mut App) {
        let (signalled, mut signals) = futures_channel::mpsc::unbounded();
        let handled = ctrlc::set_handler(move || {
            let _ = signalled.unbounded_send(());
        });
        if let Err(e) = handled {
            tracing::warn!(error = %e, "can't handle signals; they end the app at once");
            return;
        }
        cx.spawn(async move |cx| {
            if signals.recv().await.is_ok() {
                cx.update(|cx| Self::quit(QuitReason::Signal, cx));
            }
            if signals.recv().await.is_ok() {
                cx.update(|cx| exit(QuitReason::Signal, cx));
            }
        })
        .detach();
    }

    /// However the app quits: says why, where the log of an app that was killed or
    /// crashed just stops. GPUI quits without dropping the app's state, so this shuts the
    /// engine down, whoever still holds it. Waits for it to close within the time GPUI
    /// gives, so the log says whether it did.
    fn shut_down(cx: &mut App) -> impl Future<Output = ()> + use<> {
        let resident = cx.global_mut::<Self>();
        let reason = resident.quitting.unwrap_or(QuitReason::Platform);
        tracing::info!(%reason, "quitting");
        let engine = std::mem::replace(&mut resident.engine, Engine::Closed);
        let closed = match engine {
            Engine::Open(client) => {
                client.shut_down();
                Some(client.closed())
            }
            Engine::Opening | Engine::Failed(_) | Engine::Closed => None,
        };
        let executor = cx.background_executor().clone();
        async move {
            let Some(closed) = closed else {
                return;
            };
            if closed
                .with_timeout(ENGINE_CLOSING, &executor)
                .await
                .is_err()
            {
                // Blocking work under way, such as a write to the database, still holds it.
                tracing::warn!("the engine is still open; quitting anyway");
            }
        }
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

    /// The engine, while it is open. What lives with the app takes it from here each time
    /// rather than keeping a handle of its own: there is one engine state, `Resident`'s.
    pub fn client(cx: &App) -> Option<MailClient> {
        match Self::engine(cx) {
            Engine::Open(client) => Some(client.clone()),
            Engine::Opening | Engine::Failed(_) | Engine::Closed => None,
        }
    }

    /// Opens a new mailbox where the last one was, or where a notification or the tray
    /// asked for meanwhile. Remembers where this one is when it goes.
    pub fn adopt(mailbox: &Entity<Mailbox>, window: &mut Window, cx: &mut App) {
        let released = cx.observe_release(mailbox, |mailbox, cx| {
            cx.global_mut::<Self>().reopen = mailbox.place.map(Destination::Place);
        });
        let resident = cx.global_mut::<Self>();
        resident.mailbox = mailbox.downgrade();
        resident._mailbox_released = Some(released);
        let reopen = resident.reopen.take();
        let compose = std::mem::take(&mut resident.compose);
        // Both wait for the folders, which a new mailbox hasn't read yet.
        mailbox.update(cx, |mailbox, cx| {
            if let Some(to) = reopen {
                mailbox.go_to(to, window, cx);
            }
            if compose {
                mailbox.new_message(window, cx);
            }
        });
    }

    /// Shows the main window on the display it was last on, taken to `to` (now, or once
    /// it opens). `activation_token` is what the notification or tray clicked gave, if
    /// anything, to take focus on Wayland.
    pub fn show_at(to: Option<Destination>, activation_token: Option<String>, cx: &mut App) {
        if let Some(to) = to
            && !Self::in_main(cx, |mailbox, window, cx| mailbox.go_to(to, window, cx))
        {
            cx.global_mut::<Self>().reopen = Some(to);
        }
        Self::show_again(activation_token, cx);
    }

    /// Shows the main window with a composer for a new message, opening it if needed.
    pub fn compose(activation_token: Option<String>, cx: &mut App) {
        if !Self::in_main(cx, Mailbox::new_message) {
            cx.global_mut::<Self>().compose = true;
        }
        Self::show_again(activation_token, cx);
    }

    /// Syncs every account now, whether the main window is open or not: the engine's
    /// background sync does it, and progress and failures show as the accounts' status.
    pub fn sync(cx: &mut App) {
        let Some(client) = Self::client(cx) else {
            return;
        };
        cx.background_spawn(async move {
            if let Err(e) = client.sync_now().await {
                tracing::warn!(error = %e, "couldn't sync");
            }
        })
        .detach();
    }

    /// Runs `f` on the main window's mailbox. Returns whether it is there to run it.
    fn in_main(
        cx: &mut App,
        f: impl FnOnce(&mut Mailbox, &mut Window, &mut Context<Mailbox>),
    ) -> bool {
        let resident = cx.global::<Self>();
        let (Some(main), Some(mailbox)) = (resident.main, resident.mailbox.upgrade()) else {
            return false;
        };
        main.update(cx, |_, window, cx| {
            mailbox.update(cx, |m, cx| f(m, window, cx))
        })
        .is_ok()
    }

    /// Shows the main window on the display it was last on.
    fn show_again(activation_token: Option<String>, cx: &mut App) {
        // The one it was last shown with is used up.
        let display = Display {
            activation_token,
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
        let open = matches!(engine, Engine::Open(_));
        let failed = matches!(engine, Engine::Failed(_));
        cx.global_mut::<Self>().engine = engine;
        // Both take the engine from here.
        if open {
            Notifier::start(cx);
            Tray::start(cx);
        }
        // The window was closed while it opened: there is nothing to keep running.
        if failed && cx.windows().is_empty() {
            exit(QuitReason::EngineFailed, cx);
        }
    }

    /// Quits once every composer window has saved what it holds and closed. One that
    /// can't close (see [`composer::close_all`](crate::composer::close_all)) stays open,
    /// and so does the app.
    pub fn quit(reason: QuitReason, cx: &mut App) {
        if cx.global::<Self>().quitting.is_some() {
            return;
        }
        let closed = crate::composer::close_all(cx);
        let quit = cx.spawn(async move |cx| {
            // Dropped unanswered: nothing says they closed, so they may still be open.
            let closed = closed.await.unwrap_or(false);
            cx.update(|cx| {
                if closed {
                    exit(reason, cx);
                } else {
                    tracing::info!(%reason, "quit called off: a composer stayed open");
                    cx.global_mut::<Self>().quitting = None;
                }
            });
        });
        let resident = cx.global_mut::<Self>();
        resident.quitting = Some(reason);
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
                match &switched {
                    Ok(()) => tracing::info!("connected to the display again"),
                    Err(e) => tracing::error!(error = %e, "can't connect to the display"),
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
        let (window, _) =
            crate::layout::open_window(crate::window_options(cx), cx, |window, cx| {
                cx.new(|cx| MainWindow::new(window, cx))
            })?;
        tracing::info!("main window opened");
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
            tracing::info!("main window closed");
            cx.global_mut::<Self>().main = None;
        }
        if !cx.windows().is_empty() {
            return;
        }
        let keep_running = Preferences::get(cx).keep_running;
        let resident = cx.global::<Self>();
        let quitting = resident.quitting;
        match after_last_window(
            keep_running,
            &resident.engine,
            quitting.is_some(),
            GOES_HEADLESS,
        ) {
            AfterLastWindow::Quit => exit(quitting.unwrap_or(QuitReason::LastWindowClosed), cx),
            AfterLastWindow::Stay => {}
            AfterLastWindow::GoHeadless => {
                Self::switch(WindowingRequest::Headless, cx, |switched, pending, cx| {
                    cx.global_mut::<Self>().windowing = match switched {
                        Ok(()) => {
                            tracing::info!("no window open; display connection dropped");
                            // Nothing of the windows is left: what they used is free.
                            cx.background_spawn(async { crate::memory::release_freed() })
                                .detach();
                            Windowing::Headless
                        }
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
    use crate::tray::Command;

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
        clicking_the_notification_opens_it(Place::Folder(mail.folder_id("INBOX")), cx);
    }

    #[gpui_kit::test]
    fn a_notification_clicked_with_the_window_behind_opens_the_message_there(
        cx: &mut TestAppContext,
    ) {
        let mail = start(None, cx);
        main_mailbox(cx);
        mail.deliver_and_sync("Lunch?");
        clicking_the_notification_opens_it(Place::Folder(mail.folder_id("INBOX")), cx);
    }

    #[gpui_kit::test]
    fn a_notification_clicked_with_all_inboxes_shown_opens_the_message_in_its_inbox(
        cx: &mut TestAppContext,
    ) {
        let mail = TestMail::start();
        mail.add_another_account("bob@example.com");
        let mail = start_with(mail, None, cx);
        let (_, mailbox) = main_mailbox(cx);
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| mailbox.read(cx).place == Some(Place::Inboxes))
        });
        mail.deliver_and_sync("Lunch?");
        // Not in All Inboxes, where other accounts' newer mail may keep it off the page.
        clicking_the_notification_opens_it(Place::Folder(mail.folder_id("INBOX")), cx);
    }

    /// The app with its main window and the engine `TestMail::start` has, taking
    /// `requests` from later launches.
    fn start(requests: Option<Requests>, cx: &mut TestAppContext) -> TestMail {
        start_with(TestMail::start(), requests, cx)
    }

    fn start_with(mail: TestMail, requests: Option<Requests>, cx: &mut TestAppContext) -> TestMail {
        cx.executor().allow_parking();
        let client = mail.client.clone();
        cx.update(|cx| {
            crate::init_app(Preferences::load_nowhere(), cx);
            Resident::start(Task::ready(Ok(client)), requests, cx).unwrap();
        });
        mail
    }

    /// The one notification, for "Lunch?", shows that message in `place` once clicked.
    fn clicking_the_notification_opens_it(place: Place, cx: &mut TestAppContext) {
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
        assert_eq!(cx.read(|cx| mailbox.read(cx).place), Some(place));
    }

    #[gpui_kit::test]
    fn new_mail_is_notified_while_the_main_window_is_in_front(cx: &mut TestAppContext) {
        let mail = start(None, cx);
        let (window, _) = main_mailbox(cx);
        cx.update_window(window, |_, window, _| window.activate_window())
            .unwrap();
        cx.run_until_parked();
        assert!(
            cx.update_window(window, |_, window, _| window.is_window_active())
                .unwrap()
        );

        mail.deliver_and_sync("Lunch?");
        clicking_the_notification_opens_it(Place::Folder(mail.folder_id("INBOX")), cx);
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

        // From the tray, as from the key.
        cx.update(|cx| Tray::choose(Command::Quit, None, cx));
        assert_eq!(
            cx.read(|cx| cx.global::<Resident>().quitting),
            Some(QuitReason::Tray)
        );
        cx.run_until_parked();
        assert_eq!(cx.read(|cx| cx.global::<Resident>().quitting), None);
        assert_eq!(cx.read(|cx| cx.windows().len()), 2);
    }

    #[gpui_kit::test]
    fn quitting_closes_the_engine_whoever_holds_it(cx: &mut TestAppContext) {
        let mail = start(None, cx);
        main_mailbox(cx);
        // A sync under way, held at the server, and a handle kept here.
        mail.deliver("INBOX", "Lunch?");
        let _gate = mail.hold_fetches();
        let fetches = mail.header_fetches();
        let (client, account) = (mail.client.clone(), mail.account());
        // On a thread of its own: blocking on it here would stop the test's executor.
        let syncing = std::thread::spawn(move || pollster::block_on(client.sync_account(account)));
        wait_until(|| mail.header_fetches() > fetches);
        let mut closed = Box::pin(mail.client.closed());

        cx.update(|cx| Resident::quit(QuitReason::Shortcut, cx));
        cx.run_until_parked();
        // What the platform does once asked to quit.
        cx.quit();
        let mut noop = std::task::Context::from_waker(std::task::Waker::noop());
        wait_until(|| {
            cx.run_until_parked();
            closed.as_mut().poll(&mut noop).is_ready()
        });
        let synced = syncing.join().unwrap();
        assert!(
            matches!(synced, Err(catmail_core::Error::Closed)),
            "{synced:?}"
        );
    }

    /// Closes the main window, as the user does.
    fn close_main(cx: &mut TestAppContext) {
        let (window, mailbox) = main_mailbox(cx);
        drop(mailbox);
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        assert!(cx.read(|cx| cx.windows().is_empty()));
    }

    #[gpui_kit::test]
    fn the_tray_counts_the_unread_mail_in_the_inboxes(cx: &mut TestAppContext) {
        let mail = start(None, cx);
        let unread = pollster::block_on(mail.client.count_unread_in_inboxes()).unwrap();
        let counted = |count, cx: &mut TestAppContext| {
            wait_until(|| {
                cx.run_until_parked();
                cx.read(Tray::unread) == Some(count)
            })
        };
        counted(unread, cx);
        close_main(cx);
        mail.deliver_and_sync("Lunch?");
        counted(unread + 1, cx);
    }

    #[gpui_kit::test]
    fn the_tray_brings_the_closed_window_back(cx: &mut TestAppContext) {
        let _mail = start(None, cx);
        close_main(cx);
        cx.update(|cx| Tray::choose(Command::Open, None, cx));
        main_mailbox(cx);
    }

    #[gpui_kit::test]
    fn a_new_message_from_the_tray_opens_the_closed_window_with_a_composer(
        cx: &mut TestAppContext,
    ) {
        let _mail = start(None, cx);
        close_main(cx);
        cx.update(|cx| Tray::choose(Command::Compose, None, cx));
        let (main, _) = main_mailbox(cx);
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| cx.windows().len() == 2)
        });
        assert_eq!(cx.read(crate::composer::texts).len(), 1);
        assert!(cx.read(|cx| cx.windows().contains(&main)));
    }

    #[gpui_kit::test]
    fn syncing_from_the_tray_shows_in_the_window(cx: &mut TestAppContext) {
        let mail = start_with(TestMail::start_syncing_in_background(), None, cx);
        let (_, mailbox) = main_mailbox(cx);
        // Not INBOX, which background sync watches on its own.
        mail.deliver("Archive", "Lunch?");
        let gate = mail.hold_fetches();
        cx.update(|cx| Tray::choose(Command::Sync, None, cx));
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| mailbox.read(cx).syncing())
        });
        gate.add_permits(tokio::sync::Semaphore::MAX_PERMITS);
        wait_until(|| {
            cx.run_until_parked();
            cx.read(|cx| !mailbox.read(cx).syncing())
        });
        assert!(
            mail.stored_subjects("Archive")
                .contains(&"Lunch?".to_owned())
        );
    }

    #[gpui_kit::test]
    fn syncing_from_the_tray_works_with_the_window_closed(cx: &mut TestAppContext) {
        let mail = start_with(TestMail::start_syncing_in_background(), None, cx);
        close_main(cx);
        mail.deliver("Archive", "Lunch?");
        cx.update(|cx| Tray::choose(Command::Sync, None, cx));
        wait_until(|| {
            cx.run_until_parked();
            mail.stored_subjects("Archive")
                .contains(&"Lunch?".to_owned())
        });
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
