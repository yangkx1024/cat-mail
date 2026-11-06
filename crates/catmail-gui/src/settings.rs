//! The Settings dialog: appearance (light or dark, the theme for each, importing a terminal
//! color scheme, text size), accounts and the addresses they send as, and reading (with the
//! cache of remote images). The settings are `shell`'s, shared by every window; the themes
//! and their import are `catmail_frontend::theme`'s, the wording of Accounts
//! `catmail_frontend::accounts`'.
//!
//! General (running in the background, new mail notifications) comes with them (P9).

use std::rc::Rc;

use catmail_core::{AccountId, IdentityId};
use catmail_frontend::accounts;
use catmail_frontend::folders::FolderIcon;
use catmail_frontend::settings::{
    Appearance, DEFAULT_TEXT_SIZE, Settings, TEXT_SIZES, image_cache_help,
};
use catmail_frontend::theme::{
    IMPORT_HELP, IMPORTABLE_FORMATS, ImportCandidate, TerminalPalette, ThemeConfig, ThemeEntry,
    file_name, import_title, read_import,
};
use futures_util::StreamExt as _;
use nsh_base::{alert_dialog, switch, theme};
use nsh_core::Color;
use nsh_core::element::desktop::{MenuItem, popup_menu};
use nsh_core::element::{open_path, set_color_scheme};
use nsh_core::file_dialog::OpenDialog;
use nsh_core::geometry::relative;
use nsh_core::prelude::*;

use crate::app::{client, in_mailbox, off_thread, report};
use crate::commands::{Size, icon_button, with_accounts};
use crate::form::{self, NameRequest, ask_name, modal};
use crate::icons::{Icon, icon};
use crate::login;
use crate::shell;
use crate::sidebar::{self, Accounts};
use crate::toast::{Level, notify};

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

/// An import waiting to be confirmed, or why it can't be.
#[derive(Clone, PartialEq)]
enum Import {
    Ready(Candidate),
    Failed { file_name: String, error: String },
}

/// A file read for importing; one read is never equal to another.
#[derive(Clone)]
struct Candidate(Rc<ImportCandidate>);

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
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
        Section::Appearance => appearance().into_element(),
        Section::Accounts => accounts_section().into_element(),
        Section::Reading => reading().into_element(),
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

/// A small button with an icon and a label: outlined, or plain.
fn labelled_button(
    glyph: Icon,
    label: &'static str,
    outlined: bool,
    enabled: impl Fn() -> bool + Copy + 'static,
    press: impl Fn() + 'static,
) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(30.0))
        .px(px(12.0))
        .rounded(px(15.0))
        .border_1()
        .border_color(move || {
            if outlined {
                theme().border
            } else {
                AdaptiveColor::TRANSPARENT
            }
        })
        .hover(move |s| {
            s.bg(move || {
                if enabled() {
                    theme().accent
                } else {
                    AdaptiveColor::TRANSPARENT
                }
            })
        })
        .opacity(move || if enabled() { 1.0_f32 } else { 0.5 })
        .label(label)
        .on_press(move || {
            if untracked(enabled) {
                press();
            }
        })
        .child(icon(move || glyph, 15.0).text_color(move || theme().foreground))
        .child(
            text(label)
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || theme().foreground),
        )
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

// --- Appearance ---------------------------------------------------------------------

fn appearance() -> impl IntoElement {
    let import = signal(None::<Import>);
    // The themes on offer, read again once one is imported.
    let themes = signal(shell::with_themes(|t| t.entries().to_vec()));
    dynamic(move || match import.get() {
        Some(pending) => import_pane(pending, import, themes).into_element(),
        None => themes_pane(import, themes).into_element(),
    })
}

