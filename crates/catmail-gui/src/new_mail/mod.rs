//! The system's notifications for new mail: what the engine announces (`Event::NewMail`) is
//! gathered for a moment, read again, and told about; clicking a notification shows the
//! main window on the message. There with or without a window. What they say and where a
//! click goes are `catmail_frontend::new_mail`'s; showing them and hearing their clicks
//! is the platform's: GLib's `GNotification` on Linux (`linux.rs`), UserNotifications on
//! macOS (`macos.rs`).
//!
//! ⚠️ nsh's notifications go through a view's root, and the app runs on without one.

use std::cell::RefCell;

use catmail_core::{Event, MessageId, RecvError};
use catmail_frontend::new_mail::{Arrivals, GATHER, Target, notifications};

use crate::{engine, shell};

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod platform;

#[cfg(target_os = "macos")]
pub use self::platform::install;

thread_local! {
    /// Announced and not yet told about (the main thread).
    static ARRIVALS: RefCell<Arrivals> = RefCell::default();
}

/// Tells about the new mail the open engine announces from now on, until it closes. On
/// the main thread.
pub fn start() {
    let Some(client) = engine::open() else {
        return;
    };
    if !platform::listen() {
        return;
    }
    let mut events = client.subscribe();
    shell::spawn(async move {
        loop {
            match events.recv().await {
                Ok(Event::NewMail { messages, .. }) => arrived(messages),
                Ok(_) => {}
                // Missed announcements can't be had again; the mail is in the list.
                Err(RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "missed events, maybe of new mail");
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

fn arrived(messages: Vec<MessageId>) {
    if !ARRIVALS.with_borrow_mut(|arrivals| arrivals.arrived(messages)) {
        return;
    }
    shell::spawn(async {
        shell::sleep(GATHER).await;
        let arrived = ARRIVALS.with_borrow_mut(Arrivals::take);
        if !shell::settings().notify_new_mail {
            return;
        }
        // None once the app is quitting.
        let Some(client) = engine::open() else {
            return;
        };
        // Read or moved meanwhile, here or elsewhere: not new any more.
        let still_new = match client.unread_in_inbox(arrived).await {
            Ok(still_new) => still_new,
            Err(e) => return tracing::warn!(error = %e, "couldn't read new mail"),
        };
        for notice in notifications(&still_new) {
            platform::show(&notice);
        }
    });
}

/// The notification tagged `tag` was clicked: the main window shows where it says. On the
/// main thread.
fn clicked(tag: &str) {
    if let Some(target) = Target::from_tag(tag) {
        shell::show_at(target.destination(), None);
    }
}
