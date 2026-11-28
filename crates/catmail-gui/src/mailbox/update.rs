//! The update's button, at the end of the sidebar's bottom row: there while there is an
//! update ([`crate::system::update`]) the app can do something with (`Update::button`): to
//! install it, then to restart into it. Installing failed: a toast says why.

use catmail_frontend::system::update::{Button, Update};
use futures_util::StreamExt as _;
use nsh_core::prelude::*;

use crate::buttons::busy_labelled_button;
use crate::icons::Icon;
use crate::system::update;
use crate::toast::{Level, notify};

pub fn update_button() -> impl IntoElement {
    let (now, mut changes) = update::follow();
    let shown = signal(now);
    spawn(async move {
        while let Some(now) = changes.next().await {
            if let Some(Update::Failed(why)) = &now {
                notify(Level::Error, why.to_string());
            }
            shown.set(now);
        }
    });
    let button = computed(move || shown.with(|u| u.as_ref().and_then(Update::button)));
    // Only when they change, not with each percent: the spinner would start again.
    let installing = computed(move || matches!(button.get(), Some(Button::Installing(_))));
    let restart = computed(move || button.get() == Some(Button::Restart));
    when(
        move || button.get().is_some(),
        move || {
            busy_labelled_button(
                move || {
                    if restart.get() {
                        Icon::ArrowPath
                    } else {
                        Icon::ArrowDownTray
                    }
                },
                move || button.get().map(Button::label).unwrap_or_default(),
                move || installing.get(),
                move || {
                    if restart.get_untracked() {
                        update::restart();
                    } else {
                        update::install();
                    }
                },
            )
            .flex_shrink_0()
        },
    )
}
