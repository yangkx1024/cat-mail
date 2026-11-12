//! New mail's notifications on Linux: GLib's (`GNotification`), sent through the
//! application. Without a sandbox GLib speaks `org.freedesktop.Notifications`, and a
//! click comes back as the app's `open-mail` action, with the host's activation token
//! for the window. In a Flatpak GLib uses the notification portal, and on GNOME GNOME
//! Shell's own interface: there the click activates the action through the app id's
//! D-Bus name, which the shell owns for that (`own_app_id`).

use catmail_frontend::new_mail::Notice;
use nsh_platform_linux::gtk4::{gio, glib, prelude::*};

use crate::shell;

/// The action a notification's click runs, with its tag.
const OPEN: &str = "open-mail";

/// Hears the notifications' clicks: whether there are notifications. On GTK's thread.
pub fn listen() -> bool {
    let Some(app) = shell::application() else {
        return false;
    };
    let open = gio::SimpleAction::new(OPEN, Some(glib::VariantTy::STRING));
    open.connect_activate(|_, tag| {
        if let Some(tag) = tag.and_then(|tag| tag.str()) {
            super::clicked(tag);
        }
    });
    app.add_action(&open);
    true
}

/// On GTK's thread.
pub fn show(notice: &Notice) {
    let Some(app) = shell::application() else {
        return;
    };
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
