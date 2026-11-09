//! The settings' themes as nsh draws them, and the app's own colors mixed from them.
//!
//! A theme is GPUI Kit's (`catmail_frontend::theme`: built in, or imported into the
//! themes folder); nsh's theme is shadcn's tokens. [`css_vars`] maps the one onto the
//! other, the light theme for the light appearance and the dark one for the dark, as a
//! theme file of shadcn's (`cssVars`) that every view takes over its own
//! (`AppView::set_theme_overrides`). Colors shadcn has no token for are the app's own
//! tokens, by the names below.

use catmail_frontend::theme::ThemeConfig;
use nsh_base::theme;
use nsh_core::Color;
use nsh_core::prelude::*;
use serde_json::{Map, Value, json};

/// shadcn's token, or the app's, and the GPUI Kit key it takes its color from.
const TOKENS: &[(&str, &str)] = &[
    ("background", "background"),
    ("foreground", "foreground"),
    ("card", "background"),
    ("card-foreground", "foreground"),
    ("popover", "popover.background"),
    ("popover-foreground", "popover.foreground"),
    ("primary", "primary.background"),
    ("primary-foreground", "primary.foreground"),
    ("secondary", "secondary.background"),
    ("secondary-foreground", "secondary.foreground"),
    ("muted", "muted.background"),
    ("muted-foreground", "muted.foreground"),
    ("accent", "accent.background"),
    ("accent-foreground", "accent.foreground"),
    ("destructive", "danger.background"),
    ("border", "border"),
    ("input", "input.border"),
    ("ring", "ring"),
    ("chart-1", "chart.1"),
    ("chart-2", "chart.2"),
    ("chart-3", "chart.3"),
    ("chart-4", "chart.4"),
    ("chart-5", "chart.5"),
    ("sidebar", "sidebar.background"),
    ("sidebar-foreground", "sidebar.foreground"),
    ("sidebar-primary", "sidebar.primary.background"),
    ("sidebar-primary-foreground", "sidebar.primary.foreground"),
    ("sidebar-accent", "sidebar.accent.background"),
    ("sidebar-accent-foreground", "sidebar.accent.foreground"),
    ("sidebar-border", "sidebar.border"),
    ("sidebar-ring", "ring"),
    // The app's own. `selection` is also what nsh's components highlight with.
    ("selection", "selection.background"),
    ("list-active", "list.active.background"),
    ("list-hover", "list.hover.background"),
    ("warning", "warning.background"),
    ("success", "success.background"),
    ("info", "info.background"),
    ("link", "link"),
    ("red", "base.red"),
    ("green", "base.green"),
    ("yellow", "base.yellow"),
    ("blue", "base.blue"),
    ("magenta", "base.magenta"),
    ("cyan", "base.cyan"),
];

/// The two themes as a theme file of shadcn's. A color nsh can't read (GPUI Kit's own
/// names, `blue-500`, in a theme file edited by hand) is left out, and the token keeps
/// nsh's default.
pub fn css_vars(light: &ThemeConfig, dark: &ThemeConfig) -> String {
    let vars = |theme: &ThemeConfig| {
        TOKENS
            .iter()
            .filter_map(|&(token, key)| {
                let value = theme.color(key)?;
                Color::parse(value)
                    .is_some()
                    .then(|| (token.to_owned(), json!(value)))
            })
            .collect::<Map<String, Value>>()
    };
    json!({ "cssVars": { "light": vars(light), "dark": vars(dark) } }).to_string()
}

/// One of the app's own tokens, or `fallback` while the theme has none.
fn token(name: &str, fallback: impl FnOnce() -> AdaptiveColor) -> AdaptiveColor {
    theme().color(name).unwrap_or_else(fallback)
}

/// `hue` over the background, light enough for the foreground to read on: dark
/// backgrounds need more of it for the same visible difference.
pub fn tint(hue: AdaptiveColor) -> AdaptiveColor {
    let background = theme().background;
    AdaptiveColor {
        light: background.light.mix_oklab(hue.light, 1.0 - 0.16),
        dark: background.dark.mix_oklab(hue.dark, 1.0 - 0.24),
    }
}

/// A flagged message's flag.
pub fn flag() -> AdaptiveColor {
    warning()
}

/// What needs a look (a delivery not confirmed).
pub fn warning() -> AdaptiveColor {
    token("warning", || theme().destructive)
}

/// A badge for something that needs a look.
pub fn warning_badge() -> AdaptiveColor {
    tint(warning())
}