fn themes_pane(
    import: Signal<Option<Import>>,
    themes: Signal<Vec<ThemeEntry>>,
) -> impl IntoElement {
    let modes = view()
        .flex_row()
        .rounded(px(8.0))
        .border_1()
        .border_color(move || theme().border)
        .children(Appearance::ALL.map(|mode| {
            let glyph = match mode {
                Appearance::System => Icon::ComputerDesktop,
                Appearance::Light => Icon::Sun,
                Appearance::Dark => Icon::Moon,
            };
            let selected = move || prefs(|s| s.appearance == mode);
            view()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .h(px(28.0))
                .px(px(10.0))
                .rounded(px(7.0))
                .bg(move || {
                    if selected() {
                        crate::theme::tint(theme().primary)
                    } else {
                        AdaptiveColor::TRANSPARENT
                    }
                })
                .label(mode.label())
                .on_press(move || shell::update_settings(|s| s.appearance = mode))
                .child(icon(move || glyph, 15.0).text_color(move || theme().foreground))
                .child(
                    text(mode.label())
                        .text_sm()
                        .text_color(move || theme().foreground),
                )
                .into_element()
        }));
    let grid = move |dark: bool| {
        view().flex_row().flex_wrap().gap(px(12.0)).child(each(
            move || {
                themes.with(|t| {
                    t.iter()
                        .filter(|e| e.is_dark() == dark)
                        .cloned()
                        .collect::<Vec<_>>()
                })
            },
            |entry| entry.name().to_owned(),
            move |entry, _| theme_card(entry.get_untracked(), dark),
        ))
    };
    let text_size = move || {
        prefs(|s| {
            TEXT_SIZES
                .iter()
                .find(|(size, _)| *size == s.text_size)
                .map_or("Custom", |(_, label)| label)
        })
    };
    view()
        .gap(px(24.0))
        .pb(px(8.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .child(section_title("Appearance").flex_1())
                .child(modes),
        )
        .child(
            view()
                .gap(px(12.0))
                .child(section_title("Light theme"))
                .child(grid(false)),
        )
        .child(
            view()
                .gap(px(12.0))
                .child(section_title("Dark theme"))
                .child(grid(true)),
        )
        .child(
            view()
                .gap(px(8.0))
                .child(
                    view()
                        .flex_row()
                        .gap(px(8.0))
                        .child(labelled_button(
                            Icon::ArrowDownTray,
                            "Import theme",
                            true,
                            || true,
                            move || pick_import(import),
                        ))
                        .child(labelled_button(
                            Icon::Folder(FolderIcon::Folder),
                            "Open themes folder",
                            false,
                            || shell::themes_dir().is_some(),
                            open_themes_folder,
                        )),
                )
                .child(help(IMPORT_HELP.to_owned())),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(16.0))
                .pt(px(16.0))
                .border_t_1()
                .border_color(move || theme().border)
                .child(
                    view()
                        .flex_1()
                        .gap(px(2.0))
                        .child(section_title("Text size"))
                        .child(help(format!(
                            "Scales the whole window. {} and {} change it anywhere.",
                            key_label("mod+plus"),
                            key_label("mod+minus")
                        ))),
                )
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .w(px(160.0))
                        .h(px(30.0))
                        .px(px(10.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(move || theme().input)
                        .label("Text size")
                        .on_press(choose_text_size)
                        .child(
                            text(move || text_size().to_owned())
                                .flex_1()
                                .text_sm()
                                .text_color(move || theme().foreground),
                        )
                        .child(
                            icon(|| Icon::ChevronDown, 14.0)
                                .text_color(move || theme().muted_foreground),
                        ),
                ),
        )
}

fn choose_text_size() {
    let current = untracked(|| prefs(|s| s.text_size));
    let items = TEXT_SIZES
        .iter()
        .map(|&(size, label)| {
            MenuItem::new(label)
                .checked(size == current)
                .on_select(move || shell::update_settings(|s| s.text_size = size))
        })
        .collect();
    drop(popup_menu(items));
}

fn choose_theme(name: String, dark: bool) {
    shell::update_settings(|s| {
        if dark {
            s.dark_theme = name;
        } else {
            s.light_theme = name;
        }
    });
}

/// A color of the theme, for the card that pictures it: these are the theme's own
/// colors, not the window's.
fn swatch(config: &ThemeConfig, key: &str) -> Option<Hsla> {
    config.color(key).and_then(Color::parse).map(Hsla::from)
}

/// A theme drawn small, in its own colors, with its name.
fn theme_card(entry: ThemeEntry, dark: bool) -> AnyElement {
    let config = &entry.config;
    let background = swatch(config, "background").unwrap_or(hsla(0.0, 0.0, 1.0, 1.0));
    let foreground = swatch(config, "foreground").unwrap_or(hsla(0.0, 0.0, 0.0, 1.0));
    let primary = swatch(config, "primary.background").unwrap_or(foreground);
    let color = |key: &str, fallback: Hsla| swatch(config, key).unwrap_or(fallback);
    let sidebar = color("sidebar.background", background);
    let border = color("border", foreground.opacity(0.15));
    let muted = color("muted.foreground", foreground.opacity(0.6));
    let selection = color("list.active.background", primary.opacity(0.15));
    let hues = [
        "danger.background",
        "success.background",
        "warning.background",
    ]
    .map(|key| color(key, primary))
    .into_iter()
    .chain([primary, color("info.background", primary)]);
    let name = entry.name().to_owned();
    let chosen = name.clone();
    let selected = move || {
        let name = chosen.clone();
        prefs(move |s| {
            if dark {
                s.dark_theme == name
            } else {
                s.light_theme == name
            }
        })
    };
    let line = move |width: f32, color: Hsla| {
        view()
            .h(px(6.0))
            .w(relative(width / 100.0))
            .rounded(px(3.0))
            .bg(color)
    };
    let miniature = view()
        .flex_row()
        .h(px(64.0))
        .bg(background)
        .child(
            view()
                .w(px(32.0))
                .bg(sidebar)
                .border_r_1()
                .border_color(border),
        )
        .child(
            view()
                .flex_1()
                .gap(px(6.0))
                .p(px(10.0))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(view().w(px(6.0)).h(px(6.0)).rounded(px(3.0)).bg(primary))
                        .child(line(55.0, foreground)),
                )
                .child(view().pl(px(12.0)).child(line(40.0, muted)))
                .child(
                    view()
                        .ml(px(12.0))
                        .h(px(12.0))
                        .w(relative(0.6))
                        .rounded(px(3.0))
                        .bg(selection),
                ),
        );
    let picked = name.clone();
    let selected_ = selected.clone();
    let selected__ = selected.clone();
    let selected___ = selected.clone();
    view()
        .w(px(176.0))
        .rounded(px(10.0))
        .border(move || if selected_() { px(2.0) } else { px(1.0) })
        .border_color(move || {
            if selected__() {
                theme().ring
            } else {
                theme().border
            }
        })
        .overflow_hidden()
        .role(AccessibilityRole::Button)
        .label(name.clone())
        .hover(|s| s.border_color(move || theme().ring))
        .on_press(move || choose_theme(picked.clone(), dark))
        .child(miniature)
        .child(
            view().flex_row().h(px(6.0)).children(
                hues.map(|hue| view().flex_1().bg(hue).into_element())
                    .collect::<Vec<_>>(),
            ),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .child(
                    text(name)
                        .number_of_lines(1)
                        .flex_1()
                        .text_xs()
                        .style_when(selected.clone(), |s| s.font_weight(FontWeight::SEMIBOLD))
                        .text_color(move || theme().foreground),
                )
                .children(entry.is_imported().then(|| {
                    text("Imported")
                        .text_xs()
                        .px(px(6.0))
                        .rounded(px(4.0))
                        .bg(move || theme().muted)
                        .text_color(move || theme().muted_foreground)
                        .into_element()
                }))
                .child(when(selected___, || {
                    icon(|| Icon::Check, 12.0).text_color(move || theme().primary)
                })),
        )
        .into_element()
}

