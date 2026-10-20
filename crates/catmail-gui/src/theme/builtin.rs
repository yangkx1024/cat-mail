//! The themes that come with cat-mail, kept as terminal palettes and derived like
//! imported ones, so built-in and imported themes follow the same rules.

use gpui_kit::component::ThemeConfig;

use super::derive::theme_config;
use super::palette::{Rgb, TerminalPalette};

pub const DEFAULT_LIGHT: &str = "cat-mail Light";
pub const DEFAULT_DARK: &str = "cat-mail Dark";

/// (name, background, foreground, ANSI 0–15).
type Scheme = (&'static str, u32, u32, [u32; 16]);

const SCHEMES: &[Scheme] = &[
    (
        DEFAULT_LIGHT,
        0xffffff,
        0x1f2328,
        [
            0x24292f, 0xcf222e, 0x116329, 0x9a6700, 0x0969da, 0x8250df, 0x1b7c83, 0x6e7781,
            0x57606a, 0xa40e26, 0x1a7f37, 0x633c01, 0x218bff, 0xa475f9, 0x3192aa, 0x8c959f,
        ],
    ),
    (
        DEFAULT_DARK,
        0x1b1d22,
        0xe3e5e8,
        [
            0x2b2f36, 0xff7b72, 0x3fb950, 0xd29922, 0x58a6ff, 0xbc8cff, 0x39c5cf, 0xb1bac4,
            0x6e7681, 0xffa198, 0x56d364, 0xe3b341, 0x79c0ff, 0xd2a8ff, 0x56d4dd, 0xf0f6fc,
        ],
    ),
    (
        "Catppuccin Latte",
        0xeff1f5,
        0x4c4f69,
        [
            0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
            0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
        ],
    ),
    (
        "Catppuccin Mocha",
        0x1e1e2e,
        0xcdd6f4,
        [
            0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
            0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
        ],
    ),
    (
        "Gruvbox Light",
        0xfbf1c7,
        0x3c3836,
        [
            0xfbf1c7, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0x7c6f64,
            0x928374, 0x9d0006, 0x79740e, 0xb57614, 0x076678, 0x8f3f71, 0x427b58, 0x3c3836,
        ],
    ),
    (
        "Gruvbox Dark",
        0x282828,
        0xebdbb2,
        [
            0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
            0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
        ],
    ),
    (
        "Nord",
        0x2e3440,
        0xd8dee9,
        [
            0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
            0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
        ],
    ),
    (
        "Tokyo Night",
        0x1a1b26,
        0xc0caf5,
        [
            0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
            0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
        ],
    ),
    (
        "Solarized Light",
        0xfdf6e3,
        0x586e75,
        [
            0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
            0x002b36, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
        ],
    ),
    (
        "Solarized Dark",
        0x002b36,
        0x839496,
        [
            0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
            0x002b36, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
        ],
    ),
    (
        "Dracula",
        0x282a36,
        0xf8f8f2,
        [
            0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
            0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ],
    ),
];

const fn rgb(value: u32) -> Rgb {
    Rgb::new((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

pub(crate) fn palettes() -> Vec<TerminalPalette> {
    SCHEMES
        .iter()
        .map(|&(name, background, foreground, ansi)| TerminalPalette {
            name: name.to_owned(),
            background: rgb(background),
            foreground: rgb(foreground),
            ansi: ansi.map(rgb),
            cursor: None,
            selection_background: None,
            selection_foreground: None,
        })
        .collect()
}

/// In the order above: the defaults, then the others with each light and dark variant
/// together.
pub fn builtin_themes() -> Vec<ThemeConfig> {
    palettes().iter().map(theme_config).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_exist_with_the_right_appearance() {
        let themes = builtin_themes();
        let find = |name: &str| themes.iter().find(|t| t.name == name).unwrap();
        assert!(!find(DEFAULT_LIGHT).mode.is_dark());
        assert!(find(DEFAULT_DARK).mode.is_dark());
        let mut names: Vec<_> = themes.iter().map(|t| t.name.clone()).collect();
        names.dedup();
        assert_eq!(names.len(), SCHEMES.len());
    }

    #[test]
    fn solarized_dark_takes_hues_not_its_greys() {
        let palette = palettes()
            .into_iter()
            .find(|p| p.name == "Solarized Dark")
            .unwrap();
        let roles = super::super::derive::Roles::new(&palette);
        assert_eq!(roles.primary, rgb(0x268bd2));
    }
}
