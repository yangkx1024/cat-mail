//! Updates, from Sparkle 2: the framework the bundle carries in `Contents/Frameworks`
//! (`packaging/macos/build-dmg.sh` puts it there), loaded at run time rather than linked,
//! so a binary run on its own (`cargo run`) needs none, and has no updater.
//!
//! Sparkle reads the feed `SUFeedURL` names (Info.plist) on its own schedule, asks the
//! user before its first automatic check, and shows its own windows; "Check for Updates…"
//! in the app menu checks now. Installing quits the app as the Dock's Quit does (a quit
//! Apple Event, then `applicationShouldTerminate:`), so the composers save first, and a
//! quit called off leaves the update to be installed once the app quits.
//! `CATMAIL_UPDATE_FEED` names another feed, for trying an update out. See
//! docs/architecture/packaging.md.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
use objc2::{DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSBundle, NSObject, NSString, ns_string};

/// The feed to read instead of the bundle's, for trying an update out.
const FEED_VAR: &str = "CATMAIL_UPDATE_FEED";

/// Sparkle's updater, running: the target of "Check for Updates…".
pub struct Updater {
    /// An `SPUStandardUpdaterController`.
    controller: Retained<AnyObject>,
    /// Kept here: Sparkle holds its delegate weakly.
    _delegate: Option<Retained<FeedOverride>>,
}

impl Updater {
    /// What "Check for Updates…" sends `checkForUpdates:` to. It also answers
    /// `validateMenuItem:`: the item is off while a check is under way.
    pub fn controller(&self) -> &AnyObject {
        &self.controller
    }
}

/// Starts Sparkle's updater, if the app is bundled with Sparkle and a feed: else there is
/// nothing to update, and no menu item to show.
pub fn start(mtm: MainThreadMarker) -> Option<Updater> {
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
    // SAFETY: Sparkle's initializer, with the types it declares (a BOOL, the updater's
    // delegate and the user driver's, either nil), on the main thread. The delegate
    // lives as long as the controller: both are kept in `Updater`.
    let controller: Option<Retained<AnyObject>> = unsafe {
        msg_send![
            msg_send![class, alloc],
            initWithStartingUpdater: true,
            updaterDelegate: delegate_object,
            userDriverDelegate: None::<&AnyObject>,
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
    Some(Updater {
        controller,
        _delegate: delegate,
    })
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
