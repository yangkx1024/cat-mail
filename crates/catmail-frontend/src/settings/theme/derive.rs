//! A theme from a terminal palette, by the color keys the GUI reads (GPUI Kit's names,
//! which theme files are written in; see `catmail-gui`'s `theme.rs` and [`super::Swatch`]).
//!
//! A color scheme gives a background, a foreground and 16 ANSI colors; the app needs a
//! few dozen colors. Every one it reads is derived here, so none falls back to nsh's
//! (neutral grey) defaults and clashes with the scheme. Other keys aren't written; a
//! theme file that has them (an older one, or GPUI Kit's own) still loads, and they are
//! ignored. The rules are the design's "Terminal palette → UI roles" board:
//!
//! - surfaces and borders are the background mixed a little toward the foreground;
//! - the accent is the blue slot, the status colors red, green, yellow and cyan, each
//!   the normal or bright variant, whichever first reads at 3:1 on the background;
//! - secondary text is raised to 4.5:1 (or the foreground's own contrast, if lower).
//!
//! Mixing is in gamma-encoded sRGB, like CSS `color-mix(in srgb, …)`.

use serde_json::{Map, Value};

use super::palette::{Rgb, TerminalPalette};
use super::{ThemeConfig, ThemeMode, ThemeSet};

/// The colors the derivation starts from, before they are spread over the keys.
/// Kept separate so tests can check the contrast guarantees.
#[derive(Debug, Clone, Copy)]
pub(super) struct Roles {
    pub dark: bool,
    pub background: Rgb,
    pub foreground: Rgb,
    pub sidebar: Rgb,
    pub popover: Rgb,
    /// Hover and muted surfaces.
    pub hover: Rgb,
    pub border: Rgb,
    pub input: Rgb,
    pub muted_foreground: Rgb,
    pub primary: Rgb,
    pub on_primary: Rgb,
    /// The selected sidebar item: neutral, so the accent stays on the message list.
    pub sidebar_selected: Rgb,
    pub danger: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub info: Rgb,
    /// For charts only: magenta has no meaning in the UI.
    pub magenta: Rgb,
    pub link: Rgb,
}

impl Roles {
    pub fn new(palette: &TerminalPalette) -> Self {
        let (bg, fg, ansi) = (palette.background, palette.foreground, &palette.ansi);
        let dark = palette.is_dark();
        let ensure = |color: Rgb, on: Rgb, min: f64| raise(color, fg, on, min);
        // The normal or bright variant of a hue: dark schemes try bright first. Some
        // schemes make one of the two nearly the background, hence the 3:1 check rather
        // than a fixed choice; Solarized's "bright" slots are its greys, so a grey is
        // only taken when the hue itself doesn't read.
        let choose = |n: usize| {
            let order = if dark {
                [ansi[n + 8], ansi[n]]
            } else {
                [ansi[n], ansi[n + 8]]
            };
            let readable = |c: &Rgb| c.contrast(bg) >= 3.0;
            order
                .into_iter()
                .find(|c| readable(c) && !is_grey(*c))
                .or_else(|| order.into_iter().find(readable))
                .unwrap_or_else(|| ensure(order[0], bg, 3.0))
        };
        let primary = choose(4);
        let sidebar = bg.mix(fg, 0.035);
        Self {
            dark,
            background: bg,
            foreground: fg,
            sidebar,
            popover: if dark { bg.mix(fg, 0.05) } else { bg },
            hover: bg.mix(fg, 0.07),
            border: bg.mix(fg, if dark { 0.13 } else { 0.11 }),
            input: bg.mix(fg, if dark { 0.2 } else { 0.17 }),
            muted_foreground: ensure(fg.mix(bg, 0.42), bg, fg.contrast(bg).min(4.5)),
            primary,
            on_primary: on(primary, bg, fg),
            sidebar_selected: sidebar.mix(fg, if dark { 0.1 } else { 0.07 }),
            danger: choose(1),
            success: choose(2),
            warning: choose(3),
            info: choose(6),
            magenta: choose(5),
            link: ensure(primary, bg, 4.5),
        }
    }
}

