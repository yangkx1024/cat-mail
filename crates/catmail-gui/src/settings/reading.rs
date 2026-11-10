//! Settings › Reading: loading remote images, and their cache.

use catmail_frontend::settings::image_cache_help;
use nsh_base::theme;
use nsh_core::prelude::*;

use crate::engine::client;
use crate::icons::Icon;
use crate::shell;
use crate::tasks::in_view;
use crate::toast::report;

use crate::buttons::labelled_button;

use super::{help, prefs, section_title, toggle};

pub fn reading() -> impl IntoElement {
    // Measured each time the section shows: the cache grows as messages show their
    // remote images.
    // The section's: it stops when the section goes, with what shows the size.
    let size = signal(None::<u64>);
    let clearing = signal(false);
    spawn(async move {
        match client().image_cache_size().await {
            Ok(bytes) => size.set(Some(bytes)),
            Err(e) => tracing::warn!(error = %e, "couldn't measure the image cache"),
        }
    });
    let clear = move || {
        if clearing.get_untracked() {
            return;
        }
        clearing.set(true);
        // Cleared even if the section goes meanwhile; only what it shows goes with it.
        in_view(async move {
            let cleared = client().clear_image_cache().await;
            if let Err(e) = cleared {
                report("Couldn't clear the image cache", e);
            }
            let measured = client().image_cache_size().await.ok();
            if size.try_set(measured) {
                clearing.set(false);
            }
        });
    };
    view()
        .gap(px(16.0))
        .child(section_title("Reading"))
        .child(toggle(
            "Mark messages as read when opened",
            move || prefs(|s| s.mark_read_on_open),
            |on| shell::update_settings(|s| s.mark_read_on_open = on),
        ))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .pt(px(16.0))
                .border_t_1()
                .border_color(move || theme().border)
                .child(
                    view()
                        .flex_1()
                        .gap(px(2.0))
                        .child(
                            text("Remote images")
                                .text_sm()
                                .text_color(move || theme().foreground),
                        )
                        .child(help(move || image_cache_help(size.get()))),
                )
                .child(labelled_button(
                    Icon::Trash,
                    "Clear image cache",
                    true,
                    move || !clearing.get() && size.get().is_some_and(|bytes| bytes > 0),
                    clear,
                )),
        )
}
