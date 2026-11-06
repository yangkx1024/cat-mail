//! The shell on AppKit (macOS): `NSApplication`, its windows and menu bar, and the main
//! dispatch queue for the main thread's work (its futures with `async-task`). What GTK
//! has and nsh's AppKit backend hasn't is here: the press that takes the focus off a
//! text field.
//!
//! ⚠️ Not bundled: the menu bar names the app after its executable, the Dock shows no
//! icon of its own, and there are no notifications (the system's need a bundle). No tray
//! (status item) yet either: the Dock brings the window back.

// AppKit is Objective-C: its classes are defined and its messages sent with `unsafe`.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Duration;

use block2::RcBlock;
use catmail_frontend::resident::QuitReason;
use dispatch2::{DispatchQueue, DispatchTime};
use nsh_platform_macos::{AppOptions, AppView, Appearance};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{
    ClassType as _, DefinedClass as _, MainThreadMarker, MainThreadOnly, Message as _,
    define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
    NSApplicationActivationPolicy, NSApplicationDelegate, NSApplicationTerminateReply,
    NSBackingStoreType, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSMenu,
    NSMenuItem, NSText, NSTextField, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use super::{APP_NAME, Hooks, Spec};

fn main_thread() -> MainThreadMarker {
    MainThreadMarker::new().expect("the shell runs on the main thread")
}

/// Runs the application until it quits. `started` runs once, before its first window.
pub fn run(started: impl FnOnce() + 'static) -> u8 {
    let mtm = main_thread();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.setMainMenu(Some(&main_menu(mtm)));
    quit_on_signals();
    let _monitor = blur_on_press_outside();
    started();
    super::show(None);
    app.run();
    app.setDelegate(None);
    0
}

// --- The main thread's work ---------------------------------------------------------

/// Runs `future` on the main thread's loop: each wake queues a poll on the main dispatch
/// queue, which AppKit's loop drains.
pub fn spawn(future: impl Future<Output = ()> + 'static) {
    let (runnable, task) = async_task::spawn_local(future, |runnable: async_task::Runnable| {
        DispatchQueue::main().exec_async(|| {
            runnable.run();
        });
    });
    task.detach();
    runnable.schedule();
}

/// Runs `f` on the main thread, from any.
pub fn invoke(f: impl FnOnce() + Send + 'static) {
    DispatchQueue::main().exec_async(f);
}

/// Runs `f` on the main thread once `delay` is over.
pub fn after(delay: Duration, f: impl FnOnce() + Send + 'static) {
    let when = DispatchTime::try_from(delay).unwrap_or(DispatchTime::FOREVER);
    let _ = DispatchQueue::main().after(when, f);
}

/// The engine is open. Nothing follows it here yet: no tray, no notifications.
pub fn engine_opened() {}

// --- The application ----------------------------------------------------------------

define_class!(
    /// The app's answers to AppKit: it decides itself when it quits.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CatmailNshAppDelegate"]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            false
        }

        /// The app menu's Quit, the Dock's, or the session ending: the composers save
        /// first, and the loop ends rather than the process (the engine shuts down after
        /// it). ⚠️ A logout waits for nothing: it is called off, and asked again.
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            super::quit(QuitReason::Platform);
            NSApplicationTerminateReply::TerminateCancel
        }

        /// The Dock icon was clicked: the main window shows, opened if there is none.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            super::show(None);
            false
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: NSObject's `init`, on a subclass that adds nothing.
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

/// The app ends: AppKit's loop stops, and [`run`] returns, with whatever windows it has.
pub fn end() {
    let app = NSApplication::sharedApplication(main_thread());
    app.stop(None);
    // `stop:` takes effect after the next event, and this may come from the dispatch
    // queue rather than an event: one is posted, as nsh's `run_app` does.
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    );
    if let Some(event) = event {
        app.postEvent_atStart(&event, true);
    }
}

/// SIGINT, SIGTERM and SIGHUP (`ctrlc`'s handler thread), on the main thread.
fn quit_on_signals() {
    let handled = ctrlc::set_handler(|| invoke(|| super::signalled("SIGINT, SIGTERM or SIGHUP")));
    if let Err(e) = handled {
        tracing::warn!(error = %e, "signals end the app without saving");
    }
}

