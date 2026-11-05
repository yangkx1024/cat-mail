//! The window's preferences: running in the background, appearance, themes, text size,
//! reading. They belong to this frontend, not to the engine, so they live in
//! `settings.json` in the config directory instead of the database, next to the imported
//! themes. The settings dialog is in `dialog/`.

pub mod dialog;

use std::path::PathBuf;
use std::sync::Arc;

use catmail_frontend::settings::SettingsFile;
pub use catmail_frontend::settings::{Appearance, Settings, TEXT_SIZES, write_atomically};
use gpui_kit::{App, AppContext as _, Global};

/// The settings in effect, as a GPUI global, and where they are kept.
pub struct Preferences {
    settings: Settings,
    /// Writes them to the config directory; `None` keeps them in memory only (tests).
    file: Option<Arc<SettingsFile>>,
}

impl Global for Preferences {}

impl Preferences {
    /// Reads `settings.json` from `config_dir`, or the defaults if it can't.
    pub fn load(config_dir: PathBuf) -> Self {
        let file = SettingsFile::new(config_dir);
        Self {
            settings: file.load(),
            file: Some(Arc::new(file)),
        }
    }

    /// The defaults, not saved anywhere: there is no config directory.
    pub fn load_nowhere() -> Self {
        Self {
            settings: Settings::default(),
            file: None,
        }
    }

    pub fn get(cx: &App) -> &Settings {
        &cx.global::<Self>().settings
    }

    /// Where imported themes are kept, if anywhere.
    pub fn themes_dir(cx: &App) -> Option<PathBuf> {
        cx.global::<Self>()
            .file
            .as_ref()
            .map(|file| file.themes_dir())
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
        let Some(file) = preferences.file.clone() else {
            return;
        };
        file.queue(preferences.settings.clone());
        cx.background_spawn(async move { file.write_pending() })
            .detach();
    }
}
