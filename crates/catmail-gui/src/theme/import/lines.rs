//! The line-based formats: kitty, Ghostty, foot, Xresources, base16 and base24.

use super::*;

pub(super) fn kitty(contents: &str, stem: &str) -> Result<TerminalPalette> {
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
            _ => {
                if let Some(n) = ansi_index(key, "color") {
                    slots.ansi[n] = Some(color(value)?);
                }
            }
        }
    }
    slots.finish(name.unwrap_or_else(|| stem.to_owned()))
}

pub(super) fn ghostty(contents: &str, stem: &str) -> Result<TerminalPalette> {
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

pub(super) fn foot(contents: &str, stem: &str) -> Result<TerminalPalette> {
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
pub(super) fn xresource_name(key: &str) -> Option<XSlot> {
    let last = key.rsplit(['*', '.']).next().unwrap_or(key).trim();
    match last {
        "background" => Some(XSlot::Background),
        "foreground" => Some(XSlot::Foreground),
        "cursorColor" => Some(XSlot::Cursor),
        _ => ansi_index(last, "color").map(XSlot::Ansi),
    }
}

pub(super) enum XSlot {
    Background,
    Foreground,
    Cursor,
    Ansi(usize),
}

pub(super) fn xresources(contents: &str, stem: &str) -> Result<TerminalPalette> {
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

pub(super) fn is_base16_key(key: &str) -> bool {
    key.len() == 6
        && key.starts_with("base")
        && key[4..].bytes().all(|b| b.is_ascii_hexdigit())
        && u8::from_str_radix(&key[4..], 16).is_ok_and(|n| n <= 0x17)
}

/// base16 and base24 schemes, flat or with the colors under `palette:` (tinted-theming's
/// newer layout). Hand-parsed: the files are a flat list of `key: "value"`.
pub(super) fn base16(contents: &str, stem: &str) -> Result<TerminalPalette> {
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
