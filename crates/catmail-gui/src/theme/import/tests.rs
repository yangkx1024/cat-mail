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
    assert_eq!(theme.name(), "Kanagawa");
    assert!(theme.is_dark());
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
    assert_eq!(one("kanagawa_wave.conf", &contents).name(), "Kanagawa Wave");
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
    assert_eq!(theme.name(), "Gruvbox");
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
    assert_eq!(theme.name(), "Catppuccin Mocha");
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
    assert_eq!(theme.name(), "Tokyo Night");
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
    assert_eq!(theme.name(), "Campbell");
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
    let names: Vec<_> = themes.iter().map(|t| t.name()).collect();
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
    assert_eq!(theme.name(), "Solarized Dark");
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
    assert_eq!(theme.name(), "Nord");
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
    assert_eq!(theme.name(), "Tomorrow Night");
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
    assert_eq!(theme.name(), "Flat Test");
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
    assert_eq!(themes[0].name(), configs[0].name.as_ref());
    assert_eq!(themes[1].is_dark(), configs[1].mode.is_dark());
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

#[test]
fn json_trailing_commas_go_also_before_a_comment() {
    let stripped = strip_json_comments("{\"name\": \"Scheme\", // c\n}");
    let value: Value = serde_json::from_str(&stripped).unwrap();
    assert_eq!(value["name"], "Scheme");
    let stripped = strip_json_comments("{\"b\": [1, /* c */], /* c */\n// c\n}");
    let value: Value = serde_json::from_str(&stripped).unwrap();
    assert_eq!(value["b"], serde_json::json!([1]));
    // Not in a string.
    let stripped = strip_json_comments("{\"a\": \",}\"}");
    let value: Value = serde_json::from_str(&stripped).unwrap();
    assert_eq!(value["a"], ",}");
}
