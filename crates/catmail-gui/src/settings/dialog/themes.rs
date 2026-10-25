//! The Appearance section, mostly themes: light or dark, the theme for each, importing a
//! terminal color scheme; and text size.

use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::Select;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _, rems,
};

use crate::assets::Hero;
use crate::settings::{Appearance, Preferences};
use crate::theme::appearance::{self, Swatch, Themes};
use crate::theme::colors::ThemeColors as _;

use super::*;

impl SettingsView {
    pub(super) fn pick_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let read = cx
                .background_spawn({
                    let path = path.clone();
                    async move { appearance::read_import(&path) }
                })
                .await;
            this.update(cx, |this, cx| {
                this.import = Some(match read {
                    Ok(candidate) => Import::Ready(candidate),
                    Err(error) => Import::Failed {
                        file_name: appearance::file_name(&path),
                        error,
                    },
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn confirm_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Import::Ready(candidate)) = self.import.take() else {
            return;
        };
        // One theme imported is put to use for its appearance at once.
        let single = (candidate.themes.len() == 1).then(|| candidate.themes[0].is_dark());
        match appearance::install(candidate.themes, cx) {
            Ok(names) => {
                if let (Some(dark), [name]) = (single, names.as_slice()) {
                    self.choose_theme(name.clone(), dark, cx);
                }
            }
            Err(error) => window.push_notification(
                Notification::error(format!("Couldn't import the theme: {error}")),
                cx,
            ),
        }
        cx.notify();
    }

    pub(super) fn open_themes_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = Preferences::themes_dir(cx) else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            window.push_notification(
                Notification::error(format!("Couldn't create {}: {e}", dir.display())),
                cx,
            );
            return;
        }
        cx.open_with_system(&dir);
    }

    pub(super) fn render_appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = Preferences::get(cx).clone();
        if let Some(import) = &self.import {
            return self.render_import(import, cx);
        }
        let appearance = ButtonGroup::new("appearance")
            .outline()
            .small()
            .children(Appearance::ALL.iter().map(|&a| {
                let icon = match a {
                    Appearance::System => Hero::ComputerDesktop,
                    Appearance::Light => Hero::Sun,
                    Appearance::Dark => Hero::Moon,
                };
                Button::new(SharedString::from(format!("appearance-{}", a.label())))
                    .icon(icon)
                    .label(a.label())
                    .selected(settings.appearance == a)
            }))
            .on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
                if let Some(&ix) = selected.first() {
                    this.set_appearance(Appearance::ALL[ix], cx);
                }
            }));

        // Copied out of the global, which the cards' listeners can't borrow from.
        let themes = Themes::entries(cx).to_vec();
        let grid =
            |dark: bool, selected: &str, cx: &mut Context<Self>| {
                div().grid().grid_cols(3).gap_3().children(
                    themes.iter().filter(|e| e.is_dark() == dark).map(|entry| {
                        let name = entry.name().to_owned();
                        theme_card(
                            &name,
                            entry.swatch,
                            entry.is_imported(),
                            name == selected,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.choose_theme(name.clone(), dark, cx)
                        }))
                    }),
                )
            };
        let light = grid(false, &settings.light_theme, cx);
        let dark = grid(true, &settings.dark_theme, cx);

        v_flex()
            .gap_6()
            .pb_2()
            .child(
                h_flex()
                    .gap_4()
                    .child(div().flex_1().child(section_title(
                        "Appearance",
                        "System follows the desktop's light or dark setting.",
                        cx,
                    )))
                    .child(appearance),
            )
            .child(
                v_flex()
                    .gap_3()
                    .child(section_title(
                        "Light theme",
                        "Used when the appearance is light.",
                        cx,
                    ))
                    .child(light),
            )
            .child(
                v_flex()
                    .gap_3()
                    .child(section_title(
                        "Dark theme",
                        "Used when the appearance is dark.",
                        cx,
                    ))
                    .child(dark),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("import-theme")
                                    .outline()
                                    .small()
                                    .icon(Hero::ArrowDownTray)
                                    .label("Import theme")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.pick_import(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("themes-folder")
                                    .ghost()
                                    .small()
                                    .icon(Hero::Folder)
                                    .label("Open themes folder")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_themes_folder(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Color schemes from Alacritty, kitty, Ghostty, WezTerm, Windows \
                                 Terminal, iTerm2, foot, Xresources and base16, or a GPUI Kit \
                                 theme file.",
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .pt_4()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().child(section_title(
                        "Text size",
                        "Scales the whole window. Ctrl + and Ctrl − change it anywhere.",
                        cx,
                    )))
                    .child(
                        div()
                            .w(rems(10.))
                            .child(Select::new(&self.text_size).small()),
                    ),
            )
            .into_any_element()
    }

    /// The import waiting to be confirmed, in place of the theme lists: a dialog over the
    /// dialog would stack overlays.
    fn render_import(&self, import: &Import, cx: &mut Context<Self>) -> AnyElement {
        let cancel = Button::new("import-cancel")
            .outline()
            .label("Cancel")
            .on_click(cx.listener(|this, _, _, cx| {
                this.import = None;
                cx.notify();
            }));
        match import {
            Import::Failed { file_name, error } => v_flex()
                .gap_4()
                .child(
                    Alert::error("import-error", error.clone())
                        .title(format!("Couldn’t import “{file_name}”")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "cat-mail reads color schemes from Alacritty (.toml), kitty (.conf), \
                             Ghostty, WezTerm (.toml), Windows Terminal (.json), iTerm2 \
                             (.itermcolors), foot (.ini), Xresources, base16 / base24 (.yaml) \
                             and GPUI Kit themes (.json).",
                        ),
                )
                .child(h_flex().justify_end().child(cancel.label("OK")))
                .into_any_element(),
            Import::Ready(candidate) => {
                let title = match candidate.themes.as_slice() {
                    [theme] => format!("Import “{}”?", theme.name()),
                    themes => format!("Import {} themes?", themes.len()),
                };
                let muted = cx.theme().muted_foreground;
                v_flex()
                    .gap_4()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(div().text_xs().text_color(muted).child(format!(
                                "{} · {}",
                                candidate.file_name,
                                candidate
                                    .themes
                                    .first()
                                    .map(|t| t.format.label())
                                    .unwrap_or_default()
                            ))),
                    )
                    .children(candidate.themes.iter().map(|theme| {
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_sm()
                                    .child(theme.name().to_owned())
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(if theme.is_dark() { "Dark" } else { "Light" }),
                                    ),
                            )
                            .when_some(theme.palette.as_ref(), |this, palette| {
                                this.child(palette_swatches(palette, cx))
                            })
                    }))
                    .child(
                        h_flex().justify_end().gap_2().child(cancel).child(
                            Button::new("import-confirm")
                                .primary()
                                .label("Import")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_import(window, cx)
                                })),
                        ),
                    )
                    .into_any_element()
            }
        }
    }
}

