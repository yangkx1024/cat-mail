//! The themes on offer and the one in effect. Built-in themes and the files in the
//! themes folder (GPUI Kit theme JSON, which is also what importing a terminal color
//! scheme writes there) are kept here, not in GPUI Kit's `ThemeRegistry`: a re-imported
//! theme replaces the old one under the same name, which the registry can't do.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeSet, try_parse_color};
use gpui_kit::{App, Global, Hsla, Window, WindowAppearance, px};

use crate::settings::{Appearance, Preferences, write_atomically};
use crate::theme::{
    DEFAULT_DARK, DEFAULT_LIGHT, ImportError, ImportedTheme, builtin_themes, import, theme_set_json,
};

/// A theme the settings offer.
#[derive(Clone)]
pub struct ThemeEntry {
    pub config: Rc<ThemeConfig>,
    /// The file it came from, for an imported theme.
    pub path: Option<PathBuf>,
    pub swatch: Swatch,
}

impl ThemeEntry {
    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn is_dark(&self) -> bool {
        self.config.mode.is_dark()
    }

    pub fn is_imported(&self) -> bool {
        self.path.is_some()
    }
}

/// The colors a theme card draws its miniature with. They are the theme's own, the card
/// being a picture of it.
#[derive(Clone, Copy)]
pub struct Swatch {
    pub background: Hsla,
    pub foreground: Hsla,
    pub sidebar: Hsla,
    pub border: Hsla,
    pub muted: Hsla,
    pub primary: Hsla,
    pub selection: Hsla,
    pub hues: [Hsla; 5],
}

impl Swatch {
    fn new(config: &ThemeConfig) -> Self {
        let colors = &config.colors;
        let color = |value: &Option<gpui_kit::SharedString>, fallback: Hsla| {
            value
                .as_deref()
                .and_then(|v| try_parse_color(v).ok())
                .unwrap_or(fallback)
        };
        let background = color(&colors.background, gpui_kit::white());
        let foreground = color(&colors.foreground, gpui_kit::black());
        let primary = color(&colors.primary, foreground);
        Self {
            background,
            foreground,
            sidebar: color(&colors.sidebar, background),
            border: color(&colors.border, foreground.opacity(0.15)),
            muted: color(&colors.muted_foreground, foreground.opacity(0.6)),
            primary,
            selection: color(&colors.list_active, primary.opacity(0.15)),
            hues: [
                color(&colors.danger, primary),
                color(&colors.success, primary),
                color(&colors.warning, primary),
                primary,
                color(&colors.info, primary),
            ],
        }
    }
}

/// Every theme on offer: the built-in ones, then the imported ones.
#[derive(Default)]
pub struct Themes {
    entries: Vec<ThemeEntry>,
    /// The desktop's appearance as a window last told it; the app-wide one isn't
    /// reliable on Linux.
    system: Option<WindowAppearance>,
}

impl Global for Themes {}

impl Themes {
    pub fn entries(cx: &App) -> &[ThemeEntry] {
        &cx.global::<Self>().entries
    }

    fn find(&self, name: &str) -> Option<&ThemeEntry> {
        self.entries.iter().find(|e| e.name() == name)
    }

    fn add(&mut self, config: ThemeConfig, path: Option<PathBuf>) {
        let entry = ThemeEntry {
            swatch: Swatch::new(&config),
            config: Rc::new(config),
            path,
        };
        match self.entries.iter_mut().find(|e| e.name() == entry.name()) {
            Some(old) => *old = entry,
            None => self.entries.push(entry),
        }
    }
}

/// Loads the themes and applies the settings. Before any window opens.
pub fn init(cx: &mut App) {
    let mut themes = Themes::default();
    for config in builtin_themes() {
        themes.add(config, None);
    }
    if let Some(dir) = Preferences::themes_dir(cx) {
        for (config, path) in read_themes_dir(&dir) {
            // A file can't take over a built-in theme's name.
            if themes.find(&config.name).is_none_or(|e| e.is_imported()) {
                themes.add(config, Some(path));
            }
        }
    }
    cx.set_global(themes);
    // Whatever changes the settings, they take effect.
    cx.observe_global::<Preferences>(apply).detach();
    apply(cx);
}

/// Follows the desktop's light or dark appearance as the window sees it, for "System".
pub fn system_appearance_changed(window: &Window, cx: &mut App) {
    cx.global_mut::<Themes>().system = Some(window.appearance());
    apply(cx);
}

