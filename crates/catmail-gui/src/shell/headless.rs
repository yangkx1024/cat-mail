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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use catmail_core::MailClient;
use catmail_frontend::resident::QuitReason;
use nsh_engine::{ColorScheme, EngineConfig, Environment, Points, Size};
use nsh_testing::Headless;

use super::{Hooks, Spec, lock};
use crate::nsh_platform::{App, AppOptions, Appearance};

/// What the main thread is asked to do, from any thread.
enum Job {
    Call(Box<dyn FnOnce() + Send>),
    Poll(async_task::Runnable),
}

/// The main thread's queue while a [`Desktop`] runs, with the run's number: a run that
/// ends late (a test that failed left it stuck) doesn't take the next one's.
static QUEUE: Mutex<Option<(u64, mpsc::Sender<Job>)>> = Mutex::new(None);

static RUNS: AtomicU64 = AtomicU64::new(0);

fn send(job: Job) {
    if let Some((_, queue)) = &*lock(&QUEUE) {
        // Gone once the loop is over: as on a platform, what comes later never runs.
        let _ = queue.send(job);
    }
}

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
    let run = RUNS.fetch_add(1, Ordering::SeqCst);
    *lock(&QUEUE) = Some((run, queue));
    started();
    super::launched(false);
    while !ENDED.get() {
        let next = run_due_timers();
        let job = match next {
            Some(at) => jobs.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => jobs.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match job {
            Ok(Job::Call(f)) => f(),
            Ok(Job::Poll(runnable)) => {
                runnable.run();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let mut queue = lock(&QUEUE);
    if queue.as_ref().is_some_and(|(of, _)| *of == run) {
        queue.take();
    }
    0
}

/// Runs the timers that are due: when the next is, if any is left.
fn run_due_timers() -> Option<Instant> {
    loop {
        let now = Instant::now();
        let due = TIMERS.with_borrow_mut(|timers| {
            let (&(at, _), _) = timers.first_key_value()?;
            Some(if at <= now {
                Ok(timers.pop_first().map(|(_, f)| f))
            } else {
                Err(at)
            })
        });
        match due {
            Some(Ok(Some(f))) => f(),
            Some(Err(at)) => return Some(at),
            Some(Ok(None)) | None => return None,
        }
    }
}

/// Runs `future` on the main thread's loop, from the main thread.
pub fn spawn(future: impl Future<Output = ()> + 'static) {
    let (runnable, task) = async_task::spawn_local(future, |runnable| send(Job::Poll(runnable)));
    runnable.schedule();
    task.detach();
}

/// Runs `f` on the main thread, from any.
pub fn invoke(f: impl FnOnce() + Send + 'static) {
    send(Job::Call(Box::new(f)));
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
        let mut environment = Environment {
            color_scheme: match self.appearance {
                Appearance::Dark => ColorScheme::Dark,
                _ => ColorScheme::Light,
            },
            // Dialogs show at once, not sliding in.
            reduce_motion: true,
            idiom: nsh_core::platform::Idiom::Desktop,
            ..Environment::default()
        };
        #[cfg(target_os = "linux")]
        {
            environment.os = nsh_core::platform::Os::Linux;
        }
        #[cfg(target_os = "macos")]
        {
            environment.os = nsh_core::platform::Os::MacOs;
        }
        let config = EngineConfig {
            viewport: Size::new(Points(width as f32), Points(height as f32)),
            environment,
            ..EngineConfig::new(root)
        };
        let app = Headless::start(config).expect("a headless engine starts");
        let opened = TestWindow {
            title: self.title.clone(),
            app,
            open: self.open.clone(),
        };
        let (opened_windows, arrived) = &*OPENED;
        lock(opened_windows).push(opened);
        arrived.notify_all();
    }
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
static OPENED: std::sync::LazyLock<(Mutex<Vec<TestWindow>>, Condvar)> =
    std::sync::LazyLock::new(Default::default);

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
    pub fn wait_closed(&self, timeout: Duration) -> Result<(), String> {
        let until = Instant::now() + timeout;
        while self.is_open() {
            if Instant::now() > until {
                return Err(format!("{:?} is still open", self.title));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}

/// Tests take turns: the engine, the settings and what is asked of the mailbox are the
/// process's, as in the app.
static TURN: Mutex<()> = Mutex::new(());

/// The app with `client` for its engine, its main thread running: as launched, its main
/// window opens ([`Desktop::window`]). Dropping it quits, and shuts the engine down.
pub struct Desktop {
    main: Option<JoinHandle<()>>,
    _turn: std::sync::MutexGuard<'static, ()>,
}

impl Desktop {
    pub fn start(client: MailClient) -> Self {
        let turn = lock(&TURN);
        lock(&OPENED.0).clear();
        super::reset();
        let main = std::thread::Builder::new()
            .name("catmail-main".into())
            .spawn(move || {
                run(move || crate::engine::opened_with(Ok(client)));
                super::finish();
            })
            .expect("the main thread starts");
        Self {
            main: Some(main),
            _turn: turn,
        }
    }

    /// The next window titled `title` the app opens, once it has.
    pub fn window(&self, title: &str) -> TestWindow {
        let timeout = Duration::from_secs(10);
        let (opened, arrived) = &*OPENED;
        let mut windows = lock(opened);
        let until = Instant::now() + timeout;
        loop {
            if let Some(i) = windows.iter().position(|w| w.title == title) {
                return windows.remove(i);
            }
            let left = until.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "no window {title:?} opened; opened: {:?}",
                windows.iter().map(|w| &w.title).collect::<Vec<_>>()
            );
            windows = arrived
                .wait_timeout(windows, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
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

    /// Waits for the app to have quit.
    pub fn wait_quit(mut self, timeout: Duration) -> Result<(), String> {
        let main = self.main.take().expect("running");
        let until = Instant::now() + timeout;
        while !main.is_finished() {
            if Instant::now() > until {
                self.main = Some(main);
                return Err("the app didn't quit".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        main.join()
            .map_err(|_| "the main thread panicked".to_owned())
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        let Some(main) = self.main.take() else {
            return;
        };
        invoke(end);
        // A test that failed may have left the app stuck: the next goes on all the same.
        let until = Instant::now() + Duration::from_secs(5);
        while !main.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        if main.is_finished() && main.join().is_err() && !std::thread::panicking() {
            panic!("the main thread panicked");
        }
    }
}
