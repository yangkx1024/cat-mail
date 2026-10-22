//! The Reading section: marking messages read, and the cache of remote images.

use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window, div};

use crate::format;

use super::*;

impl SettingsView {
    /// Asks how much the cache of remote images takes.
    pub(super) fn measure_image_cache(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.clearing_image_cache {
            return;
        }
        let client = self.client.clone();
        self._image_cache = cx.spawn_in(window, async move |this, cx| {
            let size = client.image_cache_size().await;
            this.update(cx, |this, cx| {
                match size {
                    Ok(size) => this.image_cache = Some(size),
                    Err(e) => tracing::warn!(error = %e, "couldn't measure the image cache"),
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn clear_image_cache(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.clearing_image_cache {
            return;
        }
        self.clearing_image_cache = true;
        cx.notify();
        let client = self.client.clone();
        self._image_cache = cx.spawn_in(window, async move |this, cx| {
            let cleared = client.clear_image_cache().await;
            let size = client.image_cache_size().await.ok();
            this.update_in(cx, |this, window, cx| {
                this.clearing_image_cache = false;
                this.image_cache = size;
                if let Err(e) = cleared {
                    window.push_notification(
                        Notification::error(format!("Couldn't clear the image cache: {e}")),
                        cx,
                    );
                }
                cx.notify();
            })
            .ok();
        });
    }

    pub(super) fn render_reading(&self, cx: &mut Context<Self>) -> AnyElement {
        let mark_read = Preferences::get(cx).mark_read_on_open;
        let theme = cx.theme();
        let help = "Remote images you chose to show are kept on this computer and show \
                    again without being loaded.";
        let help = match self.image_cache {
            Some(size) => format!("{help} They take {}.", format::size(size)),
            None => help.to_owned(),
        };
        v_flex()
            .gap_4()
            .child(section_title(
                "Reading",
                "How messages behave as you read them.",
                cx,
            ))
            .child(
                Switch::new("mark-read-on-open")
                    .checked(mark_read)
                    .label("Mark messages as read when opened")
                    .on_change(cx.listener(|_, checked: &bool, _, cx| {
                        let checked = *checked;
                        Preferences::update(cx, |s| s.mark_read_on_open = checked);
                        cx.notify();
                    })),
            )
            .child(
                h_flex()
                    .gap_3()
                    .pt_4()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().child("Remote images"))
                            .child(
                                div()
                                    .id("image-cache-size")
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(help),
                            ),
                    )
                    .child(
                        Button::new("clear-image-cache")
                            .outline()
                            .small()
                            .label("Clear image cache")
                            .disabled(self.image_cache.is_none_or(|size| size == 0))
                            .loading(self.clearing_image_cache)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.clear_image_cache(window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }
}
