//! The window's preferences: running in the background, appearance, themes, text size,
//! reading, serving AI agents. They belong to the frontends, not to the engine, so they live in
//! `settings.json` in the config directory instead of the database, next to the imported
//! themes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::theme::{DEFAULT_DARK, DEFAULT_LIGHT};

/// Light or dark, or whichever the system uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "System",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }
}

/// The text sizes offered, in pixels, with their names. The size is the window's `rem`,
/// so it scales spacing and icons along with the text.
pub const TEXT_SIZES: [(f32, &str); 5] = [
    (14., "Smaller"),
    (15., "Small"),
    (16., "Default"),
    (17., "Large"),
    (18., "Larger"),
];

/// The text size of `Default`, in pixels.
pub const DEFAULT_TEXT_SIZE: f32 = 16.;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    /// Theme names, as the theme registry knows them.
    pub light_theme: String,
    pub dark_theme: String,
    pub text_size: f32,
    /// Opening a message marks it read (on the server too, through the operation queue).
    pub mark_read_on_open: bool,
    /// Closing the last window leaves the app running, syncing and sending, until it is
    /// quit.
    pub keep_running: bool,
    /// New mail in an INBOX shows a system notification while the main window isn't in
    /// front.
    pub notify_new_mail: bool,
    /// The MCP server for AI agents, while the app runs.
    pub mcp: McpSettings,
}

/// The MCP server the app hosts ([`crate::mcp`]); `catmail serve` takes the same as its
/// options instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSettings {
    pub enabled: bool,
    /// On 127.0.0.1.
    pub port: u16,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: catmail_mcp::DEFAULT_PORT,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            light_theme: DEFAULT_LIGHT.into(),
            dark_theme: DEFAULT_DARK.into(),
            text_size: DEFAULT_TEXT_SIZE,
            mark_read_on_open: true,
            keep_running: true,
            notify_new_mail: true,
            mcp: McpSettings::default(),
        }
    }
}

impl Settings {
    /// The next text size up (`step` 1) or down (-1), staying within the offered sizes.
    pub fn stepped_text_size(&self, step: isize) -> f32 {
        let current = TEXT_SIZES
            .iter()
            .position(|(size, _)| *size >= self.text_size)
            .unwrap_or(TEXT_SIZES.len() - 1);
        let next = current
            .saturating_add_signed(step)
            .min(TEXT_SIZES.len() - 1);
        TEXT_SIZES[next].0
    }
}

/// Under "Remote images" in Settings › Reading: what the cache is, and what it takes once
/// measured.
pub fn image_cache_help(size: Option<u64>) -> String {
    let help = "Remote images you chose to show are kept on this computer and show again \
                without being loaded.";
    match size {
        Some(size) => format!("{help} They take {}.", crate::format::size(size)),
        None => help.to_owned(),
    }
}

/// `settings.json` in a config directory, and the writes to it.
///
/// Writes run on any thread (the frontend's background executor), so one that started
/// earlier may finish later; each takes the newest settings queued while holding the
/// write lock, so a late one finds nothing left rather than putting older settings back.
///
/// A write puts the settings the app holds into the file as [`load`](Self::load) read it,
/// so what the app doesn't know (settings a newer version added) stays. What it couldn't
/// parse is written over: `load` keeps a copy of such a file first, or leaves the file
/// alone.
pub struct SettingsFile {
    dir: PathBuf,
    /// The newest settings not written yet.
    pending: Mutex<Option<Settings>>,
    /// Held while writing.
    writing: Mutex<()>,
    /// The file couldn't be read, nor kept aside: writes are dropped rather than replace
    /// what may still hold the user's settings.
    read_only: AtomicBool,
    /// The file's object as `load` read it, for writes to keep what [`Settings`] leaves out.
    read: Mutex<Map<String, Value>>,
}

