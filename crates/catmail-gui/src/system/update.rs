//! Updates of the Flatpak (`catmail_frontend::system::update`; on macOS Sparkle updates
//! the app, in the AppKit shell, and there is never one here). Watched from launch, with
//! the engine open or not. The main window's sidebar shows what there is ([`follow`]),
//! installs it and restarts into it; while the main window isn't open, a notification
//! says there is one, and clicking it opens the window.

use std::sync::OnceLock;

use catmail_frontend::settings::watched::Watched;
use catmail_frontend::system::resident::QuitReason;
use catmail_frontend::system::update::{Update, Updates};
use futures_channel::mpsc;

use crate::shell;

/// What there is, for the views to follow.
static UPDATE: Watched<Option<Update>> = Watched::new(None);

/// The portal's monitor, once watching.
static UPDATES: OnceLock<Updates> = OnceLock::new();

/// Watches for updates, if there's anything to update. On the main thread, at launch.
#[cfg(target_os = "linux")]
pub fn start() {
    let Some(updates) = catmail_frontend::system::update::watch(|update| UPDATE.set(update)) else {
        return;
    };
    let _ = UPDATES.set(updates);
    platform::remind();
}

/// The monitor says `update`, as a test would have it.
#[cfg(test)]
pub fn said(update: Option<Update>) {
    UPDATE.set(update);
}

/// What there is now, and each time that changes.
pub fn follow() -> (Option<Update>, mpsc::UnboundedReceiver<Option<Update>>) {
    UPDATE.follow()
}

/// Installs the update: [`follow`] says how it goes.
pub fn install() {
    if let Some(updates) = UPDATES.get() {
        updates.install();
    }
}

/// Quits, and starts the version installed once the engine has closed: unless a composer
/// stays open, which calls quitting off, as any quit. From a view's thread.
pub fn restart() {
    drop(shell::on_main(|| {
        shell::quit_then(QuitReason::Update, start_installed)
    }));
}

/// Starts the version installed, from the Flatpak portal: it outlives this process, finds
/// it quitting (no longer answering later launches), and waits for it to let go of the
/// data directory.
fn start_installed() {
    if let Err(e) = catmail_frontend::system::update::restart() {
        tracing::error!(error = %e, "couldn't start the update; quitting without it");
    }
}

/// The notification while the main window isn't open: GLib's, as new mail's.
#[cfg(target_os = "linux")]
mod platform {
    use catmail_frontend::system::update::Update;
    use futures_util::StreamExt as _;
    use nsh_platform_linux::gtk4::{gio, prelude::*};

    use crate::shell;

    /// The notification's id, and the action its click runs.
    const ID: &str = "update";
    const SHOW: &str = "show-update";

    /// Tells of an update found or installed while the main window isn't open, and takes
    /// the notification back once there's nothing to tell (installing, or gone).
    pub fn remind() {
        let Some(app) = gio::Application::default() else {
            return;
        };
        let show = gio::SimpleAction::new(SHOW, None);
        show.connect_activate(|_, _| shell::show_at(None, None));
        app.add_action(&show);
        let (_, mut changes) = super::follow();
        shell::spawn(async move {
            // A notification is out.
            let mut told = false;
            while let Some(update) = changes.next().await {
                let Some((title, body)) = update.as_ref().and_then(Update::notice) else {
                    if std::mem::take(&mut told) {
                        app.withdraw_notification(ID);
                    }
                    continue;
                };
                if shell::main_window_open() {
                    continue;
                }
                let notification = gio::Notification::new(title);
                notification.set_body(Some(body));
                notification.set_default_action(&format!("app.{SHOW}"));
                app.send_notification(Some(ID), &notification);
                told = true;
                tracing::info!(title, "told of the update");
            }
        });
    }
}