/// `color` moved toward the foreground until it reads at `min` on `on`; the foreground
/// itself if nothing short of it does. In twentieths, as the design board steps.
fn raise(color: Rgb, fg: Rgb, on: Rgb, min: f64) -> Rgb {
    (0..=20)
        .map(|step| color.mix(fg, f64::from(step) / 20.0))
        .find(|c| c.contrast(on) >= min)
        .unwrap_or(fg)
}

/// Too little chroma to stand for a hue. Solarized's grey "bright" slots spread 14 to 30
/// between channels; the palest real accents (Gruvbox's bright blue, Nord's frost)
/// spread more than 32.
fn is_grey(c: Rgb) -> bool {
    let max = c.r.max(c.g).max(c.b);
    let min = c.r.min(c.g).min(c.b);
    max - min < 32
}

/// Of the background and foreground, the one that reads better on `color`.
fn on(color: Rgb, bg: Rgb, fg: Rgb) -> Rgb {
    if color.contrast(bg) >= color.contrast(fg) {
        bg
    } else {
        fg
    }
}

/// `color` at `alpha`, drawn over whatever is under it: a selected row keeps the
/// sidebar's or the list's surface showing through.
fn translucent(color: Rgb, alpha: f64) -> String {
    format!("{}{:02x}", color.to_hex(), (alpha * 255.0).round() as u8)
}

/// The translucent color that, drawn at `alpha` over `bg`, gives `target`: a scheme's
/// opaque selection color survives the 30% the GUI draws a selection at, at most.
fn over(bg: Rgb, target: Rgb, alpha: f64) -> String {
    let channel = |b: u8, t: u8| {
        let (b, t) = (f64::from(b), f64::from(t));
        (b + (t - b) / alpha).round().clamp(0.0, 255.0) as u8
    };
    let color = Rgb::new(
        channel(bg.r, target.r),
        channel(bg.g, target.g),
        channel(bg.b, target.b),
    );
    translucent(color, alpha)
}

