use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::{Error, Result};

/// Where catmail keeps its local state.
#[derive(Debug, Clone)]
pub struct Config {
    data_dir: PathBuf,
}

impl Config {
    /// Platform default: `$XDG_DATA_HOME/catmail` on Linux,
    /// `~/Library/Application Support/net.yangkx.catmail` on macOS,
    /// `%APPDATA%\yangkx\catmail\data` on Windows.
    pub fn platform_default() -> Result<Self> {
        let dirs = project_dirs().ok_or(Error::NoDataDir)?;
        Ok(Self::with_data_dir(dirs.data_dir()))
    }

    /// Where frontends keep their settings: `$XDG_CONFIG_HOME/catmail` on Linux,
    /// `~/Library/Application Support/net.yangkx.catmail` on macOS,
    /// `%APPDATA%\yangkx\catmail\config` on Windows.
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

    /// The log files a frontend keeps, for finding out later what happened.
    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Locks the data directory for one engine, creating it if needed (see
    /// [`create_data_dir`]). Fails with [`Error::AlreadyRunning`] if another engine, in
    /// this or another process, has it, and with [`Error::DataDirNotOwned`] if it belongs
    /// to another user. Opening the engine does this, unless given the lock
    /// ([`MailClient::builder_locked`](crate::MailClient::builder_locked)).
    pub fn lock(&self) -> Result<DataDirLock> {
        create_data_dir(&self.data_dir)?;
        warn_if_listable(&self.data_dir);
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.lock_path())?;
        match file.try_lock() {
            Ok(()) => Ok(DataDirLock {
                config: self.clone(),
                _file: file,
            }),
            Err(TryLockError::WouldBlock) => Err(Error::AlreadyRunning(self.data_dir.clone())),
            Err(TryLockError::Error(e)) => Err(e.into()),
        }
    }
}

/// The data directory, locked for one engine until this is dropped; see [`Config::lock`].
#[derive(Debug)]
pub struct DataDirLock {
    config: Config,
    _file: File,
}

impl DataDirLock {
    pub fn config(&self) -> &Config {
        &self.config
    }
}

/// Creates the data directory readable by the user alone: the database holds every
/// message's headers and text, and the drafts. Parent directories are made as usual. An
/// existing directory is left as it is (it may be one the user chose, such as their
/// home; the files the engine makes in it are private anyway), but it must belong to
/// this user.
pub(crate) fn create_data_dir(dir: &Path) -> Result<()> {
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    // Windows: the new directory inherits the user profile's ACL.
    catmail_store::create_private_dir(dir)?;
    check_data_dir_owner(dir)
}

/// Its owner could swap the files in it, and a symbolic link someone else planted where
/// the directory was expected (in a shared location) points to one they own. A link to
/// a directory of the user's own is fine. Windows: the directory's ACL is the user's to
/// set.
fn check_data_dir_owner(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if fs::metadata(dir)?.uid() != rustix::process::geteuid().as_raw() {
            return Err(Error::DataDirNotOwned(dir.to_path_buf()));
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Says so if other users can list the directory: once, as the engine locks it, though
/// the log file creates it too.
fn warn_if_listable(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(metadata) = fs::metadata(dir) else {
            return;
        };
        let mode = metadata.mode() & 0o777;
        if mode & 0o077 != 0 {
            tracing::warn!(
                dir = %dir.display(),
                mode = format!("{mode:o}"),
                "other users can list the data directory; the files in it are private"
            );
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("net", "yangkx", "catmail")
}
