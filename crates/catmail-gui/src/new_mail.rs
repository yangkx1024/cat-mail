//! The system's notifications for new mail, which the engine announces
//! (`Event::NewMail`); clicking one shows the main window on the message. Lives with
//! the app, not a window: it tells about mail that arrives while none is open. What they
//! say and where a click goes are `catmail_frontend::new_mail`'s. See
//! docs/architecture/background.md.

use catmail_core::{Event, MessageId, RecvError};
use catmail_frontend::new_mail::{Arrivals, GATHER, Target, notifications};
use gpui_kit::{App, Global, SystemNotification, Task};

use crate::resident::Resident;
use crate::settings::Preferences;

pub struct Notifier {
    /// Announced and not yet told about. The task waiting out [`GATHER`] isn't kept: it
    /// would be dropped, and so cancelled, as it takes what arrived.
    arrivals: Arrivals,
    _events: Task<()>,
}

impl Global for Notifier {}

impl Notifier {
    /// Tells about the new mail [`Resident`]'s engine announces from now on, until it
    /// closes.
    pub fn start(cx: &mut App) {
        let Some(client) = Resident::client(cx) else {
            return;
        };
        let mut events = client.subscribe();
        let events = cx.spawn(async move |cx| {
            loop {
                match events.recv().await {
                    Ok(Event::NewMail { messages, .. }) => {
                        cx.update(|cx| Self::arrived(messages, cx));
                    }
                    Ok(_) => {}
                    // Missed announcements can't be had again; the mail is in the list.
                    Err(RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "missed events, maybe of new mail");
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
        cx.set_global(Self {
            arrivals: Arrivals::default(),
            _events: events,
        });
        cx.on_system_notification_response(|response, cx| {
            if let Some(target) = Target::from_tag(&response.tag) {
                Resident::show_at(target.destination(), None, cx);
            }
        });
    }

    fn arrived(messages: Vec<MessageId>, cx: &mut App) {
        if !cx.global_mut::<Self>().arrivals.arrived(messages) {
            return;
        }
        cx.spawn(async move |cx| {
            cx.background_executor().timer(GATHER).await;
            let told = cx.update(|cx| {
                let tell = Preferences::get(cx).notify_new_mail;
                let arrived = cx.global_mut::<Self>().arrivals.take();
                if !tell {
                    return None;
                }
                // None once the app is quitting.
                Some((Resident::client(cx)?, arrived))
            });
            let Some((client, arrived)) = told else {
                return;
            };
            // Read or moved meanwhile, here or elsewhere: not new any more.
            let still_new = match client.unread_in_inbox(arrived).await {
                Ok(still_new) => still_new,
                Err(e) => return tracing::warn!(error = %e, "couldn't read new mail"),
            };
            cx.update(|cx| {
                for notice in notifications(&still_new) {
                    cx.show_system_notification(SystemNotification {
                        tag: notice.tag.into(),
                        title: notice.title.into(),
                        body: notice.body.into(),
                        actions: Vec::new(),
                    });
                }
            });
        })
        .detach();
    }
}
