//! Reading terminal color schemes, and GPUI Kit's own theme files.
//!
//! The format is sniffed from the content, since scheme files are passed around with
//! any name (Ghostty's have no extension at all); the extension only breaks ties. Every
//! format but GPUI Kit's gives a [`TerminalPalette`], which [`derive`] turns into a
//! theme.

mod lines;
mod structured;

use std::collections::HashMap;
use std::path::Path;

use gpui_kit::component::{ThemeConfig, ThemeSet};
use serde_json::Value;

use super::derive;
use super::palette::{Rgb, TerminalPalette};
use lines::{base16, foot, ghostty, is_base16_key, kitty, xresource_name, xresources};
use structured::{iterm2, json, strip_json_comments, toml_scheme};

/// Where a theme came from, for the import dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeFormat {
    Alacritty,
    Kitty,
    Ghostty,
    WezTerm,
    WindowsTerminal,
    ITerm2,
    Foot,
    Xresources,
    Base16,
    Base24,
    GpuiKit,
}

impl ThemeFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Alacritty => "Alacritty",
            Self::Kitty => "kitty",
            Self::Ghostty => "Ghostty",
            Self::WezTerm => "WezTerm",
            Self::WindowsTerminal => "Windows Terminal",
            Self::ITerm2 => "iTerm2",
            Self::Foot => "foot",
            Self::Xresources => "Xresources",
            Self::Base16 => "base16",
            Self::Base24 => "base24",
            Self::GpuiKit => "GPUI Kit",
        }
    }
}

/// A theme read from a file, ready to be saved to the themes folder.
#[derive(Debug, Clone)]
pub struct ImportedTheme {
    pub format: ThemeFormat,
    /// What the theme was derived from; `None` for a GPUI Kit theme, taken as it is.
    pub palette: Option<TerminalPalette>,
    pub config: ThemeConfig,
}

impl ImportedTheme {
    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn is_dark(&self) -> bool {
        self.config.mode.is_dark()
    }
}

/// Why a file couldn't be imported, worded for the person who picked it.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImportError {
    #[error("This isn't a color scheme catmail can read.")]
    Unrecognized,
    #[error("No background and foreground colors were found in it.")]
    NoColors,
    #[error("It has no {0} color.")]
    Missing(&'static str),
    #[error("It doesn't set {0}.")]
    MissingAnsi(String),
    #[error("“{0}” isn't a color.")]
    BadColor(String),
    #[error("It isn't valid {format}: {detail}.")]
    Syntax {
        format: &'static str,
        detail: String,
    },
    #[error("It contains no themes.")]
    Empty,
}

type Result<T> = std::result::Result<T, ImportError>;

/// Reads every theme in the file: usually one, several for a Windows Terminal settings
/// file or a GPUI Kit theme set.
pub fn import(file_name: &str, contents: &str) -> Result<Vec<ImportedTheme>> {
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    let stem = display_name(file_name);
    let extension = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let trimmed = contents.trim_start();

    let palettes = if trimmed.starts_with('<') {
        vec![(iterm2(contents, &stem)?, ThemeFormat::ITerm2)]
    } else if strip_json_comments(trimmed).trim_start().starts_with('{') {
        // Windows Terminal's settings file starts with a comment.
        return json(contents);
    } else if looks_like_toml(contents) {
        vec![toml_scheme(contents, &stem)?]
    } else if let Some(format) = sniff_text(contents, &extension) {
        let palette = match format {
            ThemeFormat::Kitty => kitty(contents, &stem)?,
            ThemeFormat::Ghostty => ghostty(contents, &stem)?,
            ThemeFormat::Foot => foot(contents, &stem)?,
            ThemeFormat::Xresources => xresources(contents, &stem)?,
            ThemeFormat::Base16 | ThemeFormat::Base24 => base16(contents, &stem)?,
            ThemeFormat::Alacritty
            | ThemeFormat::WezTerm
            | ThemeFormat::WindowsTerminal
            | ThemeFormat::ITerm2
            | ThemeFormat::GpuiKit => unreachable!("not sniffed"),
        };
        vec![(palette, format)]
    } else if let Ok((palette, format)) = toml_scheme(contents, &stem) {
        vec![(palette, format)]
    } else if contents.trim().is_empty() {
        return Err(ImportError::NoColors);
    } else {
        return Err(ImportError::Unrecognized);
    };
    Ok(palettes
        .into_iter()
        .map(|(palette, format)| from_palette(palette, format))
        .collect())
}

fn from_palette(palette: TerminalPalette, format: ThemeFormat) -> ImportedTheme {
    ImportedTheme {
        format,
        config: derive::theme_config(&palette),
        palette: Some(palette),
    }
}

/// The lines that say what a line-based scheme is: no blank lines or comments.
fn significant_lines(contents: &str) -> Vec<&str> {
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .collect()
}

/// Alacritty's and WezTerm's TOML, recognized by their tables; foot's ini isn't TOML
/// (bare hex).
fn looks_like_toml(contents: &str) -> bool {
    let lines = significant_lines(contents);
    let any = |f: &dyn Fn(&str) -> bool| lines.iter().any(|l| f(l));
    any(&|l| l.starts_with("[colors.") || l.starts_with("[metadata]"))
        || any(&|l| l.starts_with("[colors]")) && any(&|l| l.starts_with("ansi"))
}