/// The theme for the palette, with every key the GUI reads set.
pub fn theme_config(palette: &TerminalPalette) -> ThemeConfig {
    ThemeConfig {
        name: palette.name.clone(),
        mode: if palette.is_dark() {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        colors: colors(palette),
        rest: Map::new(),
    }
}

fn colors(palette: &TerminalPalette) -> Map<String, Value> {
    let r = Roles::new(palette);
    let (bg, fg, ansi) = (r.background, r.foreground, &palette.ansi);
    let secondary = bg.mix(fg, if r.dark { 0.1 } else { 0.08 });
    let selected_alpha = if r.dark { 0.2 } else { 0.11 };

    let mut colors = Map::new();
    let mut set = |key: &str, value: String| {
        colors.insert(key.to_owned(), Value::String(value));
    };
    let hex = Rgb::to_hex;

    set("background", hex(bg));
    set("foreground", hex(fg));
    set("border", hex(r.border));
    set("input.border", hex(r.input));
    set("ring", hex(r.primary));
    set(
        "selection.background",
        match palette.selection_background {
            Some(selection) => over(bg, selection, 0.3),
            None => translucent(r.primary, 0.3),
        },
    );

    set("accent.background", hex(r.hover));
    set("accent.foreground", hex(fg));
    set("muted.background", hex(r.hover));
    set("muted.foreground", hex(r.muted_foreground));
    set("popover.background", hex(r.popover));
    set("popover.foreground", hex(fg));

    set("primary.background", hex(r.primary));
    set("primary.foreground", hex(r.on_primary));
    set("secondary.background", hex(secondary));
    set("secondary.foreground", hex(fg));

    set("danger.background", hex(r.danger));
    set("success.background", hex(r.success));
    set("warning.background", hex(r.warning));
    set("info.background", hex(r.info));
    set("link", hex(r.link));

    set(
        "list.active.background",
        translucent(r.primary, selected_alpha),
    );
    set("list.hover.background", hex(r.hover));

    set("sidebar.background", hex(r.sidebar));
    set("sidebar.foreground", hex(fg));
    set("sidebar.border", hex(r.border));
    set("sidebar.accent.background", hex(r.sidebar_selected));
    set("sidebar.accent.foreground", hex(fg));
    set("sidebar.primary.background", hex(r.primary));
    set("sidebar.primary.foreground", hex(r.on_primary));

    set("chart.1", hex(r.primary));
    set("chart.2", hex(r.success));
    set("chart.3", hex(r.warning));
    set("chart.4", hex(r.magenta));
    set("chart.5", hex(r.info));

    // The raw ANSI hues, for whatever wants the scheme's own colors (avatars).
    for (n, name) in ["red", "green", "yellow", "blue", "magenta", "cyan"]
        .into_iter()
        .enumerate()
    {
        set(&format!("base.{name}"), hex(ansi[n + 1]));
    }
    colors
}

/// A theme file for the themes folder: pretty JSON of a GPUI Kit theme set, without
/// the unset (null) fields, so it reads well when edited by hand.
pub fn theme_set_json(configs: &[ThemeConfig]) -> String {
    let set = ThemeSet {
        name: configs.first().map(|c| c.name.clone()).unwrap_or_default(),
        themes: configs.to_vec(),
        rest: Map::new(),
    };
    let mut value = serde_json::to_value(&set).expect("a theme set serializes");
    strip_nulls(&mut value);
    serde_json::to_string_pretty(&value).expect("JSON serializes")
}

fn strip_nulls(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            map.values_mut().for_each(strip_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::theme::builtin;

    fn palettes() -> Vec<TerminalPalette> {
        builtin::palettes()
    }

    #[test]
    fn contrast_guarantees_hold_for_every_builtin() {
        for palette in palettes() {
            let r = Roles::new(&palette);
            let name = &palette.name;
            let fg_contrast = r.foreground.contrast(r.background);
            assert!(
                r.muted_foreground.contrast(r.background) >= fg_contrast.min(4.5) - 1e-9,
                "{name}: secondary text"
            );
            assert!(r.primary.contrast(r.background) >= 3.0, "{name}: accent");
            for status in [r.danger, r.success, r.warning, r.info] {
                assert!(status.contrast(r.background) >= 3.0, "{name}: {status}");
            }
            assert!(
                r.link.contrast(r.background) >= fg_contrast.min(4.5) - 1e-9,
                "{name}: link"
            );
            // Text on the accent: whichever of background and foreground reads better.
            let best = r
                .primary
                .contrast(r.background)
                .max(r.primary.contrast(r.foreground));
            assert_eq!(r.on_primary.contrast(r.primary), best, "{name}");
            assert!(best >= 3.0, "{name}: text on the accent");
        }
    }

    #[test]
    fn mode_follows_the_palette() {
        for palette in palettes() {
            let config = theme_config(&palette);
            assert_eq!(config.is_dark(), palette.is_dark(), "{}", palette.name);
            assert_eq!(config.name, palette.name);
        }
        let mocha = palettes()
            .into_iter()
            .find(|p| p.name == "Catppuccin Mocha")
            .unwrap();
        assert!(mocha.is_dark());
    }

    #[test]
    fn theme_files_round_trip() {
        let configs: Vec<_> = palettes().iter().map(theme_config).collect();
        let json = theme_set_json(&configs);
        assert!(!json.contains("null"));
        let set: ThemeSet = serde_json::from_str(&json).unwrap();
        assert_eq!(set.themes, configs);
    }

    #[test]
    fn an_opaque_selection_color_is_kept_under_the_opacity_cap() {
        let bg = Rgb::new(0x1f, 0x1f, 0x28);
        let selection = Rgb::new(0x2d, 0x4f, 0x67);
        let value = over(bg, selection, 0.3);
        let drawn = Rgb::parse(&value[..7]).unwrap();
        assert_eq!(&value[7..], "4d");
        // Drawn at 30% over the background, it lands on the scheme's color.
        let composited = bg.mix(drawn, 0.3);
        assert!(
            composited.contrast(selection) < 1.02,
            "{composited} vs {selection}"
        );
    }
}
