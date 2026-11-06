//! A theme drawn small, as Settings › Appearance shows each: the colors its card takes,
//! with what stands in for those the theme doesn't set.

use super::{Rgb, ThemeConfig};

/// A color with its opacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub rgb: Rgb,
    pub alpha: u8,
}

impl Rgba {
    pub const fn opaque(rgb: Rgb) -> Self {
        Self { rgb, alpha: 255 }
    }

    /// As [`Rgb::parse`], keeping a trailing alpha (`#rrggbbaa`), as the theme files write
    /// translucent colors.
    pub fn parse(s: &str) -> Option<Self> {
        let (rgb, alpha) = Rgb::parse_with_alpha(s)?;
        let alpha = alpha.unwrap_or(255);
        Some(Self { rgb, alpha })
    }

    /// The color at `factor` of its opacity.
    pub fn opacity(self, factor: f32) -> Self {
        let alpha = (f32::from(self.alpha) * factor.clamp(0.0, 1.0)).round() as u8;
        Self { alpha, ..self }
    }
}

/// The colors of a theme's card: a sidebar beside the list, a line of text, a dim one and
/// a selected row, over a strip of the theme's hues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swatch {
    pub background: Rgba,
    pub foreground: Rgba,
    pub primary: Rgba,
    pub sidebar: Rgba,
    pub border: Rgba,
    pub muted: Rgba,
    pub selection: Rgba,
    /// Danger, success, warning, primary and info, left to right.
    pub hues: [Rgba; 5],
}

impl Swatch {
    /// What isn't set (or can't be read) is drawn from what is: the background is white
    /// and the text black, the rest follow them and the primary color.
    pub fn of(theme: &ThemeConfig) -> Self {
        let color = |key: &str| theme.color(key).and_then(Rgba::parse);
        let or = |key: &str, fallback: Rgba| color(key).unwrap_or(fallback);
        let background = or("background", Rgba::opaque(Rgb::WHITE));
        let foreground = or("foreground", Rgba::opaque(Rgb::BLACK));
        let primary = or("primary.background", foreground);
        Self {
            background,
            foreground,
            primary,
            sidebar: or("sidebar.background", background),
            border: or("border", foreground.opacity(0.15)),
            muted: or("muted.foreground", foreground.opacity(0.6)),
            selection: or("list.active.background", primary.opacity(0.15)),
            hues: [
                or("danger.background", primary),
                or("success.background", primary),
                or("warning.background", primary),
                primary,
                or("info.background", primary),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn theme(colors: serde_json::Value) -> ThemeConfig {
        ThemeConfig {
            colors: colors.as_object().unwrap().clone(),
            ..ThemeConfig::default()
        }
    }

    fn rgba(hex: &str) -> Rgba {
        Rgba::parse(hex).unwrap()
    }

    #[test]
    fn what_a_theme_leaves_out_follows_what_it_sets() {
        let swatch = Swatch::of(&theme(json!({
            "background": "#202020",
            "foreground": "#f0f0f0",
            "primary.background": "#3070ff",
            "warning.background": "#ffaa00",
            // Not a color the card can read: as if left out.
            "danger.background": "red",
        })));
        let primary = rgba("#3070ff");
        assert_eq!(swatch.sidebar, rgba("#202020"));
        assert_eq!(swatch.border, rgba("#f0f0f026"));
        assert_eq!(swatch.muted, rgba("#f0f0f099"));
        assert_eq!(swatch.selection, rgba("#3070ff26"));
        assert_eq!(
            swatch.hues,
            [primary, primary, rgba("#ffaa00"), primary, primary]
        );
    }

    #[test]
    fn a_theme_with_no_colors_is_black_on_white() {
        let swatch = Swatch::of(&ThemeConfig::default());
        assert_eq!(swatch.background, Rgba::opaque(Rgb::WHITE));
        assert_eq!(swatch.foreground, Rgba::opaque(Rgb::BLACK));
        assert_eq!(swatch.primary, swatch.foreground);
    }

    #[test]
    fn colors_keep_their_opacity() {
        assert_eq!(rgba("#10203080").alpha, 0x80);
        assert_eq!(rgba("#102030").alpha, 255);
        assert_eq!(rgba("#10203080").rgb, Rgb::new(0x10, 0x20, 0x30));
        assert_eq!(rgba("#102030").opacity(0.5).alpha, 128);
        assert_eq!(rgba("0x10203080").alpha, 0x80);
    }
}
