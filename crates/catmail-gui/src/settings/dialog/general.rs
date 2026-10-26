//! The General section: whether the app keeps running once its windows are closed.

use gpui_kit::component::switch::Switch;
use gpui_kit::component::v_flex;
use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _};

use super::*;

impl SettingsView {
    pub(super) fn render_general(&self, cx: &mut Context<Self>) -> AnyElement {
        let keep_running = Preferences::get(cx).keep_running;
        v_flex()
            .gap_4()
            .child(section_title(
                "General",
                "How cat-mail runs on this computer.",
                cx,
            ))
            .child(
                Switch::new("keep-running")
                    .checked(keep_running)
                    .label("Keep running after the window is closed")
                    .on_change(cx.listener(|_, checked: &bool, _, cx| {
                        let checked = *checked;
                        Preferences::update(cx, |s| s.keep_running = checked);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}
