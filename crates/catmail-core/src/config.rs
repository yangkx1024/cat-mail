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
        let dirs = ProjectDirs::from("dev", "CatMail", "CatMail").ok_or(Error::NoDataDir)?;
        Ok(Self::with_data_dir(dirs.data_dir()))
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
}