fn pick_import(import: Signal<Option<Import>>) {
    let picked = nsh_core::element::pick_paths(OpenDialog::files().button_label("Import"));
    in_mailbox(async move {
        let path = match picked.await {
            Ok(Some(paths)) => match paths.into_iter().next() {
                Some(path) => path,
                None => return,
            },
            Ok(None) => return,
            Err(e) => {
                notify(Level::Error, format!("Couldn't choose a file: {e}"));
                return;
            }
        };
        let read = {
            let path = path.clone();
            off_thread(move || read_import(&path)).await
        };
        import.set(Some(match read {
            Some(Ok(candidate)) => Import::Ready(Candidate(Rc::new(candidate))),
            Some(Err(error)) => Import::Failed {
                file_name: file_name(&path),
                error,
            },
            None => return,
        }));
    });
}

fn confirm_import(
    candidate: &ImportCandidate,
    import: Signal<Option<Import>>,
    themes: Signal<Vec<ThemeEntry>>,
) {
    import.set(None);
    let Some(dir) = shell::themes_dir() else {
        notify(
            Level::Error,
            "Couldn't import the theme: there is no folder to keep themes in.",
        );
        return;
    };
    let installed = shell::with_themes(|t| t.install(&dir, candidate.themes.clone()));
    themes.set(shell::with_themes(|t| t.entries().to_vec()));
    shell::themes_changed();
    match installed {
        // One theme imported is put to use for its appearance at once.
        Ok(names) => {
            if let (Some(dark), [name]) = (candidate.single(), names.as_slice()) {
                choose_theme(name.clone(), dark);
            }
        }
        Err(e) => notify(Level::Error, format!("Couldn't import the theme: {e}")),
    }
}

