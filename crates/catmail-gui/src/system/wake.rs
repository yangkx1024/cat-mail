//! The system says the computer woke: the engine starts over on new connections and syncs
//! and sends what is due at once (`MailClient::resume`), rather than once it notices the
//! sleep itself, up to half a minute later. Linux: logind's `PrepareForSleep(false)` on the
//! system bus; macOS: `NSWorkspaceDidWakeNotification`. Followed from the engine's opening
//! until the app ends. See docs/architecture/background.md.

use crate::engine;

fn woke() {
    // Not while the app quits.
    if let Some(client) = engine::open() {
        client.resume();
    }
}

pub use self::platform::follow;

#[cfg(target_os = "linux")]
mod platform {
    use std::cell::RefCell;

    use nsh_platform_linux::gtk4::{gio, glib};

    thread_local! {
        /// logind's signal, followed for as long as the app runs.
        static SUBSCRIPTION: RefCell<Option<gio::SignalSubscription>> =
            const { RefCell::new(None) };
    }

    /// On the main thread.
    pub fn follow() {
        gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, |bus| {
            let bus = match bus {
                Ok(bus) => bus,
                Err(e) => return tracing::info!(error = %e, "no system bus to hear wakes on"),
            };
            let subscription = bus.subscribe_to_signal(
                Some("org.freedesktop.login1"),
                Some("org.freedesktop.login1.Manager"),
                Some("PrepareForSleep"),
                Some("/org/freedesktop/login1"),
                None,
                gio::DBusSignalFlags::NONE,
                |signal| {
                    if woke_from(signal.parameters) {
                        super::woke();
                    }
                },
            );
            SUBSCRIPTION.with_borrow_mut(|held| *held = Some(subscription));
        });
    }

    /// `PrepareForSleep` is sent with `true` before the sleep and `false` after it.
    pub(super) fn woke_from(parameters: &glib::Variant) -> bool {
        parameters.get::<(bool,)>() == Some((false,))
    }
}

#[cfg(target_os = "macos")]
// AppKit is Objective-C: observing it is `unsafe`.
#[allow(unsafe_code)]
mod platform {
    use std::cell::RefCell;
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification};
    use objc2_foundation::{NSNotification, NSObjectProtocol};

    thread_local! {
        /// The observer, kept for as long as the app runs.
        static OBSERVER: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> =
            const { RefCell::new(None) };
    }

    /// On the main thread.
    pub fn follow() {
        let block = RcBlock::new(|_: NonNull<NSNotification>| super::woke());
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        // SAFETY: the name is AppKit's constant, there is no object to match, and the block
        // may run on any thread: it only takes the engine's handle, which is `Sync`. No
        // queue: the workspace posts on the main thread.
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidWakeNotification),
                None,
                None,
                &block,
            )
        };
        OBSERVER.with_borrow_mut(|held| *held = Some(observer));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use nsh_platform_linux::gtk4::glib::prelude::*;

    use super::platform::woke_from;

    #[test]
    fn only_the_signal_after_the_sleep_is_a_wake() {
        assert!(woke_from(&(false,).to_variant()));
        assert!(!woke_from(&(true,).to_variant()));
        assert!(!woke_from(&().to_variant()));
    }
}