/// The GPUI Kit theme files in `dir`, each theme with the file it is in. Files that
/// don't parse are skipped: one bad file shouldn't keep the app from starting.
fn read_themes_dir(dir: &Path) -> Vec<(ThemeConfig, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    let mut themes = Vec::new();
    for path in paths {
        let parsed = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<ThemeSet>(&bytes).map_err(|e| e.to_string())
            });
        match parsed {
            Ok(set) => themes.extend(set.themes.into_iter().map(|t| (t, path.clone()))),
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "ignoring theme file"),
        }
    }
    themes
}

/// Puts the settings' theme and text size into effect.
fn apply(cx: &mut App) {
    let settings = Preferences::get(cx).clone();
    let themes = cx.global::<Themes>();
    let pick = |name: &str, fallback: &str| {
        themes
            .find(name)
            .or_else(|| themes.find(fallback))
            .map(|e| Rc::new(crate::fonts::with_ui_font(&e.config)))
    };
    let (Some(light), Some(dark)) = (
        pick(&settings.light_theme, DEFAULT_LIGHT),
        pick(&settings.dark_theme, DEFAULT_DARK),
    ) else {
        return;
    };
    let mode = match settings.appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
        Appearance::System => themes
            .system
            .unwrap_or_else(|| cx.window_appearance())
            .into(),
    };
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
        theme.font_size = px(settings.text_size);
    });
    // Loads the mode's theme even if the mode is the same: the theme may not be.
    Theme::change(mode, None, cx);
}

/// A file read for importing, and what it holds.
pub struct ImportCandidate {
    pub file_name: String,
    pub themes: Vec<ImportedTheme>,
}

/// The largest file [`read_import`] reads. Theme files that hold dozens of themes stay
/// in the hundreds of KiB; parsing makes copies, so a file picked by mistake shouldn't be
/// read whole.
const MAX_IMPORT_SIZE: u64 = 4 * 1024 * 1024;

/// Reads and parses a color scheme file; not for the UI thread.
pub fn read_import(path: &Path) -> Result<ImportCandidate, String> {
    use std::io::Read as _;
    let file_name = file_name(path);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_IMPORT_SIZE + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("Couldn't read it: {e}."))?;
    if bytes.len() as u64 > MAX_IMPORT_SIZE {
        return Err(format!(
            "It's too large to be a color scheme (over {} MiB).",
            MAX_IMPORT_SIZE / (1024 * 1024)
        ));
    }
    let contents = String::from_utf8(bytes).map_err(|_| ImportError::Unrecognized.to_string())?;
    let themes = import(&file_name, &contents).map_err(|e| e.to_string())?;
    Ok(ImportCandidate { file_name, themes })
}

/// The name of the file at `path`, for showing.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Writes the imported themes into the themes folder and offers them. A name taken by a
/// built-in theme gets "(imported)" added; one taken by an imported theme is replaced.
/// Returns the names they are offered under.
pub fn install(themes: Vec<ImportedTheme>, cx: &mut App) -> Result<Vec<String>, String> {
    let dir = Preferences::themes_dir(cx).ok_or("There is no folder to keep themes in.")?;
    cx.global_mut::<Themes>().install(&dir, themes)
}

impl Themes {
    /// [`install`], into `dir`.
    fn install(&mut self, dir: &Path, themes: Vec<ImportedTheme>) -> Result<Vec<String>, String> {
        let mut names = Vec::new();
        for imported in themes {
            let mut config = imported.config;
            if self.find(&config.name).is_some_and(|e| !e.is_imported()) {
                config.name = format!("{} (imported)", config.name).into();
            }
            // A theme replaced keeps its file, and the other themes in that file stay.
            let path = match self.find(&config.name).and_then(|e| e.path.clone()) {
                Some(path) => path,
                None => self.free_file(dir, &config.name),
            };
            let mut in_file: Vec<ThemeConfig> = self
                .entries
                .iter()
                .filter(|e| e.path.as_ref() == Some(&path) && e.name() != config.name.as_ref())
                .map(|e| (*e.config).clone())
                .collect();
            in_file.push(config.clone());
            write_atomically(&path, theme_set_json(&in_file).as_bytes())
                .map_err(|e| format!("Couldn't save it: {e}."))?;
            names.push(config.name.to_string());
            self.add(config, Some(path));
        }
        Ok(names)
    }

    /// A file in `dir` for a new theme, named after it and nobody else's: names that
    /// differ may still make the same file name (`A/B`, `A-B`).
    fn free_file(&self, dir: &Path, name: &str) -> PathBuf {
        let taken = |path: &PathBuf| {
            path.exists() || self.entries.iter().any(|e| e.path.as_ref() == Some(path))
        };
        std::iter::once(theme_file(dir, name, None))
            .chain((2..).map(|n| theme_file(dir, name, Some(n))))
            .find(|path| !taken(path))
            .expect("some number is free")
    }
}