/// The app menu (About, Hide, Quit), Edit (the text commands fields answer: undo, cut,
/// copy, paste, select all) and Window (minimize, zoom, full screen, close), as nsh's
/// `run_app` has them.
fn main_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let bar = NSMenu::new(mtm);
    let item = |menu: &NSMenu, title: &str, action: Option<Sel>, key: &str| {
        // SAFETY: the actions are AppKit's standard ones, sent down the responder chain.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                action,
                &NSString::from_str(key),
            )
        };
        menu.addItem(&item);
        item
    };
    let submenu = |name: &str| {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(name));
        item(&bar, name, None, "").setSubmenu(Some(&menu));
        menu
    };
    let app_menu = submenu(APP_NAME);
    let about = format!("About {APP_NAME}");
    item(
        &app_menu,
        &about,
        Some(sel!(orderFrontStandardAboutPanel:)),
        "",
    );
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    item(
        &app_menu,
        &format!("Hide {APP_NAME}"),
        Some(sel!(hide:)),
        "h",
    );
    item(
        &app_menu,
        "Hide Others",
        Some(sel!(hideOtherApplications:)),
        "h",
    )
    .setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Option);
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    item(
        &app_menu,
        &format!("Quit {APP_NAME}"),
        Some(sel!(terminate:)),
        "q",
    );

    let edit = submenu("Edit");
    item(&edit, "Undo", Some(sel!(undo:)), "z");
    // An uppercase key equivalent is Shift with it.
    item(&edit, "Redo", Some(sel!(redo:)), "Z");
    edit.addItem(&NSMenuItem::separatorItem(mtm));
    item(&edit, "Cut", Some(sel!(cut:)), "x");
    item(&edit, "Copy", Some(sel!(copy:)), "c");
    item(&edit, "Paste", Some(sel!(paste:)), "v");
    item(&edit, "Select All", Some(sel!(selectAll:)), "a");

    let window = submenu("Window");
    item(&window, "Minimize", Some(sel!(performMiniaturize:)), "m");
    item(&window, "Zoom", Some(sel!(performZoom:)), "");
    item(
        &window,
        "Enter Full Screen",
        Some(sel!(toggleFullScreen:)),
        "f",
    )
    .setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Control);
    window.addItem(&NSMenuItem::separatorItem(mtm));
    item(&window, "Close", Some(sel!(performClose:)), "w");
    bar
}

// --- Windows ------------------------------------------------------------------------

pub fn new_view(options: AppOptions) -> Rc<AppView> {
    AppView::new(main_thread(), options)
}

/// An open window's view, and what to do once the window is gone.
type Open = (Rc<AppView>, Box<dyn FnOnce()>);

struct Kept {
    close_requested: Option<Box<dyn Fn() -> bool>>,
    /// Until the window is gone.
    open: RefCell<Option<Open>>,
}

impl Kept {
    /// Whether the window may close now.
    fn may_close(&self) -> bool {
        !self.close_requested.as_ref().is_some_and(|asked| asked())
    }
}

define_class!(
    /// One window's: its view, and what the app does when it closes.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CatmailNshWindowDelegate"]
    #[ivars = Kept]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        /// The close button, Cmd+W.
        #[unsafe(method(windowShouldClose:))]
        fn should_close(&self, _window: &NSWindow) -> bool {
            self.ivars().may_close()
        }

        #[unsafe(method(windowWillClose:))]
        fn will_close(&self, _notification: &NSNotification) {
            let gone = self.ivars().open.borrow_mut().take();
            WINDOWS.with_borrow_mut(|windows| {
                windows.retain(|window| !std::ptr::eq(&*window.0.delegate, self));
            });
            if let Some((view, gone)) = gone {
                view.stop();
                gone();
            }
            // Out of AppKit's close first: the last window's may end the loop.
            invoke(super::window_removed);
        }
    }
);

impl WindowDelegate {
    fn new(mtm: MainThreadMarker, kept: Kept) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(kept);
        // SAFETY: NSObject's `init`, on a subclass that adds only ivars.
        unsafe { msg_send![super(this), init] }
    }
}

struct Inner {
    window: Retained<NSWindow>,
    /// The window's delegate, which it doesn't retain.
    delegate: Retained<WindowDelegate>,
}

#[derive(Clone)]
pub struct Window(Rc<Inner>);

thread_local! {
    /// The app's windows, while they are open.
    static WINDOWS: RefCell<Vec<Window>> = RefCell::default();
}

/// The app's windows.
pub fn windows() -> Vec<Window> {
    WINDOWS.with_borrow(Clone::clone)
}

