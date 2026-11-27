//! New mail's notifications on macOS, through the app's notification center
//! ([`notifications`]). A notification's identifier is its tag, so one for several
//! messages replaces the one before, and a click brings the tag back.

use catmail_frontend::new_mail::Notice;

use crate::shell::notifications;

/// Becomes the notification center's delegate, which brings clicks here. On the main
/// thread, before the app finishes launching: a click that launches the app comes then.
pub fn install() {
    notifications::install(super::clicked);
}

/// Asks the user to let the app notify, the first time: whether there are notifications
/// (none outside the bundle). On the main thread.
pub fn listen() -> bool {
    notifications::ask()
}

/// On the main thread.
pub fn show(notice: &Notice) {
    notifications::post(&notice.tag, &notice.title, &notice.body, true, |error| {
        tracing::warn!(error, "couldn't notify");
    });
}