fn open_themes_folder() {
    let Some(dir) = shell::themes_dir() else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        notify(
            Level::Error,
            format!("Couldn't create {}: {e}", dir.display()),
        );
        return;
    }
    let opened = open_path(dir);
    in_mailbox(async move {
        if let Err(e) = opened.await {
            notify(
                Level::Error,
                format!("Couldn't open the themes folder: {e}"),
            );
        }
    });
}

/// The import waiting to be confirmed, in place of the theme lists: a dialog over the
/// dialog would stack overlays.
fn import_pane(
    pending: Import,
    import: Signal<Option<Import>>,
    themes: Signal<Vec<ThemeEntry>>,
) -> impl IntoElement {
    match pending {
        Import::Failed { file_name, error } => view()
            .gap(px(16.0))
            .child(
                view()
                    .gap(px(4.0))
                    .px(px(12.0))
                    .py(px(10.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(move || theme().destructive)
                    .child(
                        text(import_title(&file_name))
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(move || theme().destructive),
                    )
                    .child(
                        text(error)
                            .text_sm()
                            .selectable(true)
                            .text_color(move || theme().foreground),
                    ),
            )
            .child(help(IMPORTABLE_FORMATS.to_owned()))
            .child(view().flex_row().justify_end().child(form::form_button(
                "OK",
                false,
                || false,
                move || import.set(None),
            )))
            .into_element(),
        Import::Ready(Candidate(candidate)) => {
            let confirmed = candidate.clone();
            view()
                .gap(px(16.0))
                .child(
                    view()
                        .gap(px(2.0))
                        .child(
                            text(candidate.title())
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(move || theme().foreground),
                        )
                        .child(help(candidate.about())),
                )
                .children(
                    candidate
                        .themes
                        .iter()
                        .map(|theme_| {
                            view()
                                .gap(px(8.0))
                                .child(
                                    view()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(8.0))
                                        .child(
                                            text(theme_.name().to_owned())
                                                .text_sm()
                                                .text_color(move || theme().foreground),
                                        )
                                        .child(help(
                                            if theme_.is_dark() { "Dark" } else { "Light" }
                                                .to_owned(),
                                        )),
                                )
                                .children(
                                    theme_
                                        .palette
                                        .as_ref()
                                        .map(|p| palette_swatches(p).into_element()),
                                )
                                .into_element()
                        })
                        .collect::<Vec<_>>(),
                )
                .child(form::actions(
                    Some(Rc::new(move || import.set(None))),
                    "Import",
                    || false,
                    move || confirm_import(&confirmed, import, themes),
                ))
                .into_element()
        }
    }
}

/// The 16 ANSI colors of a palette, two rows of eight, under its background and
/// foreground. These are data, so they are the palette's colors, not the theme's.
fn palette_swatches(palette: &TerminalPalette) -> impl IntoElement {
    let rgb = |c: catmail_frontend::theme::Rgb| Hsla::from(Color::rgba(c.r, c.g, c.b, 0xff));
    let chip = move |color: Hsla, height: f32| {
        view()
            .flex_1()
            .h(px(height))
            .rounded(px(4.0))
            .border_1()
            .border_color(move || theme().border)
            .bg(color)
            .into_element()
    };
    view()
        .gap(px(4.0))
        .child(
            view()
                .flex_row()
                .gap(px(4.0))
                .child(chip(rgb(palette.background), 24.0))
                .child(chip(rgb(palette.foreground), 24.0)),
        )
        .children(
            palette
                .ansi
                .chunks(8)
                .map(|row| {
                    view()
                        .flex_row()
                        .gap(px(4.0))
                        .children(row.iter().map(|&c| chip(rgb(c), 20.0)).collect::<Vec<_>>())
                        .into_element()
                })
                .collect::<Vec<_>>(),
        )
}

// --- Accounts -----------------------------------------------------------------------

fn accounts_section() -> impl IntoElement {
    let accounts = move || global::<Accounts>().with(|a| a.0.clone().unwrap_or_default());
    view()
        .gap(px(16.0))
        .child(section_title("Accounts"))
        .child(view().child(each(
            accounts,
            |entry| entry.account.id,
            |entry, _| account_rows(entry.get_untracked()),
        )))
        .child(view().flex_row().child(labelled_button(
            Icon::Plus,
            "Add account",
            true,
            || true,
            || {
                close();
                login::add_account(true);
            },
        )))
}

fn account_rows(entry: catmail_frontend::mailbox::AccountEntry) -> AnyElement {
    let account = &entry.account;
    let account_id = account.id;
    let muted = move || theme().muted_foreground;
    let identities = entry
        .identities
        .iter()
        .map(|identity| {
            let id = identity.id;
            let default = identity.is_default;
            let saves_sent = identity.server_saves_sent(account);
            view()
                .flex_row()
                .items_start()
                .gap(px(12.0))
                .py(px(6.0))
                // Under the account's email, past its icon.
                .pl(px(32.0))
                .child(
                    icon(|| Icon::PaperAirplane, 16.0)
                        .mt(px(2.0))
                        .text_color(muted),
                )
                .child(
                    view()
                        .flex_1()
                        .gap(px(2.0))
                        // As the From line has it: with its name.
                        .child(
                            text(identity.address().to_string())
                                .text_sm()
                                .selectable(true)
                                .text_color(move || theme().foreground),
                        )
                        .child(help(identity.smtp.host.clone()))
                        .child(view().pt(px(4.0)).child(toggle(
                            "Server saves to Sent",
                            move || saves_sent,
                            move |on| set_saves_sent(id, on),
                        ))),
                )
                .child(
                    view()
                        .flex_row()
                        .gap(px(2.0))
                        .child(icon_button(
                            || Icon::PencilSquare,
                            || "Rename".to_owned(),
                            || true,
                            Size::Row,
                            move || rename_identity(account_id, id),
                        ))
                        // Filled for the default, which stays so until another is made it.
                        .child(icon_button(
                            move || if default { Icon::StarSolid } else { Icon::Star },
                            move || if default { "Default" } else { "Make default" }.to_owned(),
                            || true,
                            Size::Row,
                            move || {
                                if !default {
                                    make_default(id);
                                }
                            },
                        ))
                        .children((!default).then(|| {
                            icon_button(
                                || Icon::Trash,
                                || "Remove address".to_owned(),
                                || true,
                                Size::Row,
                                move || remove_identity(account_id, id),
                            )
                            .into_element()
                        })),
                )
                .into_element()
        })
        .collect::<Vec<_>>();
    view()
        .py(px(10.0))
        .border_b_1()
        .border_color(move || theme().border)
        .child(
            view()
                .flex_row()
                .items_start()
                .gap(px(12.0))
                .child(icon(|| Icon::UserCircle, 20.0).text_color(muted))
                .child(
                    view()
                        .flex_1()
                        .gap(px(2.0))
                        .child(
                            text(account.email.clone())
                                .text_sm()
                                .selectable(true)
                                .text_color(move || theme().foreground),
                        )
                        .child(help(account.imap.host.clone())),
                )
                // The confirmation says what goes, and that the server keeps it all.
                .child(icon_button(
                    || Icon::Trash,
                    || "Remove account".to_owned(),
                    || true,
                    Size::Row,
                    move || remove_account(account_id),
                )),
        )
        .children(identities)
        .child(
            view()
                .flex_row()
                .pl(px(28.0))
                .pt(px(4.0))
                .child(labelled_button(
                    Icon::Plus,
                    "Add sending address",
                    false,
                    || true,
                    move || login::add_identity(account_id),
                )),
        )
        .into_element()
}

fn entry(id: AccountId) -> Option<catmail_frontend::mailbox::AccountEntry> {
    with_accounts(|a| a.iter().find(|e| e.account.id == id).cloned())
}

/// Asks first, then does `go`: what can't be undone.
fn confirm_then(confirm: catmail_frontend::commands::Confirm, go: impl FnOnce() + 'static) {
    let answer = alert_dialog()
        .title(confirm.title)
        .description(confirm.description)
        .cancel("cancel", "Cancel")
        .destructive("remove", "Remove")
        .show();
    in_mailbox(async move {
        if answer
            .await
            .is_ok_and(|r| r.action.as_deref() == Some("remove"))
        {
            go();
        }
    });
}

fn remove_account(id: AccountId) {
    let Some(entry) = entry(id) else {
        return;
    };
    confirm_then(accounts::remove_account(&entry), move || {
        in_mailbox(async move {
            if let Err(e) = client().remove_account(id).await {
                report("Couldn't remove the account", e);
            }
        });
    });
}

fn remove_identity(account: AccountId, id: IdentityId) {
    let Some(confirm) = entry(account).and_then(|e| accounts::remove_identity(&e, id)) else {
        return;
    };
    confirm_then(confirm, move || {
        in_mailbox(async move {
            if let Err(e) = client().remove_identity(id).await {
                report("Couldn't remove the address", e);
            }
        });
    });
}

fn make_default(id: IdentityId) {
    in_mailbox(async move {
        if let Err(e) = client().set_default_identity(id).await {
            report("Couldn't make the address the default", e);
        }
    });
}

/// Whether its server saves what it sends to Sent itself, as the user says now: the
/// guess is not made again.
fn set_saves_sent(id: IdentityId, saves: bool) {
    in_mailbox(async move {
        if let Err(e) = client().set_identity_saves_sent(id, Some(saves)).await {
            report("Couldn't change how sent mail is saved", e);
        }
    });
}

fn rename_identity(account: AccountId, id: IdentityId) {
    let Some(entry) = entry(account) else {
        return;
    };
    let Some(identity) = accounts::identity(&entry, id) else {
        return;
    };
    ask_name(NameRequest {
        title: accounts::rename_title(identity),
        // Empty, the address goes alone.
        name: identity.display_name.clone().unwrap_or_default(),
        placeholder: Some("Optional"),
        ok: "Save",
        what: "change the name",
        may_be_empty: true,
        request: Rc::new(move |name| {
            Box::pin(async move { client().set_identity_name(id, Some(&name)).await.map(drop) })
        }),
    });
}

// --- Reading ------------------------------------------------------------------------

fn reading() -> impl IntoElement {
    // Measured each time the section shows: the cache grows as messages show their
    // remote images.
    let size = signal(None::<u64>);
    let clearing = signal(false);
    in_mailbox(async move {
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
        in_mailbox(async move {
            let cleared = client().clear_image_cache().await;
            size.set(client().image_cache_size().await.ok());
            clearing.set(false);
            if let Err(e) = cleared {
                notify(Level::Error, format!("Couldn't clear the image cache: {e}"));
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