/// The format of a line-based scheme other than TOML, from what its lines look like.
fn sniff_text(contents: &str, extension: &str) -> Option<ThemeFormat> {
    let lines = significant_lines(contents);
    let any = |f: &dyn Fn(&str) -> bool| lines.iter().any(|l| f(l));
    if any(&|l| l.starts_with("[colors")) {
        return Some(ThemeFormat::Foot);
    }
    if any(&|l| key_of(l, ':').is_some_and(|k| is_base16_key(k.trim_matches('"')))) {
        let base24 = any(&|l| key_of(l, ':').is_some_and(|k| k.starts_with("base1")));
        return Some(if base24 {
            ThemeFormat::Base24
        } else {
            ThemeFormat::Base16
        });
    }
    if any(&|l| {
        let key = l.split(':').next().unwrap_or("");
        l.contains(':') && (key.contains('*') || key.contains('.')) && xresource_name(key).is_some()
    }) {
        return Some(ThemeFormat::Xresources);
    }
    let ghostty_like = any(&|l| {
        key_of(l, '=')
            .is_some_and(|k| matches!(k, "palette" | "background" | "foreground" | "cursor-color"))
    });
    let kitty_like = any(&|l| {
        let key = l.split_whitespace().next().unwrap_or("");
        !l.contains('=')
            && (key == "background" || key == "foreground" || ansi_index(key, "color").is_some())
    });
    match (ghostty_like, kitty_like) {
        (true, false) => Some(ThemeFormat::Ghostty),
        (false, true) => Some(ThemeFormat::Kitty),
        (true, true) if extension == "conf" => Some(ThemeFormat::Kitty),
        (true, true) => Some(ThemeFormat::Ghostty),
        (false, false) => None,
    }
}

/// `key` of a `key <sep> value` line.
fn key_of(line: &str, separator: char) -> Option<&str> {
    let (key, _) = line.split_once(separator)?;
    let key = key.trim();
    (!key.is_empty() && !key.contains(char::is_whitespace)).then_some(key)
}

/// `n` from `color0`..`color15` (or another prefix).
fn ansi_index(key: &str, prefix: &str) -> Option<usize> {
    let n: usize = key.strip_prefix(prefix)?.parse().ok()?;
    (n < 16).then_some(n)
}

/// The scheme's name from its file name: `tokyo_night.conf` → "Tokyo Night". A name
/// that already has capitals (Ghostty's `Catppuccin Mocha`) is kept.
fn display_name(file_name: &str) -> String {
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file_name)
        .trim();
    if stem.chars().any(char::is_uppercase) {
        return stem.to_owned();
    }
    stem.split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The colors found so far; [`Slots::finish`] fills gaps between normal and bright.
#[derive(Default)]
struct Slots {
    background: Option<Rgb>,
    foreground: Option<Rgb>,
    ansi: [Option<Rgb>; 16],
    cursor: Option<Rgb>,
    selection_background: Option<Rgb>,
}

impl Slots {
    fn finish(self, name: String) -> Result<TerminalPalette> {
        let (background, foreground) = match (self.background, self.foreground) {
            (Some(bg), Some(fg)) => (bg, fg),
            (None, None) => return Err(ImportError::NoColors),
            (None, Some(_)) => return Err(ImportError::Missing("background")),
            (Some(_), None) => return Err(ImportError::Missing("foreground")),
        };
        let mut ansi = self.ansi;
        // Some schemes define only the normal or only the bright colors.
        for n in 0..8 {
            ansi[n] = ansi[n].or(ansi[n + 8]);
            ansi[n + 8] = ansi[n + 8].or(ansi[n]);
        }
        let missing: Vec<String> = (0..8)
            .filter(|&n| ansi[n].is_none())
            .map(|n| format!("{n} ({})", ANSI_NAMES[n]))
            .collect();
        if !missing.is_empty() {
            return Err(ImportError::MissingAnsi(join_list(&missing)));
        }
        Ok(TerminalPalette {
            name,
            background,
            foreground,
            ansi: ansi.map(|c| c.expect("filled above")),
            cursor: self.cursor,
            selection_background: self.selection_background,
        })
    }
}

const ANSI_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

/// "color 1 (red)", "colors 1 (red) and 3 (yellow)", "colors 1, 2 and 3".
fn join_list(items: &[String]) -> String {
    match items {
        [one] => format!("color {one}"),
        [rest @ .., last] => format!("colors {} and {last}", rest.join(", ")),
        [] => String::new(),
    }
}

fn color(value: &str) -> Result<Rgb> {
    let value = value.trim().trim_matches(['"', '\'']);
    Rgb::parse(value).ok_or_else(|| ImportError::BadColor(value.to_owned()))
}

/// For optional colors (cursor, selection) that some formats also allow as keywords
/// such as `CellForeground` or `none`: those are skipped, not an error.
fn optional_color(value: &str) -> Option<Rgb> {
    Rgb::parse(value.trim().trim_matches(['"', '\'']))
}

#[cfg(test)]
mod tests;
