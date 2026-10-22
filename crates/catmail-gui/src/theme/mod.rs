//! Themes: the built-in ones, color schemes imported from terminals, and the one in
//! effect.
//!
//! Every theme starts as a [`TerminalPalette`] (background, foreground, 16 ANSI colors)
//! and becomes a GPUI Kit [`ThemeConfig`](gpui_kit::component::ThemeConfig) through
//! [`theme_config`], so a scheme from Alacritty, kitty, Ghostty and the like looks as
//! deliberate as a built-in one. GPUI Kit's own theme files are taken as they are.
//! Only [`appearance`] (the themes on offer and the one applied) and [`colors`] (the
//! app's own colors, mixed from a theme) touch GPUI; the rest is pure.

pub mod appearance;
mod builtin;
pub mod colors;
mod derive;
mod import;
pub(crate) mod palette;

pub use builtin::{DEFAULT_DARK, DEFAULT_LIGHT, builtin_themes};
pub use derive::theme_set_json;
pub use import::{ImportError, ImportedTheme, import};
pub use palette::{Rgb, TerminalPalette};
