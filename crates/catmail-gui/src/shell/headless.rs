//! The shell in tests: each window's view runs on a headless nsh engine (nsh-testing's
//! [`Headless`]) that the test drives, as a user would the window, and the main thread
//! is a thread the test starts, with a loop of its own ([`Desktop`]).
//!
//! The rest of the shell runs as it does with GTK or AppKit: the windows it opens, what
//! their views ask of the main thread ([`invoke`]), closing them and quitting. What
//! only a platform has (signals, the tray, the session bus) isn't here.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use catmail_core::MailClient;
use catmail_frontend::system::resident::QuitReason;
use nsh_core::platform::{Idiom, Os};
use nsh_engine::{ColorScheme, EngineConfig, Environment, Points, Size};
use nsh_testing::Headless;

use super::{Hooks, PARTS, Parts, Spec, lock};
use crate::nsh_platform::{App, AppOptions, Appearance, Root};

/// How long the test waits for what a step started (a window, a sync, a send) to show.
pub const WAIT: Duration = Duration::from_secs(10);

/// What the main thread is asked to do, from any thread.
type Job = Box<dyn FnOnce() + Send>;

/// The queue of the [`Desktop`] running, or of the last one.
static QUEUE: Mutex<Option<mpsc::Sender<Job>>> = Mutex::new(None);

/// What [`after`] put off, by when and in order.
type Timers = BTreeMap<(Instant, u64), Box<dyn FnOnce()>>;

thread_local! {
    static WINDOWS: RefCell<Vec<Window>> = RefCell::default();
    static TIMERS: RefCell<Timers> = RefCell::default();
    static NEXT_TIMER: Cell<u64> = const { Cell::new(0) };
    static ENDED: Cell<bool> = const { Cell::new(false) };
}

