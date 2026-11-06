//! The window's preferences: running in the background, appearance, themes, text size,
//! reading. They belong to the frontends, not to the engine, so they live in
//! `settings.json` in the config directory instead of the database, next to the imported
//! themes. Every frontend reads and writes the same file.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

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
pub struct SettingsFile {
    dir: PathBuf,
    /// The newest settings not written yet.
    pending: Mutex<Option<Settings>>,
    /// Held while writing.
    writing: Mutex<()>,
}

impl SettingsFile {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            dir: config_dir,
            pending: Mutex::default(),
            writing: Mutex::default(),
        }
    }

    /// Where imported themes are kept.
    pub fn themes_dir(&self) -> PathBuf {
        self.dir.join("themes")
    }

    /// The settings in the file. A missing or unreadable file gives the defaults: losing
    /// preferences is better than not opening.
    pub fn load(&self) -> Settings {
        let path = self.path();
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "ignoring unreadable settings");
                Settings::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "couldn't read settings");
                Settings::default()
            }
        }
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
            && let Err(e) = self.save(&settings)
        {
            tracing::warn!(dir = %self.dir.display(), error = %e, "couldn't save settings");
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("settings.json")
    }

    fn save(&self, settings: &Settings) -> std::io::Result<()> {
        write_atomically(&self.path(), &serde_json::to_vec_pretty(settings)?)
    }
}

/// Writes through a temporary file next to `path`, so a crash mid-write leaves the old
/// file. Creates the folder if needed.
pub(crate) fn write_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(contents)?;
    file.persist(path)?;
    Ok(())
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
        std::fs::write(file.path(), "{not json").unwrap();
        assert_eq!(file.load(), Settings::default());
        // Settings added later keep their defaults in an older file.
        std::fs::write(file.path(), r#"{"appearance":"light"}"#).unwrap();
        let loaded = file.load();
        assert_eq!(loaded.appearance, Appearance::Light);
        assert!(loaded.mark_read_on_open);
        assert!(loaded.keep_running);
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
