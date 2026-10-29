//! A message's own colors moved for a dark page, as Gmail and Outlook show mail written
//! for white.
//!
//! Colors are moved in OKLab, where lightness is what the eye sees: hue and saturation
//! stay, so brand colors are still recognizably theirs. The move inverts lightness between
//! the reader's colors, white going to its background and black to its text. What a color
//! is used for decides whether it moves: a background only ever gets darker and text only
//! lighter, so text stays readable on its background, and a header already dark with
//! white text is left as it is. Lines (borders) take the inversion either way, keeping
//! their contrast with the page around them.
//!
//! The reader's colors are closer together than white and black, so an inversion between
//! them leaves less contrast than the message had. Text is inverted over a shorter range,
//! toward its end: gray text, readable on white, would otherwise come out dim.

use cssparser::color::{parse_hash_color, parse_named_color};

/// How much of the range from the reader's text to its background text is inverted over.
const TEXT_RANGE: f32 = 0.6;

/// The reader's colors that a message's own are moved toward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DarkPalette {
    /// The page's, as sRGB: where white goes.
    pub background: [u8; 3],
    /// The text's: where black goes.
    pub foreground: [u8; 3],
}

/// What a color is used for, which decides how it moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// Text and its decorations: only made lighter.
    Text,
    /// Backgrounds and shadows: only made darker.
    Surface,
    /// Borders, outlines, and colors whose use isn't known: inverted.
    Line,
}

impl Role {
    /// The role of the colors in a CSS property's value, or `None` for a property that
    /// takes none: its identifiers aren't colors (`font-family: Tomato`).
    pub(super) fn of(property: &str) -> Option<Self> {
        let property = property.to_ascii_lowercase();
        let role = match property.as_str() {
            "color"
            | "-webkit-text-fill-color"
            | "-webkit-text-stroke"
            | "-webkit-text-stroke-color"
            | "text-decoration"
            | "text-decoration-color"
            | "text-emphasis"
            | "text-emphasis-color"
            | "caret-color" => Role::Text,
            "background" | "background-color" | "background-image" | "box-shadow"
            | "text-shadow" => Role::Surface,
            "outline" | "outline-color" | "column-rule" | "column-rule-color" | "fill"
            | "stroke" => Role::Line,
            // Custom properties may hold a color for anything.
            p if p.starts_with("border") || p.starts_with("--") => Role::Line,
            _ => return None,
        };
        Some(role)
    }
}

/// An sRGB color with its alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rgba {
    pub(super) rgb: [u8; 3],
    pub(super) alpha: f32,
}

impl Rgba {
    pub(super) fn opaque(rgb: [u8; 3]) -> Self {
        Self { rgb, alpha: 1.0 }
    }
}

impl DarkPalette {
    /// `color` moved for a dark page, as used for `role`.
    pub(super) fn recolor(&self, color: Rgba, role: Role) -> Rgba {
        let lab = Lab::from_srgb(color.rgb);
        let (background, foreground) = (
            Lab::from_srgb(self.background),
            Lab::from_srgb(self.foreground),
        );
        // White to the background and black to the text, grays tinted as the two are;
        // a color keeps its own chroma on top.
        let t = match role {
            Role::Text => lab.l * TEXT_RANGE,
            Role::Surface | Role::Line => lab.l,
        };
        let inverted = Lab {
            l: foreground.l + t * (background.l - foreground.l),
            a: lab.a + foreground.a + t * (background.a - foreground.a),
            b: lab.b + foreground.b + t * (background.b - foreground.b),
        };
        let moved = match role {
            Role::Text => inverted.l > lab.l,
            Role::Surface => inverted.l < lab.l,
            Role::Line => true,
        };
        if !moved {
            return color;
        }
        Rgba {
            rgb: inverted.to_srgb(),
            alpha: color.alpha,
        }
    }

    /// A legacy color attribute (`bgcolor`, `<font color>`) moved for a dark page, as
    /// `#rrggbb`; `None` for a value that isn't a color.
    pub(super) fn recolor_attribute(&self, value: &str, role: Role) -> Option<String> {
        let rgb = legacy_color(value)?;
        let [r, g, b] = self.recolor(Rgba::opaque(rgb), role).rgb;
        Some(format!("#{r:02x}{g:02x}{b:02x}"))
    }
}

/// A legacy color attribute's value: a color name, or hex digits with or without the
/// `#` that mail often leaves out. The HTML standard's rules accept more (any text,
/// digits read in odd ways); a value only they could read is left as it is.
fn legacy_color(value: &str) -> Option<[u8; 3]> {
    let value = value.trim();
    if let Ok((r, g, b)) = parse_named_color(value) {
        return Some([r, g, b]);
    }
    let hex = value.strip_prefix('#').unwrap_or(value);
    if !matches!(hex.len(), 3 | 6) {
        return None;
    }
    let (r, g, b, _) = parse_hash_color(hex.as_bytes()).ok()?;
    Some([r, g, b])
}

/// A color in OKLab.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lab {
    l: f32,
    a: f32,
    b: f32,
}

impl Lab {
    fn from_srgb(rgb: [u8; 3]) -> Self {
        let [r, g, b] = rgb.map(|c| to_linear(f32::from(c) / 255.0));
        let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
        let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
        let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
        Self {
            l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
            a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
            b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
        }
    }

    fn to_linear_srgb(self) -> [f32; 3] {
        let l = (self.l + 0.396_337_78 * self.a + 0.215_803_76 * self.b).powi(3);
        let m = (self.l - 0.105_561_346 * self.a - 0.063_854_17 * self.b).powi(3);
        let s = (self.l - 0.089_484_18 * self.a - 1.291_485_5 * self.b).powi(3);
        [
            4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
            -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
            -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
        ]
    }

