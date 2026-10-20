//! The formats with a structure to parse: Alacritty's and WezTerm's TOML, iTerm2's
//! property list, and JSON (Windows Terminal, GPUI Kit).

use super::*;

/// Alacritty or WezTerm, which both write TOML.
pub(super) fn toml_scheme(contents: &str, stem: &str) -> Result<(TerminalPalette, ThemeFormat)> {
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
pub(super) fn iterm2(contents: &str, stem: &str) -> Result<TerminalPalette> {
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
        ..Default::default()
    };
    for (n, slot) in slots.ansi.iter_mut().enumerate() {
        *slot = get(&format!("Ansi {n} Color"));
    }
    slots.finish(stem.to_owned())
}

/// A Windows Terminal scheme or settings file, or a GPUI Kit theme set.
pub(super) fn json(contents: &str) -> Result<Vec<ImportedTheme>> {
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
                format: ThemeFormat::GpuiKit,
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

pub(super) fn windows_terminal(scheme: &Value) -> Result<TerminalPalette> {
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
pub(super) fn strip_json_comments(json: &str) -> String {
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