impl SettingsFile {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            dir: config_dir,
            pending: Mutex::default(),
            writing: Mutex::default(),
            read_only: AtomicBool::new(false),
            read: Mutex::default(),
        }
    }

    /// The config directory the file is in, with the other files frontends keep there
    /// (the MCP tokens).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where imported themes are kept.
    pub fn themes_dir(&self) -> PathBuf {
        self.dir.join("themes")
    }

    /// The settings in the file. A missing file gives the defaults, and so does each value
    /// that doesn't parse (from a hand edit, or a newer version): losing a preference is
    /// better than not opening, but the others are kept. A file with such values is copied
    /// to `settings.json.bad` before anything is written over it; one that can't be read
    /// at all isn't written over this run.
    pub fn load(&self) -> Settings {
        let path = self.path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "couldn't read settings; not saving them this run",
                );
                self.read_only.store(true, Ordering::Relaxed);
                return Settings::default();
            }
        };
        let (settings, dropped, read) = parse(&bytes);
        *self.read.lock().unwrap_or_else(PoisonError::into_inner) = read;
        if !dropped.is_empty() {
            let bad = self.dir.join("settings.json.bad");
            tracing::warn!(
                path = %path.display(),
                ?dropped,
                copy = %bad.display(),
                "ignoring settings that don't parse",
            );
            if let Err(e) = crate::fs::write_atomically(&bad, &bytes) {
                tracing::warn!(
                    copy = %bad.display(),
                    error = %e,
                    "couldn't copy settings aside; not saving them this run",
                );
                self.read_only.store(true, Ordering::Relaxed);
            }
        }
        settings
    }

    /// Makes `settings` the next to write; [`write_pending`](Self::write_pending) writes
    /// it.
    pub fn queue(&self, settings: Settings) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(settings);
    }

    /// Writes the newest queued settings, if any are left. Blocks on the file: call it off
    /// the UI thread.
    pub fn write_pending(&self) {
        let _writing = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(settings) = pending
            && !self.read_only.load(Ordering::Relaxed)
            && let Err(e) = self.save(&settings)
        {
            tracing::warn!(dir = %self.dir.display(), error = %e, "couldn't save settings");
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("settings.json")
    }

    fn save(&self, settings: &Settings) -> std::io::Result<()> {
        let mut file = Value::Object(
            self.read
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        );
        merge(&mut file, serde_json::to_value(settings)?);
        crate::fs::write_atomically(&self.path(), &serde_json::to_vec_pretty(&file)?)
    }
}

/// The settings in a file's text, the [JSON pointers](https://www.rfc-editor.org/rfc/rfc6901)
/// of the values left out because they don't parse (`""` for the whole text), and the
/// file's object. Each value left out has its default, as one missing from the file does.
fn parse(bytes: &[u8]) -> (Settings, Vec<String>, Map<String, Value>) {
    let Ok(Value::Object(entries)) = serde_json::from_slice::<Value>(bytes) else {
        return (Settings::default(), vec![String::new()], Map::new());
    };
    if let Ok(settings) = Settings::deserialize(&entries) {
        return (settings, Vec::new(), entries);
    }
    let read = entries.clone();
    // Starts from an empty object, so a value left out gets the default `#[serde(default)]`
    // gives it, not its type's.
    let mut kept = Value::Object(Map::new());
    let mut dropped = Vec::new();
    keep_parsing(&mut kept, "", entries, &mut dropped);
    let settings = Settings::deserialize(&kept).expect("only what parses is kept");
    (settings, dropped, read)
}

/// Puts `entries` into the object at the pointer `at` in `kept` one at a time, keeping each
/// that leaves `kept` parsing as [`Settings`] and adding the pointers of the rest to
/// `dropped`. An object that doesn't parse whole goes in entry by entry, so one bad value in
/// it (the MCP port) leaves out only that value.
fn keep_parsing(
    kept: &mut Value,
    at: &str,
    entries: Map<String, Value>,
    dropped: &mut Vec<String>,
) {
    fn object<'a>(kept: &'a mut Value, at: &str) -> &'a mut Map<String, Value> {
        kept.pointer_mut(at)
            .and_then(Value::as_object_mut)
            .expect("an object kept before its entries")
    }
    let parses = |kept: &Value| Settings::deserialize(kept).is_ok();
    for (key, value) in entries {
        let pointer = format!("{at}/{}", key.replace('~', "~0").replace('/', "~1"));
        object(kept, at).insert(key.clone(), value);
        if parses(kept) {
            continue;
        }
        if let Some(Value::Object(inner)) = object(kept, at).remove(&key) {
            object(kept, at).insert(key.clone(), Value::Object(Map::new()));
            if parses(kept) {
                keep_parsing(kept, &pointer, inner, dropped);
                continue;
            }
            object(kept, at).remove(&key);
        }
        dropped.push(pointer);
    }
}

