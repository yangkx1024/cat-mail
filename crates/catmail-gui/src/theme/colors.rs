//! Colors this app needs that GPUI Kit's theme has no role for, derived from the theme's
//! own colors so they follow any theme, imported ones included. Text on them stays the
//! theme's foreground: the tints are light enough for it to read, whatever the hue.

use gpui_kit::Hsla;
use gpui_kit::component::{Colorize as _, Theme};

pub trait ThemeColors {
    /// A flagged message's flag.
    fn flag(&self) -> Hsla;
    /// Background and text of a badge for something that needs a look.
    fn warning_badge(&self) -> (Hsla, Hsla);
    /// Background and text of a badge for something that failed.
    fn danger_badge(&self) -> (Hsla, Hsla);
    /// Background and text of a sender's avatar: one of the theme's six hues, picked by
    /// the address so a sender keeps theirs.
    fn avatar(&self, seed: &str) -> (Hsla, Hsla);
}

/// How much of a hue goes into a tint, light and dark: dark backgrounds need more for
/// the same visible difference.
fn tint_share(theme: &Theme) -> f32 {
    if theme.is_dark() { 0.24 } else { 0.16 }
}

fn tint(theme: &Theme, hue: Hsla) -> Hsla {
    // `mix` weighs its receiver by the factor.
    theme.background.mix_oklab(hue, 1. - tint_share(theme))
}

impl ThemeColors for Theme {
    fn flag(&self) -> Hsla {
        self.warning
    }

    fn warning_badge(&self) -> (Hsla, Hsla) {
        (tint(self, self.warning), self.foreground)
    }

    fn danger_badge(&self) -> (Hsla, Hsla) {
        (tint(self, self.danger), self.foreground)
    }

    fn avatar(&self, seed: &str) -> (Hsla, Hsla) {
        let hues = [
            self.colors.red,
            self.colors.green,
            self.colors.yellow,
            self.colors.blue,
            self.colors.magenta,
            self.colors.cyan,
        ];
        // FNV-1a: stable across runs and platforms, unlike the std hasher.
        let hash = seed
            .to_lowercase()
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
            });
        let hue = hues[(hash % hues.len() as u64) as usize];
        (tint(self, hue), self.foreground)
    }
}