/// A badge for something that failed.
pub fn danger_badge() -> AdaptiveColor {
    tint(theme().destructive)
}

/// A list's selected row, as GPUI Kit's lists draw it.
pub fn list_active() -> AdaptiveColor {
    token("list-active", || theme().accent)
}

/// A list's row under the pointer.
pub fn list_hover() -> AdaptiveColor {
    token("list-hover", || theme().accent)
}

/// Links in a message.
pub fn link() -> AdaptiveColor {
    token("link", || theme().primary)
}

/// How strongly nsh draws a selectable text's selection over its tint (as iOS does), on
/// Linux and macOS alike.
const SELECTION_TINT_STRENGTH: f32 = 0.3;

/// The highlight of a selection, the same in the app's texts and in the reader's page:
/// the theme's `selection` (what nsh-base's components draw), but no stronger than nsh
/// draws a selectable text's, which can't be.
pub fn selection() -> AdaptiveColor {
    theme().selection().map(highlight)
}

fn highlight(selection: Hsla) -> Hsla {
    selection.alpha(selection.a.min(SELECTION_TINT_STRENGTH))
}

/// The tint that has nsh draw [`selection`] as a selectable text's highlight.
fn selection_tint() -> Hsla {
    tint_for(color(selection()))
}

fn tint_for(highlight: Hsla) -> Hsla {
    highlight.alpha(highlight.a / SELECTION_TINT_STRENGTH)
}

/// A text the user can select, highlighted in [`selection`]: without a color nsh draws
/// the system's blue, whatever the theme.
pub trait SelectableText {
    fn selectable_text(self) -> Self;
}

impl SelectableText for Text {
    fn selectable_text(self) -> Self {
        self.selectable(true).selection_color(selection_tint)
    }
}

/// A sender's avatar: a tint of one of the theme's six hues, picked by the address so a
/// sender keeps theirs.
pub fn avatar(seed: &str) -> AdaptiveColor {
    const HUES: [&str; 6] = ["red", "green", "yellow", "blue", "magenta", "cyan"];
    let hue = HUES[catmail_frontend::list::avatar_hue(seed)];
    tint(token(hue, || theme().primary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use catmail_frontend::theme::{DEFAULT_DARK, DEFAULT_LIGHT, Themes};

    #[test]
    fn selectable_texts_show_the_themes_selection() {
        // As drawn: alpha in 8 bits.
        let alpha = |c: Hsla| (c.a * 255.0).round();
        let close = |a: Hsla, b: Hsla| alpha(a) == alpha(b) && a.h == b.h && a.l == b.l;
        // A theme's own, at 30 % like the built-in ones: drawn as it is.
        let theme_own = Hsla::from(Color::rgba(0xbc, 0xff, 0xff, 0x4d));
        let shown = highlight(theme_own);
        assert!(close(shown, theme_own));
        let drawn = tint_for(shown).opacity(SELECTION_TINT_STRENGTH);
        assert!(close(drawn, shown), "{drawn:?}");
        // An opaque one is as strong as nsh draws.
        let opaque = Hsla::from(Color::rgba(0x26, 0x4f, 0x78, 0xff));
        let shown = highlight(opaque);
        assert!((shown.a - SELECTION_TINT_STRENGTH).abs() < 1e-6);
        assert!((tint_for(shown).a - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_builtin_themes_set_every_token() {
        let themes = Themes::load(None);
        let (light, dark) = (
            themes.find(DEFAULT_LIGHT).unwrap(),
            themes.find(DEFAULT_DARK).unwrap(),
        );
        let file = css_vars(&light.config, &dark.config);
        let tokens = nsh_base::theme_overrides(&file).unwrap();
        for (token, _) in TOKENS {
            let color = tokens.color(token).unwrap_or_else(|| panic!("{token}"));
            assert_ne!(color.light, color.dark, "{token}");
        }
    }

    #[test]
    fn colors_nsh_cant_read_are_left_out() {
        let mut theme = Themes::load(None)
            .find(DEFAULT_LIGHT)
            .unwrap()
            .config
            .clone();
        let theme = std::sync::Arc::make_mut(&mut theme);
        theme.colors.insert("background".into(), json!("blue-500"));
        let file: Value = serde_json::from_str(&css_vars(theme, theme)).unwrap();
        assert!(file["cssVars"]["light"].get("background").is_none());
        assert!(file["cssVars"]["light"].get("foreground").is_some());
    }
}
