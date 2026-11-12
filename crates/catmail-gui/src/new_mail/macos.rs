//! New mail's notifications on macOS: UserNotifications (`UNUserNotificationCenter`),
//! the app's own delegate of the center (nsh's views don't take it: their
//! `AppOptions::notifications` is off). A notification's identifier is its tag, so one
//! for several messages replaces the one before, and a click brings the tag back.
//!
//! The framework ends a process without a bundle identifier that touches the center
//! (`cargo run`): there are none outside the app's bundle.

// UserNotifications is Objective-C: its delegate is defined with `unsafe`.
#![allow(unsafe_code)]

use std::cell::OnceCell;

use block2::{DynBlock, RcBlock};
use catmail_frontend::new_mail::Notice;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{AnyThread as _, define_class, msg_send};
use objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use crate::shell;

define_class!(
    /// The app's answers to the notification center.
    #[unsafe(super(NSObject))]
    #[name = "CatmailNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// While the app is in front, the notification shows as it does behind.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            done: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            done.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            done: &DynBlock<dyn Fn()>,
        ) {
            // SAFETY: an immutable UserNotifications constant.
            let default = unsafe { UNNotificationDefaultActionIdentifier };
            if response.actionIdentifier().isEqualToString(default) {
                let tag = response.notification().request().identifier().to_string();
                // The center may answer off the main thread.
                shell::invoke(move || super::clicked(&tag));
            }
            done.call(());
        }
    }
);

thread_local! {
    /// The notification center and its delegate, which it doesn't retain: set at launch,
    /// inside the app's bundle only.
    static CENTER: OnceCell<(Retained<UNUserNotificationCenter>, Retained<Delegate>)> =
        const { OnceCell::new() };
}

/// The notification center, once [`install`] has found the app's bundle.
fn center() -> Option<Retained<UNUserNotificationCenter>> {
    CENTER.with(|held| held.get().map(|(center, _)| center.clone()))
}

/// Becomes the notification center's delegate. On the main thread, before the app
/// finishes launching: a click that launches the app comes then.
pub fn install() {
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        return;
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    // SAFETY: NSObject's `init`, on a subclass that adds nothing.
    let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(), init] };
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // Once: the app launches once.
    CENTER.with(|held| {
        let _ = held.set((center, delegate));
    });
}

/// What the error a completion handler was given says, if there was one.
fn failure(error: *mut NSError) -> Option<String> {
    // SAFETY: the call's error, or nil.
    unsafe { error.as_ref() }.map(|error| error.localizedDescription().to_string())
}

/// Asks the user to let the app notify, the first time: whether there are notifications
/// (none outside the bundle). On the main thread.
pub fn listen() -> bool {
    let Some(center) = center() else {
        tracing::info!("no notifications outside the app's bundle");
        return false;
    };
    let answered = RcBlock::new(|granted: Bool, error: *mut NSError| {
        match failure(error) {
            Some(error) => tracing::info!(error, "couldn't ask to notify"),
            // Turned off in System Settings, it says so here too.
            None if !granted.as_bool() => tracing::info!("notifications are turned off"),
            None => {}
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &answered,
    );
    true
}

/// On the main thread.
pub fn show(notice: &Notice) {
    let Some(center) = center() else {
        return;
    };
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&notice.title));
    content.setBody(&NSString::from_str(&notice.body));
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    // By its tag: one for several messages replaces the one before. No trigger: now.
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&notice.tag),
        &content,
        None,
    );
    let added = RcBlock::new(|error: *mut NSError| {
        if let Some(error) = failure(error) {
            tracing::warn!(error, "couldn't notify");
        }
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&added));
}
