//! Themes: the built-in ones, color schemes imported from terminals, and the folder the
//! imported ones are kept in.
//!
//! Every theme starts as a [`TerminalPalette`] (background, foreground, 16 ANSI colors)
//! and becomes a [`ThemeConfig`] through the same mapping, so a scheme from Alacritty,
//! kitty, Ghostty and the like looks as deliberate as a built-in one. A theme is written
//! as GPUI Kit's theme JSON (the themes folder holds those files, and GPUI Kit's own theme
//! files are taken as they are), whichever frontend reads it: the GUI gives it to GPUI Kit,
//! the nsh spike maps its colors onto shadcn's tokens.

mod builtin;
mod derive;
mod import;
mod palette;
mod themes;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use builtin::builtin_themes;
pub use derive::theme_set_json;
use import::{ImportError, import};
pub use import::{ImportedTheme, ThemeFormat};
pub use palette::{Rgb, TerminalPalette};
pub use themes::{
    IMPORT_HELP, IMPORTABLE_FORMATS, ImportCandidate, ThemeEntry, Themes, file_name, import_title,
    read_import,
};

/// The built-in themes the settings start with.
pub const DEFAULT_LIGHT: &str = "catmail Light";
pub const DEFAULT_DARK: &str = "catmail Dark";

/// Light or dark, as a theme says it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Light,
    Dark,
}

impl ThemeMode {
    pub fn is_dark(self) -> bool {
        self == ThemeMode::Dark
    }
}

/// One theme of a GPUI Kit theme file: its name, light or dark, and its colors by GPUI
/// Kit's keys (`background`, `primary.background`, `list.active.background`). What else
/// the file says of it (fonts, radius, syntax colors) is kept as it is, for the GUI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub name: String,
    pub mode: ThemeMode,
    /// Values as GPUI Kit reads them: `#rrggbb`, `#rrggbbaa`, or a named color of its own.
    pub colors: Map<String, Value>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl ThemeConfig {
    pub fn is_dark(&self) -> bool {
        self.mode.is_dark()
    }

    /// The color at GPUI Kit's `key`, if the theme sets it.
    pub fn color(&self, key: &str) -> Option<&str> {
        self.colors.get(key).and_then(Value::as_str)
    }
}

/// A GPUI Kit theme file: one theme or several.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSet {
    pub name: String,
    pub themes: Vec<ThemeConfig>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}
