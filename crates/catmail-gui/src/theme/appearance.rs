//! The themes on offer and the one in effect. Which themes there are, and importing one,
//! are `catmail_frontend::theme`'s ([`OfferedThemes`]); here they become GPUI Kit themes,
//! kept here rather than in GPUI Kit's `ThemeRegistry`: a re-imported theme replaces the
//! old one under the same name, which the registry can't do.

use std::path::Path;
use std::rc::Rc;

use catmail_frontend::theme::{self as offered, ImportedTheme, Themes as OfferedThemes};
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, try_parse_color};
use gpui_kit::{App, Global, Hsla, Window, WindowAppearance, px};

use crate::settings::{Appearance, Preferences};
use crate::theme::{DEFAULT_DARK, DEFAULT_LIGHT};

/// A theme the settings offer.
#[derive(Clone)]
pub struct ThemeEntry {
    pub config: Rc<ThemeConfig>,
    imported: bool,
    pub swatch: Swatch,
}

impl ThemeEntry {
    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn is_dark(&self) -> bool {
        self.config.mode.is_dark()
    }

    pub fn is_imported(&self) -> bool {
        self.imported
    }
}

/// The GPUI Kit theme of one the frontend offers; `None` (with a warning) if GPUI Kit
/// can't read it, a theme file edited by hand, say.
pub fn gpui_theme(config: &offered::ThemeConfig) -> Option<ThemeConfig> {
    serde_json::to_value(config)
        .and_then(serde_json::from_value)
        .inspect_err(|e| tracing::warn!(theme = config.name, error = %e, "ignoring theme"))
        .ok()
}

/// The colors a theme card draws its miniature with. They are the theme's own, the card
/// being a picture of it.
#[derive(Clone, Copy)]
pub struct Swatch {
    pub background: Hsla,
    pub foreground: Hsla,
    pub sidebar: Hsla,
    pub border: Hsla,
    pub muted: Hsla,
    pub primary: Hsla,
    pub selection: Hsla,
    pub hues: [Hsla; 5],
}

impl Swatch {
    fn new(config: &ThemeConfig) -> Self {
        let colors = &config.colors;
        let color = |value: &Option<gpui_kit::SharedString>, fallback: Hsla| {
            value
                .as_deref()
                .and_then(|v| try_parse_color(v).ok())
                .unwrap_or(fallback)
        };
        let background = color(&colors.background, gpui_kit::white());
        let foreground = color(&colors.foreground, gpui_kit::black());
        let primary = color(&colors.primary, foreground);
        Self {
            background,
            foreground,
            sidebar: color(&colors.sidebar, background),
            border: color(&colors.border, foreground.opacity(0.15)),
            muted: color(&colors.muted_foreground, foreground.opacity(0.6)),
            primary,
            selection: color(&colors.list_active, primary.opacity(0.15)),
            hues: [
                color(&colors.danger, primary),
                color(&colors.success, primary),
                color(&colors.warning, primary),
                primary,
                color(&colors.info, primary),
            ],
        }
    }
}

/// Every theme on offer: the built-in ones, then the imported ones.
#[derive(Default)]
pub struct Themes {
    offered: OfferedThemes,
    entries: Vec<ThemeEntry>,
    /// The desktop's appearance as a window last told it; the app-wide one isn't
    /// reliable on Linux.
    system: Option<WindowAppearance>,
}

impl Global for Themes {}

impl Themes {
    pub fn entries(cx: &App) -> &[ThemeEntry] {
        &cx.global::<Self>().entries
    }

    fn new(offered: OfferedThemes) -> Self {
        let mut themes = Self {
            offered,
            ..Default::default()
        };
        themes.convert();
        themes
    }

    /// The offered themes as GPUI Kit's, once they change.
    fn convert(&mut self) {
        self.entries = self
            .offered
            .entries()
            .iter()
            .filter_map(|entry| {
                let config = gpui_theme(&entry.config)?;
                Some(ThemeEntry {
                    swatch: Swatch::new(&config),
                    config: Rc::new(config),
                    imported: entry.is_imported(),
                })
            })
            .collect();
    }