/// A theme drawn small, in its own colors, with its name. GPUI Kit's unstyled button
/// gives it focus, keyboard activation and its accessible role.
fn theme_card(
    name: &str,
    swatch: Swatch,
    imported: bool,
    selected: bool,
    cx: &App,
) -> gpui_kit::base::Button {
    let theme = cx.theme();
    let line = |width: f32, color| {
        div()
            .h_1p5()
            .w(gpui_kit::relative(width))
            .rounded_full()
            .bg(color)
    };
    let miniature = h_flex()
        .w_full()
        .items_stretch()
        .h(rems(4.))
        .bg(swatch.background)
        .child(
            div()
                .w(rems(2.))
                .bg(swatch.sidebar)
                .border_r_1()
                .border_color(swatch.border),
        )
        .child(
            v_flex()
                .flex_1()
                .gap_1p5()
                .p_2p5()
                .child(
                    h_flex()
                        .gap_1p5()
                        .child(div().size_1p5().rounded_full().bg(swatch.primary))
                        .child(line(0.55, swatch.foreground)),
                )
                .child(div().pl_3().child(line(0.4, swatch.muted)))
                .child(
                    div()
                        .ml_3()
                        .h_3()
                        .w(gpui_kit::relative(0.6))
                        .rounded_sm()
                        .bg(swatch.selection),
                ),
        );
    let hues = h_flex().w_full().h_1p5().children(
        swatch
            .hues
            .iter()
            .map(|hue| div().flex_1().h_full().bg(*hue)),
    );
    gpui_kit::base::Button::new(ElementId::Name(format!("theme-{name}").into()))
        .accessibility_label(name.to_owned())
        .selected(selected)
        .flex()
        .flex_col()
        .items_stretch()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(if selected { theme.ring } else { theme.border })
        .when(selected, |this| this.border_2())
        .overflow_hidden()
        .hover(|this| this.border_color(theme.ring))
        .child(miniature)
        .child(hues)
        .child(
            h_flex()
                .gap_1p5()
                .px_2p5()
                .py_2()
                .text_xs()
                // The card is a base Button, whose line height is the font size: the
                // name's `truncate` would cut off descenders (the p in Catppuccin).
                .line_height(rems(1.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(selected, |this| this.font_weight(FontWeight::SEMIBOLD))
                        .child(name.to_owned()),
                )
                .when(imported, |this| {
                    this.child(crate::layout::badge("Imported", theme.neutral_badge(), cx))
                })
                .when(selected, |this| {
                    this.child(Icon::new(Hero::Check).xsmall().text_color(theme.primary))
                }),
        )
}

/// The 16 ANSI colors of a palette, two rows of eight. These are data, so they are the
/// palette's colors, not the theme's.
fn palette_swatches(palette: &crate::theme::TerminalPalette, cx: &App) -> impl IntoElement + use<> {
    let to_color = |c: crate::theme::Rgb| {
        gpui_kit::Hsla::from(gpui_kit::rgb(
            (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b),
        ))
    };
    let border = cx.theme().border;
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .h_6()
                        .rounded_sm()
                        .border_1()
                        .border_color(border)
                        .bg(to_color(palette.background)),
                )
                .child(
                    div()
                        .flex_1()
                        .h_6()
                        .rounded_sm()
                        .border_1()
                        .border_color(border)
                        .bg(to_color(palette.foreground)),
                ),
        )
        .children(palette.ansi.chunks(8).map(|row| {
            h_flex().gap_1().children(row.iter().map(|&c| {
                div()
                    .flex_1()
                    .h_5()
                    .rounded_sm()
                    .border_1()
                    .border_color(border)
                    .bg(to_color(c))
            }))
        }))
}
