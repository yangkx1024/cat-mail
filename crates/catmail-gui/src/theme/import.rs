//! Reading terminal color schemes, and GPUI Kit's own theme files.
//!
//! The format is sniffed from the content, since scheme files are passed around with
//! any name (Ghostty's have no extension at all); the extension only breaks ties. Every
//! format but GPUI Kit's gives a [`TerminalPalette`], which [`derive`] turns into a
//! theme.

use std::collections::HashMap;
use std::path::Path;

use gpui_kit::component::{ThemeConfig, ThemeSet};
use serde_json::Value;

use super::derive;
use super::palette::{Rgb, TerminalPalette};

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
    pub name: String,
    pub format: ThemeFormat,
    pub dark: bool,
    /// What the theme was derived from; `None` for a GPUI Kit theme, taken as it is.
    pub palette: Option<TerminalPalette>,
    pub config: ThemeConfig,
}

/// Why a file couldn't be imported, worded for the person who picked it.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImportError {
    #[error("This isn't a color scheme cat-mail can read.")]
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
    } else if let Some(format) = sniff_text(contents, &extension) {
        let palette = match format {
            ThemeFormat::Alacritty | ThemeFormat::WezTerm => toml_scheme(contents, &stem)?.0,
            ThemeFormat::Kitty => kitty(contents, &stem)?,
            ThemeFormat::Ghostty => ghostty(contents, &stem)?,
            ThemeFormat::Foot => foot(contents, &stem)?,
            ThemeFormat::Xresources => xresources(contents, &stem)?,
            ThemeFormat::Base16 | ThemeFormat::Base24 => base16(contents, &stem)?,
            ThemeFormat::WindowsTerminal | ThemeFormat::ITerm2 | ThemeFormat::GpuiKit => {
                unreachable!("not a text format")
            }
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
        name: palette.name.clone(),
        format,
        dark: palette.is_dark(),
        config: derive::theme_config(&palette),
        palette: Some(palette),
    }
}

/// The format of a line-based scheme, from what its lines look like.
fn sniff_text(contents: &str, extension: &str) -> Option<ThemeFormat> {
    let lines: Vec<&str> = contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .collect();
    let any = |f: &dyn Fn(&str) -> bool| lines.iter().any(|l| f(l));
    // TOML schemes are recognized by their tables; foot's ini isn't TOML (bare hex).
    if any(&|l| l.starts_with("[colors.") || l.starts_with("[metadata]"))
        || any(&|l| l.starts_with("[colors]")) && any(&|l| l.starts_with("ansi"))
    {
        return toml_kind(contents);
    }
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
    selection_foreground: Option<Rgb>,
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
            selection_foreground: self.selection_foreground,
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

fn kitty(contents: &str, stem: &str) -> Result<TerminalPalette> {
    let mut slots = Slots::default();
    let mut name = None;
    for line in contents.lines().map(str::trim) {
        // Themes from kitty-themes carry `## name: Kanagawa`.
        if let Some(header) = line.strip_prefix("##")
            && let Some(value) = header.trim().strip_prefix("name:")
        {
            name = Some(value.trim().to_owned());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(key), Some(value)) = (parts.next(), parts.next()) else {
            continue;
        };
        match key {
            "background" => slots.background = Some(color(value)?),
            "foreground" => slots.foreground = Some(color(value)?),
            "cursor" => slots.cursor = optional_color(value),
            "selection_background" => slots.selection_background = optional_color(value),
            "selection_foreground" => slots.selection_foreground = optional_color(value),
            _ => {
                if let Some(n) = ansi_index(key, "color") {
                    slots.ansi[n] = Some(color(value)?);
                }
            }
        }
    }
    slots.finish(name.unwrap_or_else(|| stem.to_owned()))
}

fn ghostty(contents: &str, stem: &str) -> Result<TerminalPalette> {
    let mut slots = Slots::default();
    for line in contents.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "background" => slots.background = Some(color(value)?),
            "foreground" => slots.foreground = Some(color(value)?),
            "cursor-color" => slots.cursor = optional_color(value),
            "selection-background" => slots.selection_background = optional_color(value),
            "selection-foreground" => slots.selection_foreground = optional_color(value),
            "palette" => {
                // `palette = 1=#c34043`
                let Some((n, value)) = value.split_once('=') else {
                    return Err(ImportError::BadColor(value.trim().to_owned()));
                };
                if let Ok(n) = n.trim().parse::<usize>()
                    && n < 16
                {
                    slots.ansi[n] = Some(color(value)?);
                }
            }
            _ => {}
        }
    }
    slots.finish(stem.to_owned())
}

