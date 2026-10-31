//! The General section: whether the app keeps running once its windows are closed, and
//! tells about new mail.

use gpui_kit::component::switch::Switch;
use gpui_kit::component::v_flex;
use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _};

use super::*;

impl SettingsView {
    pub(super) fn render_general(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = Preferences::get(cx);
        let (keep_running, notify_new_mail) = (settings.keep_running, settings.notify_new_mail);
        v_flex()
            .gap_4()
            .child(section_title("General"))
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
            .child(
                Switch::new("notify-new-mail")
                    .checked(notify_new_mail)
                    .label("Notify about new mail")
                    .on_change(cx.listener(|_, checked: &bool, _, cx| {
                        let checked = *checked;
                        Preferences::update(cx, |s| s.notify_new_mail = checked);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}
