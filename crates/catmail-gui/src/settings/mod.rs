//! The Settings dialog: appearance (light or dark, the theme for each, importing a terminal
//! color scheme, text size), accounts and the addresses they send as, and reading (with the
//! cache of remote images). The settings are `shell`'s, shared by every window; the themes
//! and their import are `catmail_frontend::theme`'s, the wording of Accounts
//! `catmail_frontend::accounts`'.
//!
//! General (running in the background, new mail notifications) comes with them (P9).

mod accounts;
mod appearance;
mod reading;

use catmail_frontend::settings::{DEFAULT_TEXT_SIZE, Settings};
use futures_util::StreamExt as _;
use nsh_base::{switch, theme};
use nsh_core::element::set_color_scheme;
use nsh_core::prelude::*;

use crate::buttons::{Size, icon_button};
use crate::form::{self, modal};
use crate::icons::Icon;
use crate::mailbox::sidebar;
use crate::shell;

use accounts::accounts_section;

// --- The settings in a view ---------------------------------------------------------

/// The settings, as this view last heard of them.
#[derive(Clone, Default, PartialEq)]
pub struct Prefs(pub Settings);

/// Follows the settings in this view: its appearance, and what shows them. For every
/// window's root.
pub fn follow() {
    global::<Prefs>().set(Prefs(shell::settings()));
    let mut changes = shell::follow_settings();
    spawn(async move {
        while let Some(settings) = changes.next().await {
            set_color_scheme(shell::appearance(&settings));
            global::<Prefs>().set(Prefs(settings));
        }
    });
}

fn prefs<R>(f: impl FnOnce(&Settings) -> R) -> R {
    global::<Prefs>().with(|p| f(&p.0))
}

/// Ctrl + and Ctrl − change the text size anywhere, Ctrl 0 puts it back;
/// in the mailbox, Ctrl+, opens the dialog.
pub fn bind_keys(mailbox: bool) {
    if mailbox {
        on_key("mod+,", |_| open());
    }
    for key in ["mod+=", "mod+plus", "mod+shift+="] {
        on_key(key, |_| step_text_size(1));
    }
    on_key("mod+minus", |_| step_text_size(-1));
    on_key("mod+0", |_| {
        shell::update_settings(|s| s.text_size = DEFAULT_TEXT_SIZE);
    });
}

fn step_text_size(step: isize) {
    shell::update_settings(|s| s.text_size = s.stepped_text_size(step));
}

// --- The dialog ---------------------------------------------------------------------

#[derive(Clone, Copy, Default, PartialEq)]
struct SettingsOpen(bool);

pub fn open() {
    global::<SettingsOpen>().set(SettingsOpen(true));
}

pub fn close() {
    global::<SettingsOpen>().set(SettingsOpen(false));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    General,
    Appearance,
    Accounts,
    Reading,
}

impl Section {
    const ALL: [(Section, &'static str, Icon); 4] = [
        (Section::General, "General", Icon::Cog6Tooth),
        (Section::Appearance, "Appearance", Icon::Swatch),
        (Section::Accounts, "Accounts", Icon::UserCircle),
        (Section::Reading, "Reading", Icon::EnvelopeOpen),
    ];
}

/// The dialog, while it is open.
pub fn dialog() -> impl IntoElement {
    when(move || global::<SettingsOpen>().get().0, settings_dialog)
}

fn settings_dialog() -> impl IntoElement {
    // `CATMAIL_GUI_SETTINGS=appearance` (or `accounts`, `reading`) shows that section
    // first, for looking at it without a mouse (screenshots).
    let first = match std::env::var("CATMAIL_GUI_SETTINGS").as_deref() {
        Ok("appearance") => Section::Appearance,
        Ok("accounts") => Section::Accounts,
        Ok("reading") => Section::Reading,
        _ => Section::General,
    };
    let section = signal(first);
    // GPUI Kit's sidebar menu, 10 rem wide.
    let nav = view()
        .w(px(160.0))
        .flex_shrink_0()
        .gap(px(8.0))
        .pr(px(12.0))
        .border_r_1()
        .border_color(move || theme().border)
        .children(Section::ALL.map(|(which, label, glyph)| {
            let selected = move || section.get() == which;
            sidebar::menu_item(move || glyph, label, selected)
                .label(label)
                .on_press(move || section.set(which))
                .into_element()
        }));
    let content = dynamic(move || match section.get() {
        Section::General => general().into_element(),
        Section::Appearance => appearance::appearance().into_element(),
        Section::Accounts => accounts_section().into_element(),
        Section::Reading => reading::reading().into_element(),
    });
    modal(
        800.0,
        close,
        view()
            .gap(px(16.0))
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .child(form::title("Settings").flex_1())
                    .child(icon_button(
                        || Icon::XMark,
                        || "Close".to_owned(),
                        || true,
                        Size::Bar,
                        close,
                    )),
            )
            .child(
                view().flex_row().h(px(512.0)).child(nav).child(
                    scroll()
                        .flex_1()
                        .child(view().pl(px(20.0)).pr(px(4.0)).child(content)),
                ),
            ),
    )
}

fn section_title(title: &'static str) -> Text {
    text(title)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(move || theme().foreground)
}

fn help(text_: impl IntoReactive<SharedString>) -> Text {
    text(text_)
        .text_xs()
        .text_color(move || theme().muted_foreground)
}

/// A switch for a setting, with its label after it.
fn toggle(
    label: &'static str,
    checked: impl Fn() -> bool + Copy + 'static,
    change: impl Fn(bool) + 'static,
) -> impl IntoElement {
    switch()
        .checked(checked)
        .on_change(move |on: &bool| change(*on))
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .track_class("w-9 h-5 rounded-full p-0.5 bg-input checked:bg-primary")
        .thumb_class("size-4 rounded-full bg-background checked:translate-x-4")
        .child(text(label).text_sm().text_color(move || theme().foreground))
}

// --- General ------------------------------------------------------------------------

/// Whether the app keeps running once its windows are closed, and tells about new mail.
fn general() -> impl IntoElement {
    view()
        .gap(px(16.0))
        .child(section_title("General"))
        .child(toggle(
            "Keep running after the window is closed",
            move || prefs(|s| s.keep_running),
            |on| shell::update_settings(|s| s.keep_running = on),
        ))
        .child(toggle(
            "Notify about new mail",
            move || prefs(|s| s.notify_new_mail),
            |on| shell::update_settings(|s| s.notify_new_mail = on),
        ))
}
