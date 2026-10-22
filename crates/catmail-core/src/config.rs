use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::{Error, Result};

/// Where cat-mail keeps its local state.
#[derive(Debug, Clone)]
pub struct Config {
    data_dir: PathBuf,
}

impl Config {
    /// Platform default: `$XDG_DATA_HOME/catmail` on Linux,
    /// `~/Library/Application Support/dev.CatMail.CatMail` on macOS,
    /// `%APPDATA%\CatMail\CatMail\data` on Windows.
    pub fn platform_default() -> Result<Self> {
        let dirs = project_dirs().ok_or(Error::NoDataDir)?;
        Ok(Self::with_data_dir(dirs.data_dir()))
    }

    /// Where frontends keep their settings: `$XDG_CONFIG_HOME/catmail` on Linux,
    /// `~/Library/Application Support/dev.CatMail.CatMail` on macOS,
    /// `%APPDATA%\CatMail\CatMail\config` on Windows.
    pub fn platform_config_dir() -> Option<PathBuf> {
        project_dirs().map(|dirs| dirs.config_dir().to_path_buf())
    }

    pub fn with_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("catmail.db")
    }

    pub fn blob_dir(&self) -> PathBuf {
        self.data_dir.join("blobs")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.data_dir.join("lock")
    }

    /// Files a frontend hands to other programs, such as a message page for the browser.
    /// Opening the engine empties it.
    pub fn handoff_dir(&self) -> PathBuf {
        self.data_dir.join("handoff")
    }

    /// Remote images the user chose to show, so they show again without a request.
    pub fn image_cache_dir(&self) -> PathBuf {
        self.data_dir.join("remote-images")
    }
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("dev", "CatMail", "CatMail")
}
