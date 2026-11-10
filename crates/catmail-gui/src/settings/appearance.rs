//! Settings › Appearance: light or dark, the theme for each, importing a terminal color
//! scheme as one, and the text size.

use std::rc::Rc;

use catmail_frontend::folders::FolderIcon;
use catmail_frontend::settings::{Appearance, TEXT_SIZES};
use catmail_frontend::theme::{
    IMPORT_HELP, IMPORTABLE_FORMATS, ImportCandidate, Rgb, Rgba, Swatch, TerminalPalette,
    ThemeEntry, file_name, import_title, read_import,
};
use nsh_base::theme;
use nsh_core::Color;
use nsh_core::element::desktop::{MenuItem, popup_menu};
use nsh_core::element::open_path;
use nsh_core::file_dialog::OpenDialog;
use nsh_core::geometry::relative;
use nsh_core::prelude::*;

use crate::form;
use crate::icons::{Icon, icon};
use crate::shell;
use crate::tasks::{in_view, off_thread};
use crate::toast::{Level, notify, report};

use crate::buttons::labelled_button;

use super::{help, prefs, section_title};
use crate::theme::SelectableText as _;

pub fn appearance() -> impl IntoElement {
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
                    form::menu_field(
                        "Text size",
                        move || text_size().to_owned(),
                        choose_text_size,
                    )
                    .w(px(160.0)),
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

/// A color of the theme, for the card that pictures it: these are the theme's own
/// colors, not the window's.
fn color(color: Rgba) -> Hsla {
    let Rgb { r, g, b } = color.rgb;
    Color::rgba(r, g, b, color.alpha).into()
}

/// A theme drawn small, in its own colors, with its name.
fn theme_card(entry: ThemeEntry, dark: bool) -> AnyElement {
    let swatch = Swatch::of(&entry.config);
    let hues = swatch.hues.map(color).into_iter();
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
        .bg(color(swatch.background))
        .child(
            view()
                .w(px(32.0))
                .bg(color(swatch.sidebar))
                .border_r_1()
                .border_color(color(swatch.border)),
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
                        .child(
                            view()
                                .w(px(6.0))
                                .h(px(6.0))
                                .rounded(px(3.0))
                                .bg(color(swatch.primary)),
                        )
                        .child(line(55.0, color(swatch.foreground))),
                )
                .child(view().pl(px(12.0)).child(line(40.0, color(swatch.muted))))
                .child(
                    view()
                        .ml(px(12.0))
                        .h(px(12.0))
                        .w(relative(0.6))
                        .rounded(px(3.0))
                        .bg(color(swatch.selection)),
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
    in_view(async move {
        let path = match picked.await {
            Ok(Some(paths)) => match paths.into_iter().next() {
                Some(path) => path,
                None => return,
            },
            Ok(None) => return,
            Err(e) => {
                report("Couldn't choose a file", e);
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
        Err(e) => report("Couldn't import the theme", e),
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
    in_view(async move {
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
                            .selectable_text()
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
