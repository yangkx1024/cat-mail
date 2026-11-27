//! Updates, from Sparkle 2: the framework the bundle carries in `Contents/Frameworks`
//! (`packaging/macos/build-dmg.sh` puts it there), loaded at run time rather than linked,
//! so a binary run on its own (`cargo run`) needs none, and has no updater.
//!
//! Sparkle reads the feed `SUFeedURL` names (Info.plist) on its own schedule, asks the
//! user before its first automatic check, and shows its own windows; "Check for Updates…"
//! in the app menu checks now. Installing quits the app as the Dock's Quit does (a quit
//! Apple Event, then `applicationShouldTerminate:`), so the composers save first, and a
//! quit called off leaves the update to be installed once the app quits.
//! `CATMAIL_UPDATE_FEED` names another feed, for trying an update out. While the app runs
//! without a window, an update a scheduled check finds is a notification first
//! ([`reminders`]). See docs/architecture/packaging.md.

mod reminders;

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
use objc2::{DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSBundle, NSObject, NSString, ns_string};

use self::reminders::Reminders;
pub use self::reminders::{REMINDER, came_back};

/// The feed to read instead of the bundle's, for trying an update out.
const FEED_VAR: &str = "CATMAIL_UPDATE_FEED";

thread_local! {
    /// The updater, once started.
    static STARTED: Cell<Option<&'static Updater>> = const { Cell::new(None) };
}

/// Sparkle's updater, running: the target of "Check for Updates…". Kept for the whole
/// process, as Sparkle expects of it.
pub struct Updater {
    /// An `SPUStandardUpdaterController`.
    controller: Retained<AnyObject>,
    /// Kept here, as the one below: Sparkle holds its delegates weakly.
    _delegate: Option<Retained<FeedOverride>>,
    _reminders: Retained<Reminders>,
}

impl Updater {
    /// What "Check for Updates…" sends `checkForUpdates:` to. It also answers
    /// `validateMenuItem:`: the item is off while a check is under way.
    pub fn controller(&self) -> &AnyObject {
        &self.controller
    }
}

/// Shows the update found, in front, or checks for one if there is none: what clicking
/// the reminder does. On the main thread.
pub fn show() {
    if let Some(updater) = STARTED.get() {
        // SAFETY: the controller's action, which takes a sender, here none.
        let () = unsafe { msg_send![updater.controller(), checkForUpdates: None::<&AnyObject>] };
    }
}

/// Starts Sparkle's updater, if the app is bundled with Sparkle and a feed: else there is
/// nothing to update, and no menu item to show. Never dropped: the menu item doesn't
/// retain its target.
pub fn start(mtm: MainThreadMarker) -> Option<&'static Updater> {
    let main = NSBundle::mainBundle();
    main.bundleIdentifier()?;
    let feed = std::env::var(FEED_VAR).ok().filter(|feed| !feed.is_empty());
    if feed.is_none()
        && main
            .objectForInfoDictionaryKey(ns_string!("SUFeedURL"))
            .is_none()
    {
        tracing::warn!("the bundle names no update feed; no updates");
        return None;
    }
    let path = format!("{}/Sparkle.framework", main.privateFrameworksPath()?);
    let Some(sparkle) = NSBundle::bundleWithPath(&NSString::from_str(&path)) else {
        tracing::warn!(path, "the bundle has no Sparkle; no updates");
        return None;
    };
    // SAFETY: the bundle's own framework, signed with it, loaded on the main thread.
    if let Err(e) = unsafe { sparkle.loadAndReturnError() } {
        let error = e.localizedDescription().to_string();
        tracing::warn!(path, error, "couldn't load Sparkle; no updates");
        return None;
    }
    let Some(class) = AnyClass::get(c"SPUStandardUpdaterController") else {
        tracing::warn!(
            path,
            "Sparkle has no SPUStandardUpdaterController; no updates"
        );
        return None;
    };
    let delegate = feed.as_deref().map(|feed| FeedOverride::new(mtm, feed));
    let delegate_object: Option<&AnyObject> = delegate.as_deref().map(AsRef::as_ref);
    let reminders = Reminders::new(mtm);
    // SAFETY: Sparkle's initializer, with the types it declares (a BOOL, the updater's
    // delegate, nil or not, and the user driver's), on the main thread. The delegates
    // live as long as the controller: all are kept in `Updater`, which is leaked.
    let controller: Option<Retained<AnyObject>> = unsafe {
        msg_send![
            msg_send![class, alloc],
            initWithStartingUpdater: true,
            updaterDelegate: delegate_object,
            userDriverDelegate: &*reminders,
        ]
    };
    let Some(controller) = controller else {
        tracing::warn!("Sparkle's updater didn't start; no updates");
        return None;
    };
    // Which feed, in the log: one from the variable is easy to leave set by mistake.
    match &feed {
        Some(feed) => tracing::info!(feed, "updater started, on {FEED_VAR}'s feed"),
        None => tracing::info!("updater started"),
    }
    let updater: &'static Updater = Box::leak(Box::new(Updater {
        controller,
        _delegate: delegate,
        _reminders: reminders,
    }));
    STARTED.set(Some(updater));
    Some(updater)
}

define_class!(
    /// Sparkle's updater delegate while `CATMAIL_UPDATE_FEED` names a feed: the feed it
    /// reads instead of the bundle's.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CatmailUpdateFeed"]
    #[ivars = Retained<NSString>]
    struct FeedOverride;

    unsafe impl NSObjectProtocol for FeedOverride {}

    impl FeedOverride {
        /// `SPUUpdaterDelegate`'s: Sparkle asks for it before each check.
        #[unsafe(method_id(feedURLStringForUpdater:))]
        fn feed(&self, _updater: &AnyObject) -> Option<Retained<NSString>> {
            Some(self.ivars().clone())
        }
    }
);

impl FeedOverride {
    fn new(mtm: MainThreadMarker, feed: &str) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(NSString::from_str(feed));
        // SAFETY: NSObject's `init`, on a subclass whose ivars are set.
        unsafe { msg_send![super(this), init] }
    }
}
