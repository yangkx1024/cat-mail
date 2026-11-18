//! New mail's notifications on Linux: GLib's (`GNotification`), sent through the
//! application. Without a sandbox GLib speaks `org.freedesktop.Notifications`, and a
//! click comes back as the app's `open-mail` action, with the host's activation token
//! for the window. In a Flatpak GLib uses the notification portal, and on GNOME GNOME
//! Shell's own interface: there the click activates the action through the app id's
//! D-Bus name, which the shell owns for that (`own_app_id`).

use std::cell::RefCell;

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
    let known = MARKUP.with_borrow_mut(|state| match state {
        Markup::Known(markup) => Some(*markup),
        Markup::Asking(waiting) => {
            waiting.push(notice.clone());
            None
        }
        Markup::Unknown => {
            *state = Markup::Asking(vec![notice.clone()]);
            // Queued: runs on the main loop after this borrow ends.
            shell::spawn(post_waiting());
            None
        }
    });
    if let Some(markup) = known {
        post(notice, markup);
    }
}

/// Finds out whether the body is read as markup, then posts the notices shown meanwhile.
async fn post_waiting() {
    let markup = reads_markup().await;
    let Markup::Asking(waiting) = MARKUP.replace(Markup::Known(markup)) else {
        unreachable!("only this asks, once");
    };
    for notice in &waiting {
        post(notice, markup);
    }
}

fn post(notice: &Notice, markup: bool) {
    let Some(app) = shell::application() else {
        return;
    };
    let notification = gio::Notification::new(&notice.title);
    // A server that reads markup would make a subject's `<b>` bold and its `<a href>` a
    // link, and choke on a bare `&`.
    if markup {
        notification.set_body(Some(&glib::markup_escape_text(&notice.body)));
    } else {
        notification.set_body(Some(&notice.body));
    }
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
///
/// `GNOTIFICATION_BACKEND` names the way GLib tries first, kept when it works
/// (`_g_io_module_get_default_type`). `freedesktop` always does, so it skips the other two.
/// `portal` and `gtk` change nothing here: both show the body as it is, and when theirs
/// doesn't work GLib goes on in its usual order.
async fn reads_markup() -> bool {
    let freedesktop =
        std::env::var_os("GNOTIFICATION_BACKEND").is_some_and(|name| name == "freedesktop");
    if !freedesktop && uses_portal() {
        return false;
    }
    let Some(bus) = shell::application().and_then(|app| app.dbus_connection()) else {
        return false;
    };
    if !freedesktop {
        // GLib takes a failed question for no owner too.
        let gtk_server = call::<(bool,)>(
            &bus,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "NameHasOwner",
            Some(&("org.gtk.Notifications",).to_variant()),
        )
        .await;
        if gtk_server == Some((true,)) {
            return false;
        }
    }
    let capabilities = call::<(Vec<String>,)>(
        &bus,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "GetCapabilities",
        None,
    )
    .await;
    // Most servers read markup: escaped is the safe guess.
    capabilities.is_none_or(|(capabilities,)| capabilities.iter().any(|c| c == "body-markup"))
}

/// `method` of the interface `name` at `path`, on the bus `name`; its reply, if it came
/// in time and as `T`.
async fn call<T: glib::FromVariant>(
    bus: &gio::DBusConnection,
    name: &str,
    path: &str,
    method: &str,
    args: Option<&glib::Variant>,
) -> Option<T> {
    bus.call_future(
        Some(name),
        path,
        name,
        method,
        args,
        Some(&*T::static_variant_type()),
        gio::DBusCallFlags::NONE,
        TIMEOUT_MS,
    )
    .await
    .ok()?
    .get()
}

/// GLib's test for the portal (`glib_should_use_portal`), without its Snap branch, as
/// catmail ships no Snap: the Flatpak sandbox, or `GIO_USE_PORTALS=1`.
fn uses_portal() -> bool {
    catmail_frontend::in_flatpak()
        || std::env::var("GIO_USE_PORTALS").is_ok_and(|v| v.starts_with('1'))
}
