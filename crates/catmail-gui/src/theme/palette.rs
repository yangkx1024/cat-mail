//! Colors as terminals describe them: 8-bit sRGB, and a palette of background,
//! foreground and the 16 ANSI colors.

use std::fmt;

/// An 8-bit sRGB color, as terminal color schemes write them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb::new(0, 0, 0);
    #[cfg(test)]
    pub const WHITE: Rgb = Rgb::new(0xff, 0xff, 0xff);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// From `0.0..=1.0` components, as iTerm2 stores them.
    pub fn from_unit(r: f64, g: f64, b: f64) -> Self {
        let channel = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Self::new(channel(r), channel(g), channel(b))
    }

    /// `#rgb`, `#rrggbb`, `rrggbb`, `0xrrggbb`, or X11's `rgb:r/g/b` with one to four
    /// hex digits per channel. A trailing alpha (`#rrggbbaa`) is ignored: terminals
    /// draw the scheme opaque.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(x11) = s.strip_prefix("rgb:") {
            let mut channels = x11.split('/').map(x11_channel);
            let rgb = Self::new(channels.next()??, channels.next()??, channels.next()??);
            return channels.next().is_none().then_some(rgb);
        }
        let hex = s
            .strip_prefix('#')
            .or_else(|| s.strip_prefix("0x"))
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        match hex.len() {
            3 => {
                let nibble = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok().map(|n| n * 17);
                Some(Self::new(nibble(0)?, nibble(1)?, nibble(2)?))
            }
            6 | 8 => Some(Self::new(byte(0)?, byte(2)?, byte(4)?)),
            _ => None,
        }
    }

    /// Lower-case `#rrggbb`.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// WCAG 2 relative luminance.
    pub fn luminance(self) -> f64 {
        let linear = |c: u8| {
            let v = f64::from(c) / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// WCAG 2 contrast ratio, 1 to 21.
    pub fn contrast(self, other: Rgb) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// `t` of the way from `self` to `other`, mixed in (gamma-encoded) sRGB like CSS's
    /// `color-mix(in srgb, …)`.
    pub fn mix(self, other: Rgb, t: f64) -> Rgb {
        let channel = |a: u8, b: u8| {
            let (a, b) = (f64::from(a), f64::from(b));
            (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
        };
        Rgb::new(
            channel(self.r, other.r),
            channel(self.g, other.g),
            channel(self.b, other.b),
        )
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// One channel of X11's `rgb:` syntax: 1–4 hex digits scaled to 8 bits.
fn x11_channel(s: &str) -> Option<u8> {
    if s.is_empty() || s.len() > 4 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(s, 16).ok()?;
    let max = (1u32 << (4 * s.len())) - 1;
    Some(((value * 255 + max / 2) / max) as u8)
}

/// What every terminal color scheme provides; the rest of a UI theme is derived from it
/// (see [`super::derive`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPalette {
    pub name: String,
    pub background: Rgb,
    pub foreground: Rgb,
    /// Black, red, green, yellow, blue, magenta, cyan, white, then their bright forms.
    pub ansi: [Rgb; 16],
    pub cursor: Option<Rgb>,
    pub selection_background: Option<Rgb>,
}

impl TerminalPalette {
    /// Light text on a dark background. Decided by the two colors rather than a fixed
    /// threshold, so a dim light scheme still counts as light.
    pub fn is_dark(&self) -> bool {
        self.background.luminance() < self.foreground.luminance()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_notations_schemes_use() {
        let c = Rgb::new(0x1f, 0x1f, 0x28);
        assert_eq!(Rgb::parse("#1F1F28"), Some(c));
        assert_eq!(Rgb::parse("1f1f28"), Some(c));
        assert_eq!(Rgb::parse("0x1f1f28"), Some(c));
        assert_eq!(Rgb::parse(" #1f1f28ff "), Some(c));
        assert_eq!(Rgb::parse("#fa0"), Some(Rgb::new(0xff, 0xaa, 0x00)));
        assert_eq!(Rgb::parse("rgb:1f/1f/28"), Some(c));
        assert_eq!(Rgb::parse("rgb:ffff/0/8080"), Some(Rgb::new(0xff, 0, 0x80)));
        for bad in [
            "",
            "#",
            "#12345",
            "#gg0000",
            "red",
            "rgb:1/2",
            "rgb:12345/0/0",
        ] {
            assert_eq!(Rgb::parse(bad), None, "{bad}");
        }
        assert_eq!(c.to_hex(), "#1f1f28");
    }

    #[test]
    fn contrast_and_mixing() {
        assert!((Rgb::BLACK.contrast(Rgb::WHITE) - 21.0).abs() < 1e-9);
        assert!((Rgb::WHITE.contrast(Rgb::WHITE) - 1.0).abs() < 1e-9);
        assert_eq!(Rgb::BLACK.mix(Rgb::WHITE, 0.5), Rgb::new(128, 128, 128));
        assert_eq!(Rgb::BLACK.mix(Rgb::WHITE, 0.0), Rgb::BLACK);
        assert_eq!(Rgb::BLACK.mix(Rgb::WHITE, 1.0), Rgb::WHITE);
    }
}
