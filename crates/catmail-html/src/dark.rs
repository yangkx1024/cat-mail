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

use color::{AlphaColor, Oklab, Srgb};

/// A color as CSS has it: sRGB with alpha.
pub(crate) type Color = AlphaColor<Srgb>;

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
pub(crate) enum Role {
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
    pub(crate) fn of(property: &str) -> Option<Self> {
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
            "outline" | "outline-color" | "column-rule" | "column-rule-color" => Role::Line,
            p if p.starts_with("border") => Role::Line,
            _ => return None,
        };
        Some(role)
    }
}

impl DarkPalette {
    /// `color` moved for a dark page, as used for `role`.
    pub(crate) fn recolor(&self, color: Color, role: Role) -> Color {
        let [l, a, b, alpha] = color.convert::<Oklab>().components;
        let [background, foreground] =
            [self.background, self.foreground].map(|[r, g, b]| oklab(r, g, b));
        // White to the background and black to the text, grays tinted as the two are;
        // a color keeps its own chroma on top.
        let t = match role {
            Role::Text => l * TEXT_RANGE,
            Role::Surface | Role::Line => l,
        };
        let [inverted, tint_a, tint_b] =
            [0, 1, 2].map(|i| foreground[i] + t * (background[i] - foreground[i]));
        let moved = match role {
            Role::Text => inverted > l,
            Role::Surface => inverted < l,
            Role::Line => true,
        };
        if !moved {
            return color;
        }
        in_gamut(inverted.clamp(0.0, 1.0), a + tint_a, b + tint_b, alpha)
    }

    /// A legacy color attribute (`bgcolor`, `<font color>`) moved for a dark page, as
    /// `#rrggbb`; `None` for a value that isn't a color.
    pub(crate) fn recolor_attribute(&self, value: &str, role: Role) -> Option<String> {
        Some(format!(
            "#{}",
            hex(self.recolor(legacy_color(value)?, role))
        ))
    }
}

/// A color written in CSS (a hex color, a name, `rgb()`, `hsl()`, `oklch()`, …), unless
/// it is fully transparent: nothing to move.
pub(crate) fn parse(css: &str) -> Option<Color> {
    let color = color::parse_color(css).ok()?.to_alpha_color::<Srgb>();
    (color.components[3] > 0.0).then_some(color)
}

/// The digits of `#` notation: `rrggbb`, or `rrggbbaa` if not opaque.
pub(crate) fn hex(color: Color) -> String {
    let rgba = color.to_rgba8();
    let (r, g, b, a) = (rgba.r, rgba.g, rgba.b, rgba.a);
    if a == 255 {
        format!("{r:02x}{g:02x}{b:02x}")
    } else {
        format!("{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

/// A legacy color attribute's value: a color name, or hex digits with or without the
/// `#` that mail often leaves out. The HTML standard's rules accept more (any text,
/// digits read in odd ways); a value only they could read is left as it is.
fn legacy_color(value: &str) -> Option<Color> {
    let value = value.trim();
    let bare_hex = matches!(value.len(), 3 | 6) && value.bytes().all(|b| b.is_ascii_hexdigit());
    if bare_hex {
        parse(&format!("#{value}"))
    } else {
        parse(value)
    }
}

fn oklab(r: u8, g: u8, b: u8) -> [f32; 3] {
    let [l, a, b, _] = Color::from_rgba8(r, g, b, 255)
        .convert::<Oklab>()
        .components;
    [l, a, b]
}

/// The OKLab color in sRGB, with less chroma if it has more than sRGB can show at its
/// lightness: a light pastel made dark would otherwise clip to another hue.
fn in_gamut(l: f32, a: f32, b: f32, alpha: f32) -> Color {
    let scaled = |k: f32| AlphaColor::<Oklab>::new([l, a * k, b * k, alpha]).convert::<Srgb>();
    let fits = |k: f32| {
        let [r, g, b, _] = scaled(k).components;
        [r, g, b].iter().all(|c| (-1e-4..=1.0 + 1e-4).contains(c))
    };
    if fits(1.0) {
        return scaled(1.0);
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..16 {
        let mid = (lo + hi) / 2.0;
        if fits(mid) { lo = mid } else { hi = mid }
    }
    scaled(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PALETTE: DarkPalette = DarkPalette {
        background: [0x1e, 0x1f, 0x24],
        foreground: [0xe0, 0xe2, 0xe8],
    };

    fn opaque([r, g, b]: [u8; 3]) -> Color {
        Color::from_rgba8(r, g, b, 255)
    }

    fn rgb(color: Color) -> [u8; 3] {
        let rgba = color.to_rgba8();
        [rgba.r, rgba.g, rgba.b]
    }

    fn moved(color: [u8; 3], role: Role) -> [u8; 3] {
        rgb(PALETTE.recolor(opaque(color), role))
    }

    fn close(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(&x, y)| x.abs_diff(y) <= 1)
    }

    fn lightness([r, g, b]: [u8; 3]) -> f32 {
        oklab(r, g, b)[0]
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
        let hue = |[r, g, b]: [u8; 3]| {
            let [_, a, b] = oklab(r, g, b);
            b.atan2(a).to_degrees()
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
        let shadow = PALETTE.recolor(opaque([0xff; 3]).with_alpha(0.25), Role::Surface);
        assert_eq!(shadow.components[3], 0.25);
    }

    #[test]
    fn reads_legacy_color_attributes() {
        let read = |value: &str| legacy_color(value).map(rgb);
        assert_eq!(read(" #FFF "), Some([255; 3]));
        assert_eq!(read("ffffff"), Some([255; 3]));
        assert_eq!(read("White"), Some([255; 3]));
        assert_eq!(read("#12345"), None);
        assert_eq!(read("transparent"), None);
        let white = PALETTE.recolor_attribute("#ffffff", Role::Surface).unwrap();
        assert!(close(read(&white).unwrap(), PALETTE.background), "{white}");
        assert_eq!(PALETTE.recolor_attribute("none", Role::Surface), None);
    }

    #[test]
    fn properties_take_the_role_of_their_colors() {
        assert_eq!(Role::of("COLOR"), Some(Role::Text));
        assert_eq!(Role::of("background"), Some(Role::Surface));
        assert_eq!(Role::of("border-bottom-color"), Some(Role::Line));
        // A custom property's color is moved where it is used, if at all.
        assert_eq!(Role::of("--brand"), None);
        assert_eq!(Role::of("font-family"), None);
    }
}
