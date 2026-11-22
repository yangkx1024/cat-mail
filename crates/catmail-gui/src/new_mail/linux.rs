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
    let Some(app) = gio::Application::default() else {
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
    let Some(app) = gio::Application::default() else {
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
    let Some(bus) = gio::Application::default().and_then(|app| app.dbus_connection()) else {
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
async fn call<T: glib::variant::FromVariant>(
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::process::Command;
    use std::rc::Rc;
    use std::time::Duration;

    use catmail_frontend::new_mail::Notice;
    use nsh_platform_linux::gtk4::{gio, glib, prelude::*};

    use super::show;

    const ID: &str = "net.yangkx.catmail.NotifTest";
    const CHILD: &str = "new_mail::platform::tests::sends_escaped_or_plain_body";
    const CASE_ENV: &str = "CATMAIL_TEST_NOTIF_CASE";
    const BODY: &str = "Q&A <draft> & <b>hi</b>";
    const ESCAPED: &str = "Q&amp;A &lt;draft&gt; &amp; &lt;b&gt;hi&lt;/b&gt;";

    /// Each GLib notification backend is chosen once per process on the session bus, so
    /// each scenario runs on a bus of its own.
    #[test]
    fn body_escaping_follows_glib_backend() {
        let cases: &[(&str, &[(&str, &str)])] = &[
            // KDE / freedesktop server with `body-markup`: escaped.
            ("fdo_markup", &[]),
            // Freedesktop server without `body-markup` (e.g. noctalia): plain.
            ("fdo_plain", &[]),
            // GNOME Shell (`org.gtk.Notifications` owned, even with a markup fdo server): plain.
            ("gnome", &[]),
            // Flatpak / portal (`GIO_USE_PORTALS=1`): plain.
            ("portal", &[("GIO_USE_PORTALS", "1")]),
            // Forced freedesktop backend overrides portal and checks capabilities: escaped.
            (
                "forced_fdo",
                &[
                    ("GIO_USE_PORTALS", "1"),
                    ("GNOTIFICATION_BACKEND", "freedesktop"),
                ],
            ),
        ];
        for (case, envs) in cases {
            let mut cmd = Command::new("dbus-run-session");
            cmd.arg("--")
                .arg(std::env::current_exe().unwrap())
                .args(["--exact", CHILD, "--ignored", "--test-threads=1"])
                .env_remove("GIO_USE_PORTALS")
                .env_remove("GNOTIFICATION_BACKEND")
                .env(CASE_ENV, case);
            for (k, v) in *envs {
                cmd.env(k, v);
            }
            let run = cmd.output().expect("dbus-run-session");
            assert!(
                run.status.success(),
                "case {case} failed:\n{}{}",
                String::from_utf8_lossy(&run.stdout),
                String::from_utf8_lossy(&run.stderr),
            );
        }
    }

    const FDO_XML: &str = r#"
        <node>
          <interface name='org.freedesktop.Notifications'>
            <method name='GetCapabilities'>
              <arg type='as' name='caps' direction='out'/>
            </method>
            <method name='Notify'>
              <arg type='s' name='app_name' direction='in'/>
              <arg type='u' name='replaces_id' direction='in'/>
              <arg type='s' name='app_icon' direction='in'/>
              <arg type='s' name='summary' direction='in'/>
              <arg type='s' name='body' direction='in'/>
              <arg type='as' name='actions' direction='in'/>
              <arg type='a{sv}' name='hints' direction='in'/>
              <arg type='i' name='expire_timeout' direction='in'/>
              <arg type='u' name='id' direction='out'/>
            </method>
          </interface>
        </node>
    "#;

    const GTK_XML: &str = r#"
        <node>
          <interface name='org.gtk.Notifications'>
            <method name='AddNotification'>
              <arg type='s' name='app_id' direction='in'/>
              <arg type='s' name='id' direction='in'/>
              <arg type='a{sv}' name='notification' direction='in'/>
            </method>
          </interface>
        </node>
    "#;

    const PORTAL_XML: &str = r#"
        <node>
          <interface name='org.freedesktop.portal.Notification'>
            <method name='AddNotification'>
              <arg type='s' name='id' direction='in'/>
              <arg type='a{sv}' name='notification' direction='in'/>
            </method>
          </interface>
        </node>
    "#;

    #[test]
    #[ignore = "run on a bus of its own by body_escaping_follows_glib_backend"]
    fn sends_escaped_or_plain_body() {
        let case = std::env::var(CASE_ENV).expect("case env");
        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
        let captured: Rc<RefCell<Option<(String, String)>>> = Rc::new(RefCell::new(None));

        let app = gio::Application::new(Some(ID), gio::ApplicationFlags::NON_UNIQUE);
        app.set_default();
        app.register(gio::Cancellable::NONE).unwrap();

        glib::MainContext::default().block_on(async {
            let server = gio::DBusConnection::for_address_future(
                &address,
                gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                    | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                None,
            )
            .await
            .unwrap();

            let mut regs = Vec::new();
            let mut names = Vec::new();

            // Export freedesktop server in every case except pure portal.
            let fdo_caps: Vec<&str> = match case.as_str() {
                "fdo_plain" => vec!["actions", "body"],
                _ => vec!["actions", "body", "body-markup"],
            };
            let fdo_info = gio::DBusNodeInfo::for_xml(FDO_XML).unwrap();
            let fdo_iface = fdo_info
                .lookup_interface("org.freedesktop.Notifications")
                .unwrap();
            let cap_fdo = captured.clone();
            regs.push(
                server
                    .register_object("/org/freedesktop/Notifications", &fdo_iface)
                    .method_call(move |_, _, _, _, method, params, inv| match method {
                        "GetCapabilities" => {
                            inv.return_value(Some(&(fdo_caps.clone(),).to_variant()));
                        }
                        "Notify" => {
                            let body = params.child_value(4).get::<String>().unwrap();
                            *cap_fdo.borrow_mut() = Some(("fdo".into(), body));
                            inv.return_value(Some(&(1_u32,).to_variant()));
                        }
                        other => panic!("unexpected fdo method: {other}"),
                    })
                    .build()
                    .unwrap(),
            );
            own_name(&server, "org.freedesktop.Notifications", &mut names).await;

            if case == "gnome" {
                let gtk_info = gio::DBusNodeInfo::for_xml(GTK_XML).unwrap();
                let gtk_iface = gtk_info.lookup_interface("org.gtk.Notifications").unwrap();
                let cap_gtk = captured.clone();
                regs.push(
                    server
                        .register_object("/org/gtk/Notifications", &gtk_iface)
                        .method_call(move |_, _, _, _, method, params, inv| {
                            assert_eq!(method, "AddNotification");
                            let dict = params.child_value(2);
                            let body = dict_string(&dict, "body");
                            *cap_gtk.borrow_mut() = Some(("gtk".into(), body));
                            inv.return_value(None);
                        })
                        .build()
                        .unwrap(),
                );
                own_name(&server, "org.gtk.Notifications", &mut names).await;
            }

            if case == "portal" || case == "forced_fdo" {
                let portal_info = gio::DBusNodeInfo::for_xml(PORTAL_XML).unwrap();
                let portal_iface = portal_info
                    .lookup_interface("org.freedesktop.portal.Notification")
                    .unwrap();
                let cap_portal = captured.clone();
                regs.push(
                    server
                        .register_object("/org/freedesktop/portal/desktop", &portal_iface)
                        .method_call(move |_, _, _, _, method, params, inv| {
                            assert_eq!(method, "AddNotification");
                            let dict = params.child_value(1);
                            let body = dict_string(&dict, "body");
                            *cap_portal.borrow_mut() = Some(("portal".into(), body));
                            inv.return_value(None);
                        })
                        .build()
                        .unwrap(),
                );
                own_name(&server, "org.freedesktop.portal.Desktop", &mut names).await;
            }

            show(&Notice {
                tag: "new-mail/1/42".into(),
                title: "Alice & Bob <team>".into(),
                body: BODY.into(),
            });

            for _ in 0..300 {
                if captured.borrow().is_some() {
                    break;
                }
                glib::timeout_future(Duration::from_millis(10)).await;
            }
            for reg in regs {
                let _ = server.unregister_object(reg);
            }
            for id in names {
                gio::bus_unown_name(id);
            }
        });

        let (backend, body) = captured
            .borrow()
            .clone()
            .expect("notification was not delivered");
        match case.as_str() {
            "fdo_markup" | "forced_fdo" => {
                assert_eq!((backend.as_str(), body.as_str()), ("fdo", ESCAPED))
            }
            "fdo_plain" => assert_eq!((backend.as_str(), body.as_str()), ("fdo", BODY)),
            "gnome" => assert_eq!((backend.as_str(), body.as_str()), ("gtk", BODY)),
            "portal" => assert_eq!((backend.as_str(), body.as_str()), ("portal", BODY)),
            other => panic!("unknown case: {other}"),
        }
    }

    async fn own_name(conn: &gio::DBusConnection, name: &str, owned: &mut Vec<gio::OwnerId>) {
        let ready = Rc::new(RefCell::new(false));
        let flag = ready.clone();
        owned.push(gio::bus_own_name_on_connection(
            conn,
            name,
            gio::BusNameOwnerFlags::NONE,
            move |_, _| *flag.borrow_mut() = true,
            |_, _| {},
        ));
        for _ in 0..200 {
            if *ready.borrow() {
                return;
            }
            glib::timeout_future(Duration::from_millis(5)).await;
        }
        panic!("could not own {name}");
    }

    fn dict_string(dict: &glib::Variant, key: &str) -> String {
        let map = glib::VariantDict::new(Some(dict));
        map.lookup::<String>(key).unwrap().unwrap()
    }
}
