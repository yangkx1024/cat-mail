//! Writing attachments to disk; their names come from `catmail_core::safe_filename`.
//! Neither command overwrites a file: the paths are the user's, but the bytes are a
//! stranger's.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Writes `data` to `dir/name`, or `dir/name (1)` etc. if that exists. Never
/// overwrites. Returns the path written.
pub(crate) fn write_new(dir: &Path, name: &str, data: &[u8]) -> io::Result<PathBuf> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    for n in 0.. {
        let candidate = if n == 0 {
            dir.join(name)
        } else {
            dir.join(format!("{stem} ({n}){ext}"))
        };
        match write_new_file(&candidate, data) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("ran out of numbers")
}

/// Writes `data` to a new file at `path`. Fails with `AlreadyExists` if there is
/// anything at `path`, a symbolic link included.
pub(crate) fn write_new_file(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = File::options().write(true).create_new(true).open(path)?;
    file.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_new(dir.path(), "a.txt", b"1").unwrap();
        let second = write_new(dir.path(), "a.txt", b"2").unwrap();
        let third = write_new(dir.path(), "noext", b"3").unwrap();
        let fourth = write_new(dir.path(), "noext", b"4").unwrap();
        assert_eq!(first, dir.path().join("a.txt"));
        assert_eq!(second, dir.path().join("a (1).txt"));
        assert_eq!(fourth, dir.path().join("noext (1)"));
        assert_eq!(std::fs::read(first).unwrap(), b"1");
        assert_eq!(std::fs::read(third).unwrap(), b"3");

        let err = write_new_file(&second, b"5").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&second).unwrap(), b"2");
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_links() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = write_new_file(&link, b"1").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(!target.exists());
    }
}