fn foot(contents: &str, stem: &str) -> Result<TerminalPalette> {
    let mut slots = Slots::default();
    // foot 1.20 split `[colors]` into `[colors-dark]` and `[colors-light]`; the dark
    // one is the default, as `[colors]` was before.
    let mut in_colors = false;
    for line in contents.lines().map(str::trim) {
        if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_colors = matches!(line, "[colors]" | "[colors-dark]");
            continue;
        }
        if !in_colors {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "background" => slots.background = Some(color(value)?),
            "foreground" => slots.foreground = Some(color(value)?),
            // `cursor = <text> <cursor>`
            "cursor" => slots.cursor = value.split_whitespace().nth(1).and_then(optional_color),
            "selection-background" => slots.selection_background = optional_color(value),
            "selection-foreground" => slots.selection_foreground = optional_color(value),
            key => {
                if let Some(n) = ansi_index(key, "regular").filter(|&n| n < 8) {
                    slots.ansi[n] = Some(color(value)?);
                } else if let Some(n) = ansi_index(key, "bright").filter(|&n| n < 8) {
                    slots.ansi[n + 8] = Some(color(value)?);
                }
            }
        }
    }
    slots.finish(stem.to_owned())
}

/// What an X resource name sets: `*.color4`, `*color4`, `URxvt*background`,
/// `XTerm.vt100.foreground`, `*.cursorColor`.
fn xresource_name(key: &str) -> Option<XSlot> {
    let last = key.rsplit(['*', '.']).next().unwrap_or(key).trim();
    match last {
        "background" => Some(XSlot::Background),
        "foreground" => Some(XSlot::Foreground),
        "cursorColor" => Some(XSlot::Cursor),
        _ => ansi_index(last, "color").map(XSlot::Ansi),
    }
}

enum XSlot {
    Background,
    Foreground,
    Cursor,
    Ansi(usize),
}

fn xresources(contents: &str, stem: &str) -> Result<TerminalPalette> {
    let mut slots = Slots::default();
    let mut defines = HashMap::new();
    for line in contents.lines().map(str::trim) {
        if let Some(define) = line.strip_prefix("#define") {
            let mut parts = define.split_whitespace();
            if let (Some(name), Some(value)) = (parts.next(), parts.next()) {
                defines.insert(name.to_owned(), value.to_owned());
            }
            continue;
        }
        // `!` starts a comment; other `#` lines are preprocessor directives.
        if line.starts_with('!') || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Some(slot) = xresource_name(key) else {
            continue;
        };
        let value = value.trim();
        let value = defines.get(value).map_or(value, String::as_str);
        match slot {
            XSlot::Background => slots.background = Some(color(value)?),
            XSlot::Foreground => slots.foreground = Some(color(value)?),
            XSlot::Cursor => slots.cursor = optional_color(value),
            XSlot::Ansi(n) => slots.ansi[n] = Some(color(value)?),
        }
    }
    slots.finish(stem.to_owned())
}

fn is_base16_key(key: &str) -> bool {
    key.len() == 6
        && key.starts_with("base")
        && key[4..].bytes().all(|b| b.is_ascii_hexdigit())
        && u8::from_str_radix(&key[4..], 16).is_ok_and(|n| n <= 0x17)
}