/// Longest file stem a numbered theme file keeps of its name, so the number always
/// fits: `safe_filename` shortens long names, and would cut it off.
const NUMBERED_STEM_BYTES: usize = 150;

/// The file in `dir` for a theme with this name, valid on every platform; with `n`, the
/// `n`th one for that name.
fn theme_file(dir: &Path, name: &str, n: Option<u32>) -> PathBuf {
    // One name, not a folder and a file.
    let name = name.replace(['/', '\\'], "-");
    let mut stem = catmail_core::safe_filename(Some(&name), "theme");
    if let Some(n) = n {
        let cut = stem.floor_char_boundary(NUMBERED_STEM_BYTES);
        stem.truncate(cut);
        stem = format!("{} {n}", stem.trim_end());
    }
    dir.join(format!("{stem}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_files_have_portable_names() {
        let dir = Path::new("themes");
        let name = |theme| {
            theme_file(dir, theme, None)
                .strip_prefix(dir)
                .unwrap()
                .to_owned()
        };
        assert_eq!(name("Catppuccin Mocha"), Path::new("Catppuccin Mocha.json"));
        assert_eq!(name("a/b\\c:d"), Path::new("a-b-c_d.json"));
        assert_eq!(name("暗夜 主题"), Path::new("暗夜 主题.json"));
        assert_eq!(name(" .. "), Path::new("theme.json"));
        assert_eq!(name("CON"), Path::new("_CON.json"));
    }

    /// What importing a GPUI Kit theme file holding `configs` gives.
    fn imported(configs: &[ThemeConfig]) -> Vec<ImportedTheme> {
        import("themes.json", &theme_set_json(configs)).unwrap()
    }

    fn named(name: &str, n: usize) -> ThemeConfig {
        let mut config = builtin_themes().remove(n);
        config.name = name.to_owned().into();
        config
    }

    #[test]
    fn themes_whose_names_make_the_same_file_name_keep_their_own_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut themes = Themes::default();
        themes
            .install(dir.path(), imported(&[named("A/B", 0)]))
            .unwrap();
        themes
            .install(dir.path(), imported(&[named("A-B", 1)]))
            .unwrap();
        let mut read: Vec<_> = read_themes_dir(dir.path())
            .into_iter()
            .map(|(config, _)| config.name.to_string())
            .collect();
        read.sort();
        assert_eq!(read, ["A-B", "A/B"]);
    }

    #[test]
    fn long_names_with_the_same_start_get_numbered_files() {
        let dir = tempfile::tempdir().unwrap();
        let long = "長".repeat(100);
        let mut themes = Themes::default();
        for (n, ending) in ["A", "B", "C"].into_iter().enumerate() {
            let name = format!("{long}{ending}");
            themes
                .install(dir.path(), imported(&[named(&name, n)]))
                .unwrap();
        }
        assert_eq!(read_themes_dir(dir.path()).len(), 3);
    }

    #[test]
    fn reimporting_a_theme_keeps_the_others_in_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pair.json");
        let (x, y) = (named("X", 0), named("Y", 1));
        write_atomically(&path, theme_set_json(&[x, y.clone()]).as_bytes()).unwrap();
        let mut themes = Themes::default();
        for (config, path) in read_themes_dir(dir.path()) {
            themes.add(config, Some(path));
        }

        let new_x = named("X", 2);
        themes
            .install(dir.path(), imported(std::slice::from_ref(&new_x)))
            .unwrap();
        let read = read_themes_dir(dir.path());
        assert_eq!(read.len(), 2, "one file, both themes");
        assert!(read.iter().all(|(_, p)| *p == path));
        let find = |name: &str| read.iter().find(|(c, _)| c.name == name).unwrap().0.clone();
        assert_eq!(find("X").colors.background, new_x.colors.background);
        assert_eq!(find("Y").colors.background, y.colors.background);
    }

    #[test]
    fn oversized_files_are_not_imported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.json");
        std::fs::write(&path, vec![b' '; MAX_IMPORT_SIZE as usize + 1]).unwrap();
        let Err(e) = read_import(&path) else {
            panic!("imported an oversized file");
        };
        assert!(e.contains("too large"), "{e}");
    }

    #[test]
    fn written_themes_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let config = builtin_themes().remove(0);
        let path = dir.path().join("x.json");
        write_atomically(
            &path,
            theme_set_json(std::slice::from_ref(&config)).as_bytes(),
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.json"), "{").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a theme").unwrap();
        let read = read_themes_dir(dir.path());
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].0.name, config.name);
        assert_eq!(read[0].1, path);
    }
}
