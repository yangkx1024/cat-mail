//! Themes: the built-in ones, and color schemes imported from terminals.
//!
//! Every theme starts as a [`TerminalPalette`] (background, foreground, 16 ANSI colors)
//! and becomes a GPUI Kit [`ThemeConfig`](gpui_kit::component::ThemeConfig) through
//! [`theme_config`], so a scheme from Alacritty, kitty, Ghostty and the like looks as
//! deliberate as a built-in one. GPUI Kit's own theme files are taken as they are.
//! Nothing here touches GPUI's global state; the window applies the result.

// Wired up by the GUI in the same milestone; remove then.
#![allow(dead_code, unused_imports)]

mod builtin;
mod derive;
mod import;
mod palette;

pub use builtin::{DEFAULT_DARK, DEFAULT_LIGHT, builtin_themes};
pub use derive::{theme_config, theme_set_json};
pub use import::{ImportError, ImportedTheme, ThemeFormat, import};
pub use palette::{Rgb, TerminalPalette};