/// base16 and base24 schemes, flat or with the colors under `palette:` (tinted-theming's
/// newer layout). Hand-parsed: the files are a flat list of `key: "value"`.
fn base16(contents: &str, stem: &str) -> Result<TerminalPalette> {
    let mut colors = HashMap::new();
    let mut name = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().trim_matches('"').to_ascii_lowercase();
        // Trailing comments: `base00: "1f1f28" # Background`.
        let value = value.split(" #").next().unwrap_or(value).trim();
        let value = value.trim_matches(['"', '\'']);
        match key.as_str() {
            "scheme" | "name" if name.is_none() && !value.is_empty() => {
                name = Some(value.to_owned());
            }
            key if is_base16_key(key) => {
                colors.insert(key[4..].to_ascii_lowercase(), color(value)?);
            }
            _ => {}
        }
    }
    let get = |slot: &str| colors.get(slot).copied();
    let base24 = colors.keys().any(|k| k.starts_with('1'));
    // base16-shell's mapping; base24 has its own bright hues in 12–17.
    let bright = |base16: &str, base24_slot: &str| {
        if base24 {
            get(base24_slot).or(get(base16))
        } else {
            get(base16)
        }
    };
    let slots = Slots {
        background: get("00"),
        foreground: get("05"),
        ansi: [
            get("00"),
            get("08"),
            get("0b"),
            get("0a"),
            get("0d"),
            get("0e"),
            get("0c"),
            get("05"),
            get("03"),
            bright("08", "12"),
            bright("0b", "14"),
            bright("0a", "13"),
            bright("0d", "16"),
            bright("0e", "17"),
            bright("0c", "15"),
            get("07"),
        ],
        ..Default::default()
    };
    slots.finish(name.unwrap_or_else(|| stem.to_owned()))
}

/// Alacritty or WezTerm, which both write TOML.
fn toml_kind(contents: &str) -> Option<ThemeFormat> {
    let table: toml::Table = toml::from_str(contents).ok()?;
    let colors = table.get("colors")?.as_table()?;
    if colors.contains_key("primary") || colors.contains_key("normal") {
        Some(ThemeFormat::Alacritty)
    } else if colors.contains_key("ansi") || colors.contains_key("background") {
        Some(ThemeFormat::WezTerm)
    } else {
        None
    }
}

fn toml_scheme(contents: &str, stem: &str) -> Result<(TerminalPalette, ThemeFormat)> {
    let table: toml::Table = toml::from_str(contents).map_err(|e| ImportError::Syntax {
        format: "TOML",
        detail: e.message().trim_end_matches('.').to_owned(),
    })?;
    let colors = table
        .get("colors")
        .and_then(toml::Value::as_table)
        .ok_or(ImportError::Unrecognized)?;
    let get = |table: &str, key: &str| {
        colors
            .get(table)
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get(key))
            .and_then(toml::Value::as_str)
    };
    let mut slots = Slots::default();
    let format = if colors.contains_key("primary") || colors.contains_key("normal") {
        slots.background = get("primary", "background").map(color).transpose()?;
        slots.foreground = get("primary", "foreground").map(color).transpose()?;
        slots.cursor = get("cursor", "cursor").and_then(optional_color);
        slots.selection_background = get("selection", "background").and_then(optional_color);
        slots.selection_foreground = get("selection", "text").and_then(optional_color);
        for (n, name) in ANSI_NAMES.iter().enumerate() {
            slots.ansi[n] = get("normal", name).map(color).transpose()?;
            slots.ansi[n + 8] = get("bright", name).map(color).transpose()?;
        }
        ThemeFormat::Alacritty
    } else {
        let key = |key: &str| colors.get(key).and_then(toml::Value::as_str);
        slots.background = key("background").map(color).transpose()?;
        slots.foreground = key("foreground").map(color).transpose()?;
        slots.cursor = key("cursor_bg").and_then(optional_color);
        slots.selection_background = key("selection_bg").and_then(optional_color);
        slots.selection_foreground = key("selection_fg").and_then(optional_color);
        for (offset, list) in [(0, "ansi"), (8, "brights")] {
            let values = colors.get(list).and_then(toml::Value::as_array);
            for (n, value) in values.into_iter().flatten().take(8).enumerate() {
                let value = value
                    .as_str()
                    .ok_or_else(|| ImportError::BadColor(value.to_string()))?;
                slots.ansi[offset + n] = Some(color(value)?);
            }
        }
        ThemeFormat::WezTerm
    };
    let name = table
        .get("metadata")
        .and_then(|m| m.get("name"))
        .and_then(toml::Value::as_str)
        .map_or_else(|| stem.to_owned(), str::to_owned);
    Ok((slots.finish(name)?, format))
}

