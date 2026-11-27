//! The app's notifications on macOS: UserNotifications (`UNUserNotificationCenter`), with
//! the app's own delegate of the center (nsh's views don't take it: their
//! `AppOptions::notifications` is off). New mail's (`system::new_mail`) and the update's
//! reminder (the updater's) go through it. A notification's identifier says what it is
//! about: one posted with the identifier of one still shown replaces it, and a click
//! brings the identifier back, to what [`on_click`] set for it, or else to what
//! [`install`] was given.
//!
//! The framework ends a process without a bundle identifier that touches the center
//! (`cargo run`): there is none outside the app's bundle.

use std::cell::{OnceCell, RefCell};

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{AnyThread as _, DefinedClass as _, define_class, msg_send};
use objc2_foundation::{NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

define_class!(
    /// Its ivar is where clicks go that [`on_click`] doesn't take.
    #[unsafe(super(NSObject))]
    #[name = "CatmailNotificationDelegate"]
    #[ivars = fn(&str)]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// While the app is in front, a notification shows as it does behind.
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
                let identifier = response.notification().request().identifier().to_string();
                let clicked = *self.ivars();
                // The center may answer off the main thread.
                super::invoke(move || match handler(&identifier) {
                    Some(handler) => handler(),
                    None => clicked(&identifier),
                });
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

/// What clicking a notification with an identifier does.
type Handler = (&'static str, fn());

thread_local! {
    static HANDLERS: RefCell<Vec<Handler>> = const { RefCell::new(Vec::new()) };
}

/// Clicking a notification with this identifier does `clicked`, on the main thread.
pub fn on_click(identifier: &'static str, clicked: fn()) {
    HANDLERS.with_borrow_mut(|handlers| handlers.push((identifier, clicked)));
}

fn handler(identifier: &str) -> Option<fn()> {
    HANDLERS.with_borrow(|handlers| {
        handlers
            .iter()
            .find(|(known, _)| *known == identifier)
            .map(|&(_, clicked)| clicked)
    })
}

/// The notification center, once [`install`] has found the app's bundle.
fn center() -> Option<Retained<UNUserNotificationCenter>> {
    CENTER.with(|held| held.get().map(|(center, _)| center.clone()))
}

/// Becomes the notification center's delegate: a click on a notification that
/// [`on_click`] doesn't take goes to `clicked`, with its identifier, on the main thread. On the
/// main thread, before the app finishes launching: a click that launches the app comes
/// then.
pub fn install(clicked: fn(&str)) {
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        return;
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let delegate = Delegate::alloc().set_ivars(clicked);
    // SAFETY: NSObject's `init`, on a subclass whose ivars are set.
    let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // Once: the app launches once.
    CENTER.with(|held| {
        let _ = held.set((center, delegate));
    });
}

fn failure(error: *mut NSError) -> Option<String> {
    // SAFETY: the call's error, or nil.
    unsafe { error.as_ref() }.map(|error| error.localizedDescription().to_string())
}

/// Asks the user to let the app notify, the first time: whether there are notifications
/// at all (none outside the bundle). On the main thread.
pub fn ask() -> bool {
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

/// Notifies now, in place of a notification still shown with the same identifier. If the
/// center refuses it (the app may not notify, or there are no notifications outside the
/// bundle), `failed` hears why, on the main thread. On the main thread.
pub fn post(identifier: &str, title: &str, body: &str, sound: bool, failed: fn(String)) {
    let Some(center) = center() else {
        return failed("no notifications outside the app's bundle".into());
    };
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    if sound {
        content.setSound(Some(&UNNotificationSound::defaultSound()));
    }
    // No trigger: now.
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(identifier),
        &content,
        None,
    );
    let added = RcBlock::new(move |error: *mut NSError| {
        if let Some(error) = failure(error) {
            // The center answers off the main thread.
            super::invoke(move || failed(error));
        }
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&added));
}

/// Takes the notification with this identifier away from Notification Center. On the
/// main thread.
pub fn withdraw(identifier: &str) {
    if let Some(center) = center() {
        let identifiers = NSArray::from_retained_slice(&[NSString::from_str(identifier)]);
        center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
    }
}
