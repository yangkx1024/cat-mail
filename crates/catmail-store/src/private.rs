//! Files and directories readable by the user alone: they hold whole messages. On Unix
//! that is 0600 / 0700; elsewhere they inherit the parent directory's ACL, which for the
//! default data directory is the user profile's.

use std::fs::{self, File};
use std::io;
use std::path::Path;

/// Creates `dir` (its parent must exist) readable by the user alone. An existing
/// directory is left as it is.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = fs::DirBuilder::new();
    match builder.create(dir) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        result => result,
    }
}

/// Takes group and other access away from a file or directory, such as one made by a
/// version that didn't restrict it. Does nothing if it doesn't exist, or outside Unix.
pub(crate) fn make_private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = match fs::metadata(path) {
            Ok(metadata) => metadata.permissions(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let mode = permissions.mode();
        if mode & 0o077 != 0 {
            permissions.set_mode(mode & !0o077);
            fs::set_permissions(path, permissions)?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Creates `path` empty for the user alone if it doesn't exist. Created with that mode
/// rather than restricted afterwards: a reader that opens the file in between keeps
/// reading it.
pub(crate) fn new_file(path: &Path) -> io::Result<()> {
    let mut options = File::options();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_file_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, b"x").unwrap();
        new_file(&file).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"x");
    }
}
