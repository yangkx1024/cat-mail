//! Gentle reminders of an update (Sparkle's). While the app runs without a window, as an
//! accessory app, Sparkle would show an update a scheduled check finds at once: behind
//! the other apps, or in front if the app has just launched (at login). It leaves that to
//! the app then, which posts a notification instead. Clicking it, or opening a window,
//! shows the update in front. With a window the app is regular, and Sparkle's own way is
//! gentle already: it waits for the user to come back to the app. Without leave to
//! notify, the update shows as Sparkle would have shown it.

use std::cell::Cell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObjectProtocol};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{NSArray, NSError, NSObject, NSString, ns_string};
use objc2_user_notifications::{
    UNAuthorizationStatus, UNMutableNotificationContent, UNNotificationRequest,
    UNNotificationSettings, UNUserNotificationCenter,
};

/// The identifier of the reminder's notification.
pub const REMINDER: &str = "update";

thread_local! {
    /// A reminder is out: the update waits for the user.
    static WAITING: Cell<bool> = const { Cell::new(false) };
}

define_class!(
    /// Sparkle's user driver delegate: the reminders (`SPUStandardUserDriverDelegate`).
    /// Sparkle asks it with `respondsToSelector:`, so it needs no protocol of Sparkle's.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CatmailUpdateReminders"]
    pub struct Reminders;

    unsafe impl NSObjectProtocol for Reminders {}

    impl Reminders {
        #[unsafe(method(supportsGentleScheduledUpdateReminders))]
        fn supports_gentle_reminders(&self) -> Bool {
            Bool::YES
        }

        /// Sparkle shows the update, unless the app runs without a window.
        #[unsafe(method(standardUserDriverShouldHandleShowingScheduledUpdate:andInImmediateFocus:))]
        fn sparkle_shows(&self, _update: &AnyObject, _immediate_focus: Bool) -> Bool {
            let app = NSApplication::sharedApplication(self.mtm());
            Bool::new(app.activationPolicy() != NSApplicationActivationPolicy::Accessory)
        }

        #[unsafe(method(standardUserDriverWillHandleShowingUpdate:forUpdate:state:))]
        fn will_show(&self, sparkle_shows: Bool, update: &AnyObject, _state: &AnyObject) {
            if !sparkle_shows.as_bool() {
                remind(update);
            }
        }

        /// The user has seen the update, or chosen what to do with it.
        #[unsafe(method(standardUserDriverDidReceiveUserAttentionForUpdate:))]
        fn attended(&self, _update: &AnyObject) {
            withdraw();
        }

        /// Dismissed, skipped, or an error.
        #[unsafe(method(standardUserDriverWillFinishUpdateSession))]
        fn finishing(&self) {
            withdraw();
        }
    }
);

impl Reminders {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: NSObject's `init`, on a subclass that adds nothing.
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

/// The app has a window again: an update waiting for the user shows now, in front.
pub fn came_back() {
    if WAITING.get() {
        super::show();
    }
}

/// Posts the reminder of `update` (an `SUAppcastItem`) if the app may notify, and shows
/// the update otherwise.
fn remind(update: &AnyObject) {
    // SAFETY: SUAppcastItem's version for people, never nil.
    let version: Retained<NSString> = unsafe { msg_send![update, displayVersionString] };
    let version = version.to_string();
    WAITING.set(true);
    let answered = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        // SAFETY: the center's settings, lent for the call.
        let status = unsafe { settings.as_ref() }.authorizationStatus();
        let allowed = status == UNAuthorizationStatus::Authorized
            || status == UNAuthorizationStatus::Provisional;
        let version = version.clone();
        // The center answers off the main thread.
        super::super::invoke(move || {
            if allowed {
                post(&version);
            } else {
                tracing::info!(version, "can't notify of the update; showing it");
                super::show();
            }
        });
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .getNotificationSettingsWithCompletionHandler(&answered);
}

fn post(version: &str) {
    // Seen meanwhile.
    if !WAITING.get() {
        return;
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(ns_string!("Update available"));
    content.setBody(&NSString::from_str(&format!(
        "catmail {version} is out. Click to see what's new and install it."
    )));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(REMINDER),
        &content,
        None,
    );
    let added = RcBlock::new(|error: *mut NSError| {
        // SAFETY: the call's error, or nil.
        if let Some(error) = unsafe { error.as_ref() } {
            let error = error.localizedDescription().to_string();
            tracing::warn!(
                error,
                "couldn't post the update's reminder; showing the update"
            );
            super::super::invoke(super::show);
        }
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, Some(&added));
    tracing::info!(version, "reminded of the update");
}

/// Takes the reminder away: also one left from an earlier run of the app.
fn withdraw() {
    WAITING.set(false);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let identifiers = NSArray::from_retained_slice(&[NSString::from_str(REMINDER)]);
    center.removePendingNotificationRequestsWithIdentifiers(&identifiers);
    center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
}