/// Puts `from` into `into`: an object's entries one by one, into the object there, so the
/// entries only `into` has stay; anything else in place of what's there.
fn merge(into: &mut Value, from: Value) {
    match (into, from) {
        (Value::Object(into), Value::Object(from)) => {
            for (key, value) in from {
                merge(into.entry(key).or_insert(Value::Null), value);
            }
        }
        (into, from) => *into = from,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_late_write_does_not_put_older_settings_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::new(dir.path().to_path_buf());
        let sized = |text_size| Settings {
            text_size,
            ..Settings::default()
        };
        // Two changes, two writes; the second runs first and writes the newest.
        file.queue(sized(15.));
        file.queue(sized(16.));
        file.write_pending();
        file.write_pending();
        assert_eq!(file.load().text_size, 16.);
    }

    #[test]
    fn saves_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::new(dir.path().to_path_buf());
        let settings = Settings {
            appearance: Appearance::Dark,
            dark_theme: "Nord".into(),
            text_size: 18.,
            mark_read_on_open: false,
            mcp: McpSettings {
                enabled: true,
                port: 9000,
            },
            ..Settings::default()
        };
        file.save(&settings).unwrap();
        assert_eq!(file.load(), settings);
    }

    #[test]
    fn missing_or_broken_files_give_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::new(dir.path().to_path_buf());
        assert_eq!(file.load(), Settings::default());
        for broken in ["{not json", "", "[]"] {
            std::fs::write(file.path(), broken).unwrap();
            assert_eq!(file.load(), Settings::default());
        }
        // Settings added later keep their defaults in an older file.
        std::fs::write(file.path(), r#"{"appearance":"light"}"#).unwrap();
        let loaded = file.load();
        assert_eq!(loaded.appearance, Appearance::Light);
        assert!(loaded.mark_read_on_open);
        assert!(loaded.keep_running);
        assert_eq!(loaded.mcp, McpSettings::default());
        std::fs::write(file.path(), r#"{"mcp":{"enabled":true}}"#).unwrap();
        let loaded = file.load().mcp;
        assert!(loaded.enabled);
        assert_eq!(loaded.port, catmail_mcp::DEFAULT_PORT);
    }

    #[test]
    fn a_value_that_does_not_parse_leaves_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::new(dir.path().to_path_buf());
        let text = r#"{
            "appearance": "auto",
            "dark_theme": "Nord",
            "keep_running": null,
            "notify_new_mail": false,
            "mcp": {"enabled": true, "port": 70000},
            "added_later": 1
        }"#;
        std::fs::write(file.path(), text).unwrap();
        let loaded = file.load();
        // The values left out get the settings' defaults (`keep_running` stays on), not
        // their types'.
        assert_eq!(
            loaded,
            Settings {
                dark_theme: "Nord".into(),
                notify_new_mail: false,
                mcp: McpSettings {
                    enabled: true,
                    ..McpSettings::default()
                },
                ..Settings::default()
            }
        );
        // The file is copied aside before a save writes over it.
        let bad = dir.path().join("settings.json.bad");
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), text);
        file.queue(loaded.clone());
        file.write_pending();
        assert_eq!(file.load(), loaded);
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), text);
    }

    #[test]
    fn dropped_values_are_named() {
        let (_, dropped, _) = parse(br#"{"appearance":"auto","mcp":{"port":-1},"text_size":16}"#);
        assert_eq!(dropped, ["/appearance", "/mcp/port"]);
        let (_, dropped, _) = parse(br#"{"mcp":7}"#);
        assert_eq!(dropped, ["/mcp"]);
        assert_eq!(parse(b"{not json").1, [""]);
        assert!(parse(br#"{"mcp":{"enabled":true}}"#).1.is_empty());
    }

    #[test]
    fn a_save_keeps_what_the_app_does_not_know() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::new(dir.path().to_path_buf());
        // From a newer version: a setting, and one inside the MCP settings.
        std::fs::write(
            file.path(),
            r#"{"text_size":18,"added_later":[1],"mcp":{"port":9000,"host":"::1"}}"#,
        )
        .unwrap();
        let mut settings = file.load();
        settings.text_size = 14.;
        settings.mcp.enabled = true;
        file.queue(settings.clone());
        file.write_pending();
        let written: Value = serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
        assert_eq!(written["added_later"], serde_json::json!([1]));
        assert_eq!(
            written["mcp"],
            serde_json::json!({"enabled": true, "port": 9000, "host": "::1"})
        );
        assert_eq!(file.load(), settings);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_be_read_is_not_written_over() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::new(dir.path().to_path_buf());
        std::fs::write(file.path(), r#"{"text_size":18}"#).unwrap();
        let mode = |mode| std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(file.path(), mode(0o000)).unwrap();
        if std::fs::read(file.path()).is_ok() {
            return; // Root reads it anyway.
        }
        assert_eq!(file.load(), Settings::default());
        file.queue(Settings::default());
        file.write_pending();
        std::fs::set_permissions(file.path(), mode(0o600)).unwrap();
        assert_eq!(file.load().text_size, 18.);
    }

    #[test]
    fn text_size_steps_stay_in_range() {
        let at = |text_size| Settings {
            text_size,
            ..Settings::default()
        };
        assert_eq!(at(16.).stepped_text_size(1), 17.);
        assert_eq!(at(16.).stepped_text_size(-1), 15.);
        assert_eq!(at(18.).stepped_text_size(1), 18.);
        assert_eq!(at(14.).stepped_text_size(-1), 14.);
        // A size from a hand-edited file snaps to the offered ones.
        assert_eq!(at(15.5).stepped_text_size(0), 16.);
    }
}