    fn find(&self, name: &str) -> Option<&ThemeEntry> {
        self.entries.iter().find(|e| e.name() == name)
    }
}

/// Loads the themes and applies the settings. Before any window opens.
pub fn init(cx: &mut App) {
    let dir = Preferences::themes_dir(cx);
    cx.set_global(Themes::new(OfferedThemes::load(dir.as_deref())));
    // Whatever changes the settings, they take effect.
    cx.observe_global::<Preferences>(apply).detach();
    apply(cx);
}

/// The desktop's light or dark appearance, whatever the app's own theme is set to.
pub fn system(cx: &App) -> WindowAppearance {
    cx.global::<Themes>()
        .system
        .unwrap_or_else(|| cx.window_appearance())
}

/// Follows the desktop's light or dark appearance as the window sees it, for "System".
pub fn system_appearance_changed(window: &Window, cx: &mut App) {
    cx.global_mut::<Themes>().system = Some(window.appearance());
    apply(cx);
}

/// Puts the settings' theme and text size into effect.
fn apply(cx: &mut App) {
    let settings = Preferences::get(cx).clone();
    let themes = cx.global::<Themes>();
    let pick = |name: &str, fallback: &str| {
        themes
            .find(name)
            .or_else(|| themes.find(fallback))
            .map(|e| Rc::new(crate::fonts::with_ui_font(&e.config)))
    };
    let (Some(light), Some(dark)) = (
        pick(&settings.light_theme, DEFAULT_LIGHT),
        pick(&settings.dark_theme, DEFAULT_DARK),
    ) else {
        return;
    };
    let mode = match settings.appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
        Appearance::System => system(cx).into(),
    };
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
        theme.font_size = px(settings.text_size);
    });
    // Loads the mode's theme even if the mode is the same: the theme may not be.
    Theme::change(mode, None, cx);
}

/// Writes the imported themes into the themes folder and offers them (see
/// [`OfferedThemes::install`]). Returns the names they are offered under.
pub fn install(themes: Vec<ImportedTheme>, cx: &mut App) -> Result<Vec<String>, String> {
    let dir = Preferences::themes_dir(cx).ok_or("There is no folder to keep themes in.")?;
    cx.global_mut::<Themes>().install(&dir, themes)
}

impl Themes {
    fn install(&mut self, dir: &Path, themes: Vec<ImportedTheme>) -> Result<Vec<String>, String> {
        let installed = self.offered.install(dir, themes);
        // Those written before one failed are offered too.
        self.convert();
        installed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use catmail_frontend::theme::{builtin_themes, theme_set_json};

    #[test]
    fn every_color_key_is_set() {
        for theme in builtin_themes() {
            let config = gpui_theme(&theme).unwrap();
            let colors = serde_json::to_value(&config.colors).unwrap();
            let colors = colors.as_object().unwrap();
            let unset: Vec<_> = colors
                .iter()
                .filter(|(_, v)| v.is_null())
                .map(|(k, _)| k.as_str())
                .collect();
            assert!(unset.is_empty(), "{}: {unset:?}", theme.name);
            // And nothing derived was dropped as an unknown key.
            assert_eq!(colors.len(), theme.colors.len(), "{}", theme.name);
        }
    }

    #[test]
    fn theme_files_round_trip_through_gpui_kit() {
        let configs = builtin_themes();
        let json = theme_set_json(&configs);
        let set: gpui_kit::component::ThemeSet = serde_json::from_str(&json).unwrap();
        assert_eq!(set.themes.len(), configs.len());
        for (read, written) in set.themes.iter().zip(&configs) {
            assert_eq!(read.name, written.name);
            assert_eq!(read.mode.is_dark(), written.is_dark());
            assert_eq!(
                serde_json::to_value(&read.colors).unwrap(),
                serde_json::Value::Object(written.colors.clone())
            );
        }
    }

    #[test]
    fn a_theme_gpui_kit_cant_read_is_left_out() {
        let mut theme = builtin_themes().remove(0);
        theme.rest.insert("radius".into(), "round".into());
        assert!(gpui_theme(&theme).is_none());
    }
}
