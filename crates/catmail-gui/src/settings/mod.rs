//! The window's preferences: appearance, themes, text size, reading. They belong to this
//! frontend, not to the engine, so they live in `settings.json` in the config directory
//! instead of the database, next to the imported themes. The settings dialog is in
//! `dialog/`.

pub mod dialog;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use gpui_kit::{App, AppContext as _, Global};
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

const DEFAULT_TEXT_SIZE: f32 = 16.;

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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            light_theme: DEFAULT_LIGHT.into(),
            dark_theme: DEFAULT_DARK.into(),
            text_size: DEFAULT_TEXT_SIZE,
            mark_read_on_open: true,
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

/// The settings in effect, as a GPUI global, and where they are kept.
pub struct Preferences {
    settings: Settings,
    /// Writes them to the config directory; `None` keeps them in memory only (tests).
    writer: Option<Arc<Writer>>,
}

impl Global for Preferences {}

impl Preferences {
    /// Reads `settings.json` from `config_dir`. A missing or unreadable file gives the
    /// defaults: losing preferences is better than not opening.
    pub fn load(config_dir: PathBuf) -> Self {
        let path = settings_path(&config_dir);
        let settings = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "ignoring unreadable settings");
                Settings::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "couldn't read settings");
                Settings::default()
            }
        };
        Self {
            settings,
            writer: Some(Arc::new(Writer::new(config_dir))),
        }
    }

    /// The defaults, not saved anywhere: there is no config directory.
    pub fn load_nowhere() -> Self {
        Self {
            settings: Settings::default(),
            writer: None,
        }
    }

    pub fn get(cx: &App) -> &Settings {
        &cx.global::<Self>().settings
    }

    /// Where imported themes are kept, if anywhere.
    pub fn themes_dir(cx: &App) -> Option<PathBuf> {
        cx.global::<Self>()
            .writer
            .as_ref()
            .map(|writer| writer.dir.join("themes"))
    }

    /// Changes the settings and writes them out in the background. Observers of this
    /// global put them into effect (see `appearance::init`).
    pub fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
        let preferences = cx.global_mut::<Self>();
        let before = preferences.settings.clone();
        change(&mut preferences.settings);
        if preferences.settings == before {
            return;
        }
        let Some(writer) = preferences.writer.clone() else {
            return;
        };
        writer.queue(preferences.settings.clone());
        cx.background_spawn(async move { writer.write_pending() })
            .detach();
    }
}

/// Writes the settings out in the background, in order. Writes run on any thread, so
/// one that started earlier may finish later; each takes the newest settings queued
/// while holding the write lock, so a late one finds nothing left rather than putting
/// older settings back.
struct Writer {
    dir: PathBuf,
    /// The newest settings not written yet.
    pending: Mutex<Option<Settings>>,
    /// Held while writing.
    writing: Mutex<()>,
}

impl Writer {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            pending: Mutex::default(),
            writing: Mutex::default(),
        }
    }

    fn queue(&self, settings: Settings) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(settings);
    }

    fn write_pending(&self) {
        let _writing = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(settings) = pending
            && let Err(e) = save(&self.dir, &settings)
        {
            tracing::warn!(dir = %self.dir.display(), error = %e, "couldn't save settings");
        }
    }
}

fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

fn save(config_dir: &Path, settings: &Settings) -> std::io::Result<()> {
    write_atomically(
        &settings_path(config_dir),
        &serde_json::to_vec_pretty(settings)?,
    )
}

/// Writes through a temporary file next to `path`, so a crash mid-write leaves the old
/// file. Creates the folder if needed.
pub fn write_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
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
        let writer = Writer::new(dir.path().to_path_buf());
        let sized = |text_size| Settings {
            text_size,
            ..Settings::default()
        };
        // Two changes, two writes; the second runs first and writes the newest.
        writer.queue(sized(15.));
        writer.queue(sized(16.));
        writer.write_pending();
        writer.write_pending();
        let loaded = Preferences::load(dir.path().to_path_buf()).settings;
        assert_eq!(loaded.text_size, 16.);
    }

    #[test]
    fn saves_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings {
            appearance: Appearance::Dark,
            dark_theme: "Nord".into(),
            text_size: 18.,
            mark_read_on_open: false,
            ..Settings::default()
        };
        save(dir.path(), &settings).unwrap();
        assert_eq!(
            Preferences::load(dir.path().to_path_buf()).settings,
            settings
        );
    }

    #[test]
    fn missing_or_broken_files_give_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Preferences::load(dir.path().to_path_buf()).settings,
            Settings::default()
        );
        std::fs::write(settings_path(dir.path()), "{not json").unwrap();
        assert_eq!(
            Preferences::load(dir.path().to_path_buf()).settings,
            Settings::default()
        );
        // Settings added later keep their defaults in an older file.
        std::fs::write(settings_path(dir.path()), r#"{"appearance":"light"}"#).unwrap();
        let loaded = Preferences::load(dir.path().to_path_buf()).settings;
        assert_eq!(loaded.appearance, Appearance::Light);
        assert!(loaded.mark_read_on_open);
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
