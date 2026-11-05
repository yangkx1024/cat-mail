//! The system's notifications for new mail, as the GUI's (`new_mail.rs`): what the
//! engine announces (`Event::NewMail`) is gathered for a moment, read again, and told
//! about; clicking a notification shows the main window on the message. There with or
//! without a window. What they say and where a click goes are
//! `catmail_frontend::new_mail`'s.
//!
//! ⚠️ nsh has no notifications; these are GLib's (`GNotification`), sent through the
//! application. Without a sandbox GLib speaks `org.freedesktop.Notifications`, and a
//! click comes back as the app's `open-mail` action, with the host's activation token
//! for the window. On GNOME, GLib uses GNOME Shell's own interface instead, which
//! activates the app through its D-Bus name: the app doesn't own one (it isn't unique
//! through D-Bus, see `shell::run`), so a click there does nothing.

use std::cell::RefCell;

use catmail_core::{Event, MessageId, RecvError};
use catmail_frontend::new_mail::{Arrivals, GATHER, Target, notifications};
use nsh_platform_linux::gtk4::{self as gtk, gio, glib, prelude::*};

use crate::{engine, shell};

/// The action a notification's click runs, with its tag.
const OPEN: &str = "open-mail";

thread_local! {
    /// Announced and not yet told about (GTK's thread).
    static ARRIVALS: RefCell<Arrivals> = RefCell::default();
}

/// Tells about the new mail the open engine announces from now on, until it closes. On
/// GTK's thread.
pub fn start(app: &gtk::Application) {
    let Some(client) = engine::open() else {
        return;
    };
    let open = gio::SimpleAction::new(OPEN, Some(glib::VariantTy::STRING));
    open.connect_activate(|_, tag| {
        let target = tag.and_then(|tag| tag.str()).and_then(Target::from_tag);
        if let Some(target) = target {
            shell::show_at(target.destination(), None);
        }
    });
    app.add_action(&open);
    let mut events = client.subscribe();
    glib::spawn_future_local(async move {
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
    glib::spawn_future_local(async {
        glib::timeout_future(GATHER).await;
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
        let Some(app) = shell::application() else {
            return;
        };
        for notice in notifications(&still_new) {
            let notification = gio::Notification::new(&notice.title);
            notification.set_body(Some(&notice.body));
            notification.set_category(Some("email.arrived"));
            notification.set_default_action_and_target_value(
                &format!("app.{OPEN}"),
                Some(&notice.tag.to_variant()),
            );
            // By its tag: one for several messages replaces the one before.
            app.send_notification(Some(&notice.tag), &notification);
        }
    });
}
