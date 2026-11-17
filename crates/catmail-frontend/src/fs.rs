//! Writing and removing the app's own files: the settings, the themes, the autostart
//! entries, the single instance's socket.

use std::path::Path;

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

/// Removes the file at `path`: one that isn't there is removed already.
pub(crate) fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_written_whole_in_a_folder_made_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new").join("file.json");
        write_atomically(&path, b"old").unwrap();
        write_atomically(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        // Nothing of the temporary files is left beside it.
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn removing_a_file_that_is_not_there_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        remove_if_present(&path).unwrap();
        std::fs::write(&path, b"").unwrap();
        remove_if_present(&path).unwrap();
        assert!(!path.exists());
        // A folder isn't a file: that fails.
        assert!(remove_if_present(dir.path()).is_err());
    }
}
