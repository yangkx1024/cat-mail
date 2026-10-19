//! Writing attachments to disk; their names come from `catmail_core::safe_filename`.

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
        match File::options()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(data)?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("ran out of numbers")
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
    }
}