    /// In sRGB, with less chroma if it has more than sRGB can show at its lightness: a
    /// light pastel made dark would otherwise clip to another hue.
    fn to_srgb(self) -> [u8; 3] {
        let l = self.l.clamp(0.0, 1.0);
        let fits = |k: f32| {
            let lab = Lab {
                l,
                a: self.a * k,
                b: self.b * k,
            };
            lab.to_linear_srgb()
                .iter()
                .all(|c| (-1e-4..=1.0 + 1e-4).contains(c))
        };
        let mut k = 1.0;
        if !fits(k) {
            let (mut lo, mut hi) = (0.0, 1.0);
            for _ in 0..16 {
                let mid = (lo + hi) / 2.0;
                if fits(mid) { lo = mid } else { hi = mid }
            }
            k = lo;
        }
        let lab = Lab {
            l,
            a: self.a * k,
            b: self.b * k,
        };
        lab.to_linear_srgb()
            .map(|c| (from_linear(c.clamp(0.0, 1.0)) * 255.0).round() as u8)
    }
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PALETTE: DarkPalette = DarkPalette {
        background: [0x1e, 0x1f, 0x24],
        foreground: [0xe0, 0xe2, 0xe8],
    };

    fn moved(rgb: [u8; 3], role: Role) -> [u8; 3] {
        PALETTE.recolor(Rgba::opaque(rgb), role).rgb
    }

    fn close(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(&x, y)| x.abs_diff(y) <= 1)
    }

    fn lightness(rgb: [u8; 3]) -> f32 {
        Lab::from_srgb(rgb).l
    }

    #[test]
    fn white_and_black_go_to_the_readers_colors() {
        for role in [Role::Surface, Role::Line] {
            assert!(close(moved([255; 3], role), PALETTE.background), "{role:?}");
        }
        for role in [Role::Text, Role::Line] {
            assert!(close(moved([0; 3], role), PALETTE.foreground), "{role:?}");
        }
    }

    #[test]
    fn backgrounds_only_darken_and_text_only_lightens() {
        // A dark header and its white text are left as they are.
        assert_eq!(moved([0x22, 0x33, 0x44], Role::Surface), [0x22, 0x33, 0x44]);
        assert_eq!(moved([255; 3], Role::Text), [255; 3]);
        assert_eq!(moved([0xaa; 3], Role::Text), [0xaa; 3]);
        // Light ones made dark, and dark text light.
        for rgb in [[0xf5; 3], [0xff, 0xf3, 0xcd], [0xff, 0x66, 0x00], [0x88; 3]] {
            let after = moved(rgb, Role::Surface);
            assert!(lightness(after) < lightness(rgb), "{rgb:?} → {after:?}");
            assert!(lightness(after) < 0.55, "{rgb:?} → {after:?}");
        }
        for rgb in [[0x33; 3], [0x1a, 0x0d, 0xab], [0x66; 3]] {
            let after = moved(rgb, Role::Text);
            assert!(lightness(after) > 0.6, "{rgb:?} → {after:?}");
        }
        // A light border stays a faint line on the page; a dark one a strong one.
        let faint = lightness(moved([0xe5; 3], Role::Line));
        let page = lightness(PALETTE.background);
        assert!(faint > page && faint < page + 0.1, "{faint}");
        assert!(lightness(moved([0x33; 3], Role::Line)) > 0.6);
    }

    #[test]
    fn a_color_keeps_its_hue_and_alpha() {
        let hue = |rgb: [u8; 3]| {
            let lab = Lab::from_srgb(rgb);
            lab.b.atan2(lab.a).to_degrees()
        };
        for rgb in [[0xff, 0x66, 0x00], [0xff, 0xeb, 0x3b], [0x1a, 0x73, 0xe8]] {
            for role in [Role::Text, Role::Surface, Role::Line] {
                let after = moved(rgb, role);
                if after != rgb {
                    let turned = (hue(after) - hue(rgb)).abs();
                    assert!(turned < 12.0, "{rgb:?} as {role:?} → {after:?}: {turned}°");
                }
            }
        }
        let shadow = PALETTE.recolor(
            Rgba {
                rgb: [0xff; 3],
                alpha: 0.25,
            },
            Role::Surface,
        );
        assert_eq!(shadow.alpha, 0.25);
    }

    #[test]
    fn reads_legacy_color_attributes() {
        assert_eq!(legacy_color(" #FFF "), Some([255; 3]));
        assert_eq!(legacy_color("ffffff"), Some([255; 3]));
        assert_eq!(legacy_color("White"), Some([255; 3]));
        assert_eq!(legacy_color("#12345"), None);
        assert_eq!(legacy_color("transparent"), None);
        let white = PALETTE.recolor_attribute("#ffffff", Role::Surface).unwrap();
        assert!(
            close(legacy_color(&white).unwrap(), PALETTE.background),
            "{white}"
        );
        assert_eq!(PALETTE.recolor_attribute("none", Role::Surface), None);
    }

    #[test]
    fn properties_take_the_role_of_their_colors() {
        assert_eq!(Role::of("COLOR"), Some(Role::Text));
        assert_eq!(Role::of("background"), Some(Role::Surface));
        assert_eq!(Role::of("border-bottom-color"), Some(Role::Line));
        assert_eq!(Role::of("--brand"), Some(Role::Line));
        assert_eq!(Role::of("font-family"), None);
    }
}
