//! New mail's notifications on Linux: GLib's (`GNotification`), sent through the
//! application. Without a sandbox GLib speaks `org.freedesktop.Notifications`, and a
//! click comes back as the app's `open-mail` action, with the host's activation token
//! for the window. In a Flatpak GLib uses the notification portal, and on GNOME GNOME
//! Shell's own interface: there the click activates the action through the app id's
//! D-Bus name, which the shell owns for that (`own_app_id`).

use std::cell::RefCell;
use std::path::Path;

use catmail_frontend::new_mail::Notice;
use nsh_platform_linux::gtk4::{gio, glib, prelude::*};

use crate::shell;

/// The action a notification's click runs, with its tag.
const OPEN: &str = "open-mail";

/// How long the bus and the notification server get to answer.
const TIMEOUT_MS: i32 = 5000;

/// Whether a notification's body reaches a server that reads it as markup, once known.
enum Markup {
    Unknown,
    /// Being found out; the notices shown meanwhile, in order.
    Asking(Vec<Notice>),
    Known(bool),
}

thread_local! {
    /// GTK's thread.
    static MARKUP: RefCell<Markup> = const { RefCell::new(Markup::Unknown) };
}

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
    let (markup, ask) = MARKUP.with_borrow_mut(|state| match state {
        Markup::Known(markup) => (Some(*markup), false),
        Markup::Asking(waiting) => {
            waiting.push(notice.clone());
            (None, false)
        }
        Markup::Unknown => {
            *state = Markup::Asking(vec![notice.clone()]);
            (None, true)
        }
    });
    if let Some(markup) = markup {
        post(notice, markup);
    }
    if ask {
        shell::spawn(async {
            let markup = reads_markup().await;
            let waiting = MARKUP.with_borrow_mut(|state| {
                match std::mem::replace(state, Markup::Known(markup)) {
                    Markup::Asking(waiting) => waiting,
                    _ => Vec::new(),
                }
            });
            for notice in &waiting {
                post(notice, markup);
            }
        });
    }
}

fn post(notice: &Notice, markup: bool) {
    let Some(app) = shell::application() else {
        return;
    };
    let notification = gio::Notification::new(&notice.title);
    // A server that reads markup would make a subject's `<b>` bold and its `<a href>` a
    // link, and choke on a bare `&`.
    let body = if markup {
        glib::markup_escape_text(&notice.body).to_string()
    } else {
        notice.body.clone()
    };
    notification.set_body(Some(&body));
    notification.set_category(Some("email.arrived"));
    notification.set_default_action_and_target_value(
        &format!("app.{OPEN}"),
        Some(&notice.tag.to_variant()),
    );
    // By its tag: one for several messages replaces the one before.
    app.send_notification(Some(&notice.tag), &notification);
}

/// Whether the body GLib sends goes to a server that reads it as markup. GLib picks its
/// way once, at the first notification, in this order: the portal in a sandbox, whose
/// `body` is plain text; GNOME Shell's `org.gtk.Notifications` when someone owns it, which
/// shows the body as it is; else `org.freedesktop.Notifications`, which gets the body
/// unchanged and reads markup when it has the `body-markup` capability. Asked just before
/// the first notification, as GLib does.
async fn reads_markup() -> bool {
    if uses_portal() {
        return false;
    }
    let Some(bus) = shell::application().and_then(|app| app.dbus_connection()) else {
        return false;
    };
    let gtk_server = bus
        .call_future(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&("org.gtk.Notifications",).to_variant()),
            Some(glib::VariantTy::new("(b)").unwrap()),
            gio::DBusCallFlags::NONE,
            TIMEOUT_MS,
        )
        .await;
    // GLib takes a failed question for no owner too.
    if gtk_server.is_ok_and(|reply| reply.get::<(bool,)>().is_some_and(|(owned,)| owned)) {
        return false;
    }
    let capabilities = bus
        .call_future(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "GetCapabilities",
            None,
            Some(glib::VariantTy::new("(as)").unwrap()),
            gio::DBusCallFlags::NONE,
            TIMEOUT_MS,
        )
        .await;
    match capabilities.map(|reply| reply.get::<(Vec<String>,)>()) {
        Ok(Some((capabilities,))) => capabilities.iter().any(|c| c == "body-markup"),
        // Most servers read markup: escaped is the safe guess.
        _ => true,
    }
}

/// GLib's own test for the portal (`glib_should_use_portal`): a Flatpak or Snap sandbox,
/// or `GIO_USE_PORTALS=1`.
fn uses_portal() -> bool {
    Path::new("/.flatpak-info").exists()
        || std::env::var_os("SNAP").is_some()
        || std::env::var("GIO_USE_PORTALS").is_ok_and(|v| v.starts_with('1'))
}