impl Window {
    /// A window showing `view`, which it keeps while it is open and stops when it is gone.
    /// Not shown yet.
    pub fn open(spec: &Spec, view: &Rc<AppView>, hooks: Hooks) -> Self {
        let mtm = main_thread();
        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(f64::from(spec.size.0), f64::from(spec.size.1)),
        );
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        // SAFETY: NSWindow's designated initializer.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: the window is kept (`Inner`) until it closes, and after.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(spec.title));
        if let Some((width, height)) = spec.min_size {
            window.setContentMinSize(NSSize::new(f64::from(width), f64::from(height)));
        }
        let delegate = WindowDelegate::new(
            mtm,
            Kept {
                close_requested: hooks.close_requested,
                open: RefCell::new(Some((view.clone(), hooks.gone))),
            },
        );
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let content = view.view();
        window.setContentView(Some(&content));
        // Keys go to the view's root while no field has the focus.
        if let Some(root) = root_of(&content) {
            window.makeFirstResponder(Some(&root));
        }
        window.center();
        let window = Self(Rc::new(Inner { window, delegate }));
        WINDOWS.with_borrow_mut(|windows| windows.push(window.clone()));
        window
    }

    /// Shows the window in front, and the app with it (a menu bar click leaves it behind
    /// the others). Activation tokens are Wayland's.
    pub fn present(&self, _activation_token: Option<&str>) {
        let window = &self.0.window;
        if window.isMiniaturized() {
            window.deminiaturize(None);
        }
        window.makeKeyAndOrderFront(None);
        // `activate` is macOS 14's.
        #[allow(deprecated)]
        NSApplication::sharedApplication(main_thread()).activateIgnoringOtherApps(true);
    }

    /// Closes the window as the user would: [`Hooks::close_requested`] may keep it.
    pub fn close(&self) {
        if self.0.delegate.ivars().may_close() {
            self.0.window.close();
        }
    }

    /// Closes the window, without asking.
    pub fn destroy(&self) {
        self.0.window.close();
    }
}

/// Gives the app's windows (their title bars) and panels the app's appearance: each view
/// sets its own.
pub fn set_appearance(appearance: Appearance) {
    // SAFETY: immutable AppKit constants.
    let name = match appearance {
        Appearance::Light => Some(unsafe { NSAppearanceNameAqua }),
        Appearance::Dark => Some(unsafe { NSAppearanceNameDarkAqua }),
        Appearance::System => None,
    };
    let appearance = name.and_then(NSAppearance::appearanceNamed);
    NSApplication::sharedApplication(main_thread()).setAppearance(appearance.as_deref());
}

/// The nsh surface's root view in an `AppView`'s view: its first subview (later ones are
/// development overlays), which handles the keys while no field has the focus.
fn root_of(content: &NSView) -> Option<Retained<NSView>> {
    content.subviews().firstObject()
}

// --- Taking the focus off a field ---------------------------------------------------

/// A press outside the text field that has the focus takes the focus off it, as in a
/// browser and as on Linux (`gtk.rs`): nsh leaves it in the field, as desktops do, and
/// the keys that act on messages (E, X, L) wouldn't reach the view. The view's root
/// takes it, as when nsh blurs a field. Watches the app's presses for as long as the
/// monitor returned is kept.
fn blur_on_press_outside() -> Option<Retained<AnyObject>> {
    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes the event it is about to send.
        blur_outside(unsafe { event.as_ref() });
        event.as_ptr()
    });
    let mask = NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown;
    // SAFETY: the handler returns the event it was given, which AppKit then sends on.
    unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &handler) }
}

fn blur_outside(event: &NSEvent) {
    let mtm = main_thread();
    let Some(window) = event.window(mtm) else {
        return;
    };
    let Some(focus) = window.firstResponder() else {
        return;
    };
    let Some(focus) = focus.downcast_ref::<NSText>() else {
        return;
    };
    let Some(content) = window.contentView() else {
        return;
    };
    // A single-line field edits in the window's field editor, inside the field; a text
    // view in its scroll view: either is the field pressed.
    let field = field_of(focus);
    let at = event.locationInWindow();
    // SAFETY: the window retains its views while this runs: nothing changes them.
    let at = match unsafe { content.superview() } {
        Some(frame) => frame.convertPoint_fromView(at, None),
        None => at,
    };
    let on_it = content
        .hitTest(at)
        .is_some_and(|hit| hit.isDescendantOf(&field));
    if !on_it && let Some(root) = root_of(&content) {
        // The window takes it first: nsh's root refuses it while a field edits.
        window.makeFirstResponder(None);
        window.makeFirstResponder(Some(&root));
    }
}

/// The field `editor` edits: the text field around the field editor, the scroll view
/// around a text view.
fn field_of(editor: &NSView) -> Retained<NSView> {
    // SAFETY: the window retains its views while this runs: nothing changes them.
    let superview = |view: &NSView| unsafe { view.superview() };
    let mut view = superview(editor);
    while let Some(ancestor) = view {
        if ancestor.isKindOfClass(NSTextField::class()) {
            return ancestor;
        }
        view = superview(&ancestor);
    }
    editor
        .enclosingScrollView()
        .map(Retained::into_super)
        .unwrap_or_else(|| editor.retain())
}