/// Runs the loop on this thread until [`end`]. `started` runs first, then the app is
/// launched, as the toolkits do.
pub fn run(started: impl FnOnce() + 'static) -> u8 {
    let (queue, jobs) = mpsc::channel();
    *lock(&QUEUE) = Some(queue);
    started();
    super::launched(false);
    while !ENDED.get() {
        let next = run_due_timers();
        let job = match next {
            Some(at) => jobs.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => jobs.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match job {
            Ok(f) => f(),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    0
}

/// Runs the timers that are due: when the next is, if any is left.
fn run_due_timers() -> Option<Instant> {
    loop {
        let now = Instant::now();
        let due = TIMERS.with_borrow_mut(|timers| {
            let timer = timers.first_entry()?;
            let (at, _) = *timer.key();
            Some(if at <= now {
                Ok(timer.remove())
            } else {
                Err(at)
            })
        });
        match due? {
            Ok(f) => f(),
            Err(at) => return Some(at),
        }
    }
}

/// Runs `future` on the main thread's loop, from the main thread.
pub fn spawn(future: impl Future<Output = ()> + 'static) {
    let (runnable, task) = async_task::spawn_local(future, |runnable: async_task::Runnable| {
        invoke(move || {
            runnable.run();
        });
    });
    runnable.schedule();
    task.detach();
}

/// Runs `f` on the main thread, from any.
pub fn invoke(f: impl FnOnce() + Send + 'static) {
    // Out of the lock: a job dropped unsent may drop a waker that sends.
    let queue = lock(&QUEUE).clone();
    if let Some(queue) = queue {
        // Gone once the loop is over: as on a platform, what comes later never runs.
        let _ = queue.send(Box::new(f));
    }
}

/// Runs `f` on the main thread once `delay` is over.
pub fn after(delay: Duration, f: impl FnOnce() + 'static) {
    let n = NEXT_TIMER.replace(NEXT_TIMER.get() + 1);
    TIMERS.with_borrow_mut(|timers| timers.insert((Instant::now() + delay, n), Box::new(f)));
}

pub fn windows() -> Vec<Window> {
    WINDOWS.with_borrow(Clone::clone)
}

/// The loop ends, with whatever windows are open.
pub fn end() {
    ENDED.set(true);
}

pub fn quit_called_off() {}

pub fn resident() {}

pub fn set_appearance(_appearance: Appearance) {}

/// A window's view: the engine it runs once loaded goes to the test.
pub struct View {
    appearance: Appearance,
    title: String,
    /// Its window's, once it has one.
    size: Cell<(i32, i32)>,
    /// Until its window is destroyed.
    open: Arc<AtomicBool>,
}

pub fn new_view(options: AppOptions) -> Rc<View> {
    Rc::new(View {
        appearance: options.appearance,
        title: options.title,
        size: Cell::new((0, 0)),
        open: Arc::new(AtomicBool::new(true)),
    })
}

impl View {
    /// The settings' text size and theme aren't applied: headless, text is laid out by an
    /// estimate and nothing is drawn.
    pub fn set_font_scale(&self, _scale: Option<f32>) {}

    pub fn set_theme_overrides(&self, _json: Option<&str>) -> Result<(), std::convert::Infallible> {
        Ok(())
    }

    /// Starts `app` on an engine of its own, at its window's size, and hands it to the
    /// test ([`Desktop::window`]).
    pub fn load(&self, app: App) {
        let App::Rust(root) = app else {
            panic!("catmail's windows run Rust roots");
        };
        let (width, height) = self.size.get();
        let environment = Environment {
            color_scheme: match self.appearance {
                Appearance::Dark => ColorScheme::Dark,
                _ => ColorScheme::Light,
            },
            // Dialogs show at once, not sliding in.
            reduce_motion: true,
            idiom: Idiom::Desktop,
            // Not the default, a phone's.
            os: if cfg!(target_os = "macos") {
                Os::MacOs
            } else {
                Os::Linux
            },
            ..Environment::default()
        };
        let config = EngineConfig {
            viewport: Size::new(Points(width as f32), Points(height as f32)),
            environment,
            // The shell's own windows: without a backend for windows, nsh-testing takes
            // a desktop root for the invisible app host and shows nothing of it.
            windows: Some(Arc::new(ShellWindows)),
            ..EngineConfig::new(root)
        };
        let app = Headless::start(config).expect("a headless engine starts");
        let opened = TestWindow {
            title: self.title.clone(),
            app,
            open: self.open.clone(),
        };
        let (opened_windows, arrived) = &OPENED;
        lock(opened_windows).push(opened);
        arrived.notify_all();
    }
}

/// The windows nsh would open, as the shell's are: each view's root is its window, as
/// in GTK's and AppKit's `AppView`, and catmail declares no nsh windows of its own.
struct ShellWindows;

impl nsh_core::desktop::WindowBackend for ShellWindows {
    fn command(&self, _window: nsh_core::NodeId, _command: &nsh_core::desktop::DesktopCommand) {}
}

/// A window, which the list of open windows keeps until it is destroyed.
#[derive(Clone)]
pub struct Window(Rc<Inner>);

struct Inner {
    view: Rc<View>,
    hooks: RefCell<Option<Hooks>>,
}

impl Window {
    pub fn open(spec: &Spec, view: &Rc<View>, hooks: Hooks) -> Self {
        view.size.set(spec.size);
        let window = Self(Rc::new(Inner {
            view: view.clone(),
            hooks: RefCell::new(Some(hooks)),
        }));
        WINDOWS.with_borrow_mut(|windows| windows.push(window.clone()));
        window
    }

    pub fn present(&self, _activation_token: Option<&str>) {}

    /// Closes the window as the user would: [`Hooks::close_requested`] may keep it.
    pub fn close(&self) {
        let kept = self
            .0
            .hooks
            .borrow()
            .as_ref()
            .and_then(|hooks| hooks.close_requested.as_ref())
            .is_some_and(|close_requested| close_requested());
        if !kept {
            self.destroy();
        }
    }

    /// Closes the window, without asking. Its engine runs on until the test lets go of
    /// it, but the test sees it closed ([`TestWindow::is_open`]).
    pub fn destroy(&self) {
        let Some(hooks) = self.0.hooks.borrow_mut().take() else {
            return;
        };
        self.0.view.open.store(false, Ordering::SeqCst);
        WINDOWS.with_borrow_mut(|windows| windows.retain(|w| !Rc::ptr_eq(&w.0, &self.0)));
        (hooks.gone)();
        super::window_removed();
    }
}

/// The windows opened and not yet taken by the test, in order.
static OPENED: (Mutex<Vec<TestWindow>>, Condvar) = (Mutex::new(Vec::new()), Condvar::new());

/// A window the app opened: its view, for the test to drive.
pub struct TestWindow {
    title: String,
    pub app: Headless,
    open: Arc<AtomicBool>,
}

impl TestWindow {
    /// Until the app closes it.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    /// Waits for the app to close it.
    pub fn wait_closed(&self) {
        let closed = poll_until(WAIT, || !self.is_open());
        assert!(closed, "{:?} is still open", self.title);
    }
}

/// Whether `done` holds within `timeout`, checked every few milliseconds: for what has no
/// signal to wait on.
pub fn poll_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + timeout;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Tests take turns: the engine, the settings and what is asked of the mailbox are the
/// process's, as in the app.
static TURN: Mutex<()> = Mutex::new(());

/// A test's turn, from before it sets up what its app runs on (servers, a client) until
/// it lets go.
pub struct Turn {
    _held: std::sync::MutexGuard<'static, ()>,
}

impl Turn {
    pub fn take() -> Self {
        Self { _held: lock(&TURN) }
    }
}

/// The app with `client` for its engine, its main thread running: as launched, its main
/// window opens ([`Desktop::window`]). Dropping it quits, and shuts the engine down.
pub struct Desktop {
    main: Option<JoinHandle<()>>,
}

impl Desktop {
    /// Starts the app, with `main_window` the main window's view, on the test's turn.
    pub fn start(client: MailClient, main_window: fn() -> Root, _turn: &Turn) -> Self {
        lock(&OPENED.0).clear();
        super::reset();
        let main = std::thread::Builder::new()
            .name("catmail-main".into())
            .spawn(move || {
                PARTS.set(Some(Parts {
                    main_window,
                    launching: || {},
                    engine_opened: || {},
                }));
                run(move || crate::engine::opened_with(Ok(client)));
                super::finish();
            })
            .expect("the main thread starts");
        Self { main: Some(main) }
    }

    /// The next window titled `title` the app opens, once it has.
    pub fn window(&self, title: &str) -> TestWindow {
        let (opened, arrived) = &OPENED;
        let (mut windows, _) = arrived
            .wait_timeout_while(lock(opened), WAIT, |windows| {
                !windows.iter().any(|w| w.title == title)
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(i) = windows.iter().position(|w| w.title == title) else {
            let opened: Vec<_> = windows.iter().map(|w| &w.title).collect();
            panic!("no window {title:?} opened; opened: {opened:?}");
        };
        windows.remove(i)
    }

    /// Runs `f` on the main thread and waits for it.
    pub fn on_main<R: Send + 'static>(&self, f: impl FnOnce() -> R + Send + 'static) -> R {
        pollster::block_on(super::on_main(f)).expect("the main thread runs")
    }

    /// Closes the window titled `title` as the user would, with its close button.
    pub fn close(&self, title: &str) {
        let title = title.to_owned();
        self.on_main(move || {
            let window = windows().into_iter().find(|w| w.0.view.title == title);
            if let Some(window) = window {
                window.close();
            }
        });
    }

    /// Quits as Ctrl+Q does.
    pub fn quit(&self) {
        invoke(|| super::quit(QuitReason::Shortcut));
    }

    /// Waits for the app to have quit, and its engine to have shut down.
    pub fn wait_quit(&self) {
        let quit = poll_until(WAIT, || {
            self.main.as_ref().is_none_or(JoinHandle::is_finished)
        });
        assert!(quit, "the app didn't quit");
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        let Some(main) = self.main.take() else {
            return;
        };
        invoke(end);
        // A test that failed may have left the app stuck: the next goes on all the same.
        poll_until(Duration::from_secs(5), || main.is_finished());
        if main.is_finished() && main.join().is_err() && !std::thread::panicking() {
            panic!("the main thread panicked");
        }
    }
}