/// `.itermcolors`: an XML property list of `<key>Ansi 4 Color</key><dict>…</dict>`
/// entries, each with `Red Component` etc. as reals from 0 to 1.
fn iterm2(contents: &str, stem: &str) -> Result<TerminalPalette> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let syntax = |detail: String| ImportError::Syntax {
        format: "XML",
        detail,
    };
    let mut reader = Reader::from_str(contents);
    reader.config_mut().trim_text(true);
    // Depth of `<dict>` nesting; color entries are keys of the top-level dict (depth 1)
    // whose values are dicts (depth 2).
    let mut depth = 0;
    let mut element = String::new();
    let mut entry: Option<String> = None;
    let mut component: Option<String> = None;
    let mut channels: HashMap<String, [Option<f64>; 3]> = HashMap::new();
    let mut seen_plist = false;
    loop {
        match reader.read_event().map_err(|e| syntax(e.to_string()))? {
            Event::Start(tag) => {
                element = String::from_utf8_lossy(tag.name().as_ref()).into_owned();
                match element.as_str() {
                    "plist" => seen_plist = true,
                    "dict" => depth += 1,
                    _ => {}
                }
            }
            Event::End(tag) => {
                if tag.name().as_ref() == b"dict" {
                    depth -= 1;
                    if depth == 1 {
                        entry = None;
                    }
                }
                element.clear();
            }
            Event::Text(text) => {
                let text = text.decode().map_err(|e| syntax(e.to_string()))?;
                let text = text.trim();
                match (element.as_str(), depth) {
                    ("key", 1) => entry = Some(text.to_owned()),
                    ("key", 2) => component = Some(text.to_owned()),
                    ("real" | "integer", 2) => {
                        let (Some(entry), Some(component)) = (&entry, component.take()) else {
                            continue;
                        };
                        let slot = match component.as_str() {
                            "Red Component" => 0,
                            "Green Component" => 1,
                            "Blue Component" => 2,
                            _ => continue,
                        };
                        let value: f64 = text
                            .parse()
                            .map_err(|_| ImportError::BadColor(text.to_owned()))?;
                        channels.entry(entry.clone()).or_default()[slot] = Some(value);
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !seen_plist {
        return Err(ImportError::Unrecognized);
    }
    let get = |key: &str| {
        let [r, g, b] = channels.get(key)?;
        Some(Rgb::from_unit((*r)?, (*g)?, (*b)?))
    };
    let mut slots = Slots {
        background: get("Background Color"),
        foreground: get("Foreground Color"),
        cursor: get("Cursor Color"),
        selection_background: get("Selection Color"),
        selection_foreground: get("Selected Text Color"),
        ..Default::default()
    };
    for (n, slot) in slots.ansi.iter_mut().enumerate() {
        *slot = get(&format!("Ansi {n} Color"));
    }
    slots.finish(stem.to_owned())
}

/// A Windows Terminal scheme or settings file, or a GPUI Kit theme set.
fn json(contents: &str) -> Result<Vec<ImportedTheme>> {
    let value: Value =
        serde_json::from_str(&strip_json_comments(contents)).map_err(|e| ImportError::Syntax {
            format: "JSON",
            detail: e.to_string(),
        })?;
    if value.get("themes").is_some_and(Value::is_array) {
        let set: ThemeSet = serde_json::from_value(value).map_err(|e| ImportError::Syntax {
            format: "GPUI Kit theme JSON",
            detail: e.to_string(),
        })?;
        if set.themes.is_empty() {
            return Err(ImportError::Empty);
        }
        return Ok(set
            .themes
            .into_iter()
            .map(|config| ImportedTheme {
                name: config.name.to_string(),
                format: ThemeFormat::GpuiKit,
                dark: config.mode.is_dark(),
                palette: None,
                config,
            })
            .collect());
    }
    let schemes = match value.get("schemes") {
        Some(Value::Array(schemes)) => schemes.iter().collect(),
        _ if value.get("background").is_some() => vec![&value],
        _ => return Err(ImportError::Unrecognized),
    };
    if schemes.is_empty() {
        return Err(ImportError::Empty);
    }
    schemes
        .into_iter()
        .map(|scheme| {
            let palette = windows_terminal(scheme)?;
            Ok(from_palette(palette, ThemeFormat::WindowsTerminal))
        })
        .collect()
}

fn windows_terminal(scheme: &Value) -> Result<TerminalPalette> {
    let get = |key: &str| scheme.get(key).and_then(Value::as_str);
    // Windows Terminal says purple where others say magenta.
    const KEYS: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "purple", "cyan", "white",
    ];
    let mut slots = Slots {
        background: get("background").map(color).transpose()?,
        foreground: get("foreground").map(color).transpose()?,
        cursor: get("cursorColor").and_then(optional_color),
        selection_background: get("selectionBackground").and_then(optional_color),
        ..Default::default()
    };
    for (n, key) in KEYS.iter().enumerate() {
        slots.ansi[n] = get(key).map(color).transpose()?;
        let bright = format!("bright{}{}", key[..1].to_ascii_uppercase(), &key[1..]);
        slots.ansi[n + 8] = get(&bright).map(color).transpose()?;
    }
    let name = get("name").unwrap_or("Windows Terminal scheme").to_owned();
    slots.finish(name)
}

/// Windows Terminal's settings file is JSON with comments and trailing commas.
fn strip_json_comments(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut chars = json.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = '\0';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            (',', _) => {
                // A trailing comma: only whitespace before the closing `}` or `]`.
                let rest: String = chars.clone().collect();
                let next = rest.trim_start().chars().next();
                if !matches!(next, Some('}' | ']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(file_name: &str, contents: &str) -> ImportedTheme {
        let mut themes = import(file_name, contents).unwrap();
        assert_eq!(themes.len(), 1);
        themes.pop().unwrap()
    }

    fn hex(c: Rgb) -> String {
        c.to_hex()
    }

    const KANAGAWA_KITTY: &str = "\
## name: Kanagawa
## license: MIT

background #1F1F28
foreground #DCD7BA
selection_background #2D4F67
selection_foreground #C8C093
cursor #C8C093

# normal
color0 #16161D
color1 #C34043
color2 #76946A
color3 #C0A36E
color4 #7E9CD8
color5 #957FB8
color6 #6A9589
color7 #C8C093

# bright
color8  #727169
color9  #E82424
color10 #98BB6C
color11 #E6C384
color12 #7FB4CA
color13 #938AA9
color14 #7AA89F
color15 #DCD7BA
";

    #[test]
    fn kitty() {
        let theme = one("kanagawa.conf", KANAGAWA_KITTY);
        assert_eq!(theme.format, ThemeFormat::Kitty);
        assert_eq!(theme.name, "Kanagawa");
        assert!(theme.dark);
        let p = theme.palette.unwrap();
        assert_eq!(hex(p.background), "#1f1f28");
        assert_eq!(hex(p.foreground), "#dcd7ba");
        assert_eq!(hex(p.ansi[4]), "#7e9cd8");
        assert_eq!(hex(p.ansi[15]), "#dcd7ba");
        assert_eq!(p.cursor.map(hex).as_deref(), Some("#c8c093"));
        assert_eq!(p.selection_background.map(hex).as_deref(), Some("#2d4f67"));
        assert_eq!(theme.config.name.as_ref(), "Kanagawa");
        assert!(theme.config.mode.is_dark());
    }

    #[test]
    fn kitty_without_a_name_header_takes_the_file_name() {
        let contents = KANAGAWA_KITTY.replace("## name: Kanagawa\n", "");
        assert_eq!(one("kanagawa_wave.conf", &contents).name, "Kanagawa Wave");
    }

    #[test]
    fn alacritty() {
        let theme = one(
            "gruvbox.toml",
            r##"
[colors.primary]
background = "0x282828"
foreground = "#ebdbb2"

[colors.cursor]
text = "CellBackground"
cursor = "CellForeground"

[colors.normal]
black = "#282828"
red = "#cc241d"
green = "#98971a"
yellow = "#d79921"
blue = "#458588"
magenta = "#b16286"
cyan = "#689d6a"
white = "#a89984"

[colors.bright]
black = "#928374"
red = "#fb4934"
green = "#b8bb26"
yellow = "#fabd2f"
blue = "#83a598"
magenta = "#d3869b"
cyan = "#8ec07c"
white = "#ebdbb2"
"##,
        );
        assert_eq!(theme.format, ThemeFormat::Alacritty);
        assert_eq!(theme.name, "Gruvbox");
        let p = theme.palette.unwrap();
        assert_eq!(hex(p.background), "#282828");
        assert_eq!(hex(p.ansi[9]), "#fb4934");
        // Keywords are not colors, and not an error either.
        assert_eq!(p.cursor, None);
    }

    #[test]
    fn ghostty() {
        let theme = one(
            "Catppuccin Mocha",
            "palette = 0=#45475a\npalette = 1=#f38ba8\npalette = 2=#a6e3a1\n\
             palette = 3=#f9e2af\npalette = 4=#89b4fa\npalette = 5=#f5c2e7\n\
             palette = 6=#94e2d5\npalette = 7=#bac2de\npalette = 8=#585b70\n\
             palette = 9=#f38ba8\npalette = 10=#a6e3a1\npalette = 11=#f9e2af\n\
             palette = 12=#89b4fa\npalette = 13=#f5c2e7\npalette = 14=#94e2d5\n\
             palette = 15=#a6adc8\nbackground = 1e1e2e\nforeground = cdd6f4\n\
             cursor-color = f5e0dc\nselection-background = 353749\n\
             selection-foreground = cdd6f4\n",
        );
        assert_eq!(theme.format, ThemeFormat::Ghostty);
        assert_eq!(theme.name, "Catppuccin Mocha");
        let p = theme.palette.unwrap();
        assert_eq!(hex(p.background), "#1e1e2e");
        assert_eq!(hex(p.ansi[12]), "#89b4fa");
        assert_eq!(p.cursor.map(hex).as_deref(), Some("#f5e0dc"));
    }

    #[test]
    fn wezterm() {
        let theme = one(
            "tokyonight.toml",
            r##"
[colors]
foreground = "#c0caf5"
background = "#1a1b26"
cursor_bg = "#c0caf5"
selection_bg = "#283457"
ansi = ["#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6"]
brights = ["#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5"]

[metadata]
name = "Tokyo Night"
"##,
        );
        assert_eq!(theme.format, ThemeFormat::WezTerm);
        assert_eq!(theme.name, "Tokyo Night");
        let p = theme.palette.unwrap();
        assert_eq!(hex(p.ansi[8]), "#414868");
        assert_eq!(p.selection_background.map(hex).as_deref(), Some("#283457"));
    }

    const CAMPBELL: &str = r##"{
        "name": "Campbell",
        "background": "#0C0C0C",
        "foreground": "#CCCCCC",
        "cursorColor": "#FFFFFF",
        "selectionBackground": "#FFFFFF",
        "black": "#0C0C0C", "red": "#C50F1F", "green": "#13A10E", "yellow": "#C19C00",
        "blue": "#0037DA", "purple": "#881798", "cyan": "#3A96DD", "white": "#CCCCCC",
        "brightBlack": "#767676", "brightRed": "#E74856", "brightGreen": "#16C60C",
        "brightYellow": "#F9F1A5", "brightBlue": "#3B78FF", "brightPurple": "#B4009E",
        "brightCyan": "#61D6D6", "brightWhite": "#F2F2F2"
    }"##;

    #[test]
    fn windows_terminal_scheme() {
        let theme = one("campbell.json", CAMPBELL);
        assert_eq!(theme.format, ThemeFormat::WindowsTerminal);
        assert_eq!(theme.name, "Campbell");
        let p = theme.palette.unwrap();
        assert_eq!(hex(p.ansi[5]), "#881798");
        assert_eq!(hex(p.ansi[13]), "#b4009e");
    }

    #[test]
    fn windows_terminal_settings_with_comments() {
        let settings = format!(
            "// Settings, see https://aka.ms/terminal-documentation\n{{\n\
             \"$schema\": \"https://aka.ms/terminal-profiles-schema\",\n\
             /* block */ \"schemes\": [\n{CAMPBELL},\n{},\n],\n}}",
            CAMPBELL.replace("Campbell", "Campbell Copy")
        );
        let themes = import("settings.json", &settings).unwrap();
        let names: Vec<_> = themes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Campbell", "Campbell Copy"]);
    }

    #[test]
    fn iterm2() {
        let mut entries = String::new();
        let color = |name: &str, (r, g, b): (f64, f64, f64)| {
            format!(
                "<key>{name}</key>\n<dict>\n<key>Alpha Component</key><real>1</real>\n\
                 <key>Blue Component</key><real>{b}</real>\n\
                 <key>Color Space</key><string>sRGB</string>\n\
                 <key>Green Component</key><real>{g}</real>\n\
                 <key>Red Component</key><real>{r}</real>\n</dict>\n"
            )
        };
        for n in 0..16 {
            entries += &color(&format!("Ansi {n} Color"), (f64::from(n) / 15.0, 0.5, 0.0));
        }
        entries += &color("Background Color", (0.0, 0.0, 0.0));
        entries += &color("Foreground Color", (1.0, 1.0, 1.0));
        entries += &color("Cursor Color", (1.0, 0.0, 0.0));
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n{entries}</dict>\n</plist>\n"
        );
        let theme = one("Solarized Dark.itermcolors", &plist);
        assert_eq!(theme.format, ThemeFormat::ITerm2);
        assert_eq!(theme.name, "Solarized Dark");
        let p = theme.palette.unwrap();
        assert_eq!(p.background, Rgb::BLACK);
        assert_eq!(p.ansi[15], Rgb::new(255, 128, 0));
        assert_eq!(p.ansi[0], Rgb::new(0, 128, 0));
        assert_eq!(p.cursor, Some(Rgb::new(255, 0, 0)));
    }

    #[test]
    fn foot() {
        let mut ini = String::from("[main]\nfont=monospace:size=11\n\n[colors-dark]\n");
        ini += "foreground=dcd7ba\nbackground=1f1f28\ncursor=1f1f28 c8c093\n";
        for n in 0..8 {
            ini += &format!("regular{n}=1{n}1{n}1{n}\nbright{n}=2{n}2{n}2{n}\n");
        }
        let theme = one("kanagawa.ini", &ini);
        assert_eq!(theme.format, ThemeFormat::Foot);
        let p = theme.palette.unwrap();
        assert_eq!(hex(p.background), "#1f1f28");
        assert_eq!(hex(p.ansi[3]), "#131313");
        assert_eq!(hex(p.ansi[11]), "#232323");
        assert_eq!(p.cursor.map(hex).as_deref(), Some("#c8c093"));
    }

    #[test]
    fn xresources_with_defines() {
        let mut x = String::from(
            "! Nord\n#define nord0 #2E3440\n#define nord4 #D8DEE9\n\
             *.foreground:   nord4\n*.background:   nord0\nURxvt*cursorColor: nord4\n",
        );
        for n in 0..16 {
            x += &format!("*color{n}: #0{n:x}0{n:x}0{n:x}\n");
        }
        let theme = one("nord.Xresources", &x);
        assert_eq!(theme.format, ThemeFormat::Xresources);
        assert_eq!(theme.name, "Nord");
        let p = theme.palette.unwrap();
        assert_eq!(hex(p.background), "#2e3440");
        assert_eq!(hex(p.foreground), "#d8dee9");
        assert_eq!(hex(p.ansi[10]), "#0a0a0a");
        assert_eq!(p.cursor.map(hex).as_deref(), Some("#d8dee9"));
    }

    #[test]
    fn base16_nested_palette() {
        let yaml = "\
system: \"base16\"
name: \"Tomorrow Night\"
author: \"Chris Kempson\"
variant: \"dark\"
palette:
  base00: \"#1d1f21\" # Background
  base01: \"#282a2e\"
  base02: \"#373b41\"
  base03: \"#969896\"
  base04: \"#b4b7b4\"
  base05: \"#c5c8c6\"
  base06: \"#e0e0e0\"
  base07: \"#ffffff\"
  base08: \"#cc6666\"
  base09: \"#de935f\"
  base0A: \"#f0c674\"
  base0B: \"#b5bd68\"
  base0C: \"#8abeb7\"
  base0D: \"#81a2be\"
  base0E: \"#b294bb\"
  base0F: \"#a3685a\"
";
        let theme = one("tomorrow-night.yaml", yaml);
        assert_eq!(theme.format, ThemeFormat::Base16);
        assert_eq!(theme.name, "Tomorrow Night");
        let p = theme.palette.unwrap();
        assert_eq!(hex(p.background), "#1d1f21");
        assert_eq!(hex(p.foreground), "#c5c8c6");
        assert_eq!(hex(p.ansi[4]), "#81a2be");
        assert_eq!(hex(p.ansi[8]), "#969896");
        assert_eq!(hex(p.ansi[15]), "#ffffff");
    }

    #[test]
    fn base24_flat() {
        let mut yaml = String::from("scheme: \"Flat Test\"\nauthor: \"me\"\n");
        for n in 0..=0x17u8 {
            yaml += &format!("base{n:02X}: \"{n:02x}{n:02x}{n:02x}\"\n");
        }
        let theme = one("flat.yaml", &yaml);
        assert_eq!(theme.format, ThemeFormat::Base24);
        assert_eq!(theme.name, "Flat Test");
        let p = theme.palette.unwrap();
        // Bright blue from base16 (0x16), not base0D.
        assert_eq!(hex(p.ansi[12]), "#161616");
        assert_eq!(hex(p.ansi[4]), "#0d0d0d");
    }

    #[test]
    fn gpui_kit_theme_set() {
        let configs: Vec<_> = crate::theme::builtin::builtin_themes()
            .into_iter()
            .take(2)
            .collect();
        let json = derive::theme_set_json(&configs);
        let themes = import("mine.json", &json).unwrap();
        assert_eq!(themes.len(), 2);
        assert!(themes.iter().all(|t| t.format == ThemeFormat::GpuiKit));
        assert!(themes.iter().all(|t| t.palette.is_none()));
        assert_eq!(themes[0].name, configs[0].name.as_ref());
        assert_eq!(themes[1].dark, configs[1].mode.is_dark());
    }

    #[test]
    fn only_normal_colors_fill_the_bright_ones() {
        let contents: String = KANAGAWA_KITTY
            .lines()
            .filter(|l| {
                let key = l.split_whitespace().next().unwrap_or("");
                !ansi_index(key, "color").is_some_and(|n| n >= 8)
            })
            .map(|l| format!("{l}\n"))
            .collect();
        let p = one("k.conf", &contents).palette.unwrap();
        assert_eq!(p.ansi[8], p.ansi[0]);
        assert_eq!(hex(p.ansi[9]), "#c34043");
    }

    #[test]
    fn errors() {
        assert_eq!(import("empty.conf", "").unwrap_err(), ImportError::NoColors);
        assert_eq!(
            import("notes.txt", "Remember to buy milk.\nAnd eggs.").unwrap_err(),
            ImportError::Unrecognized
        );
        assert_eq!(
            import(
                "a.conf",
                &KANAGAWA_KITTY.replace("background #1F1F28\n", "")
            )
            .unwrap_err(),
            ImportError::Missing("background")
        );
        assert_eq!(
            import("a.conf", &KANAGAWA_KITTY.replace("#7E9CD8", "#7E9CZ8")).unwrap_err(),
            ImportError::BadColor("#7E9CZ8".into())
        );
        let no_blue: String = KANAGAWA_KITTY
            .lines()
            .filter(|l| !l.starts_with("color4") && !l.starts_with("color12"))
            .map(|l| format!("{l}\n"))
            .collect();
        let error = import("a.conf", &no_blue).unwrap_err();
        assert_eq!(error, ImportError::MissingAnsi("color 4 (blue)".into()));
        assert_eq!(error.to_string(), "It doesn't set color 4 (blue).");
        assert!(matches!(
            import("a.json", "{\"background\": ").unwrap_err(),
            ImportError::Syntax { .. }
        ));
        assert_eq!(
            import("a.json", "{\"themes\": []}").unwrap_err(),
            ImportError::Empty
        );
        assert_eq!(
            import("a.json", "{\"hello\": 1}").unwrap_err(),
            ImportError::Unrecognized
        );
        // Every message is a sentence.
        for error in [
            ImportError::Unrecognized,
            ImportError::NoColors,
            ImportError::Empty,
        ] {
            assert!(error.to_string().ends_with('.'));
        }
    }

    #[test]
    fn json_comment_stripping_keeps_strings() {
        let stripped = strip_json_comments("{\"a\": \"http://x//y\", // c\n\"b\": [1, 2,],}");
        let value: Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(value["a"], "http://x//y");
        assert_eq!(value["b"], serde_json::json!([1, 2]));
    }
}
