//! Remote images the user chose to show, kept on disk so that they show again without
//! being fetched: no request, and so no sign of reading, the second time.
//!
//! One file per URL, named by its SHA-256, holding the image's type, a newline and its
//! bytes. Not in the blob store: a blob may be shared with an attachment of the same
//! content and is only deleted after GC's grace period, while clearing the cache must
//! delete at once. Nothing else refers to these files, so any of them may go at any time
//! (a missing one is a miss). The engine's lock on the data directory makes it the only
//! writer.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use catmail_proto::http::RemoteImage;
use catmail_proto::mime::remote_image_url;
use sha2::{Digest, Sha256};

/// Past this, the images used longest ago go.
pub(crate) const MAX_CACHE_BYTES: u64 = 256 * 1024 * 1024;

/// Unfinished writes start with this, and are never read as images.
const TEMP_PREFIX: &str = ".tmp";

pub(crate) struct ImageCache {
    dir: PathBuf,
}

impl ImageCache {
    /// The cache in `dir`, made when the first image is kept (readable by the user alone:
    /// what was shown says what mail was read). Until then it is empty.
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Keyed by the URL as [`remote_image_url`] writes it, however the caller spelled it.
    fn path(&self, url: &str) -> PathBuf {
        let url = remote_image_url(url).unwrap_or_else(|| url.to_owned());
        self.dir.join(hex::encode(Sha256::digest(url.as_bytes())))
    }

    /// The images cached among `urls`, with their URLs. One that can't be read is a miss,
    /// and logged.
    pub(crate) fn get_many(&self, urls: &[String]) -> Vec<(String, RemoteImage)> {
        urls.iter()
            .filter_map(|url| match self.get(url) {
                Ok(image) => Some((url.clone(), image?)),
                Err(e) => {
                    tracing::warn!(%url, error = %e, "couldn't read a cached image");
                    None
                }
            })
            .collect()
    }

    /// The image cached for `url`, marked as used now. A file that isn't one (cut short,
    /// or not written by this) is dropped.
    pub(crate) fn get(&self, url: &str) -> io::Result<Option<RemoteImage>> {
        let path = self.path(url);
        // Writable only to touch it: the time it was last used decides what goes first.
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut reader = BufReader::new(file);
        let mut mime_type = Vec::new();
        reader.read_until(b'\n', &mut mime_type)?;
        let mime_type = (mime_type.pop() == Some(b'\n'))
            .then(|| String::from_utf8(mime_type).ok())
            .flatten()
            .filter(|t| t.starts_with("image/"));
        let Some(mime_type) = mime_type else {
            drop(reader);
            remove(&path)?;
            return Ok(None);
        };
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        reader.get_ref().set_modified(SystemTime::now())?;
        Ok(Some(RemoteImage { mime_type, bytes }))
    }

    /// Keeps `image` as the one for `url`, replacing any.
    pub(crate) fn put(&self, url: &str, image: &RemoteImage) -> io::Result<()> {
        catmail_store::create_private_dir(&self.dir)?;
        let mut file = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .tempfile_in(&self.dir)?;
        file.write_all(image.mime_type.as_bytes())?;
        file.write_all(b"\n")?;
        file.write_all(&image.bytes)?;
        file.persist(self.path(url)).map_err(|e| e.error)?;
        Ok(())
    }

    /// The bytes the cache takes on disk.
    pub(crate) fn size(&self) -> io::Result<u64> {
        Ok(self.entries()?.iter().map(|e| e.len).sum())
    }

    /// Deletes every cached image; returns the bytes freed.
    pub(crate) fn clear(&self) -> io::Result<u64> {
        let mut freed = 0;
        for entry in self.entries()? {
            if remove(&entry.path)? {
                freed += entry.len;
            }
        }
        Ok(freed)
    }

    /// Deletes the images used longest ago until the cache takes at most `max` bytes;
    /// returns the bytes freed.
    pub(crate) fn trim(&self, max: u64) -> io::Result<u64> {
        let mut entries: Vec<_> = (self.entries()?.into_iter()).filter(|e| !e.temp).collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.used));
        let mut kept = 0;
        let mut freed = 0;
        for entry in entries {
            if kept + entry.len <= max {
                kept += entry.len;
            } else if remove(&entry.path)? {
                freed += entry.len;
            }
        }
        Ok(freed)
    }

    fn entries(&self) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let dir = match fs::read_dir(&self.dir) {
            Ok(dir) => dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(entries),
            Err(e) => return Err(e),
        };
        for entry in dir {
            let entry = entry?;
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                // Gone meanwhile.
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if !metadata.is_file() {
                continue;
            }
            entries.push(Entry {
                temp: entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX),
                path: entry.path(),
                len: metadata.len(),
                used: metadata.modified()?,
            });
        }
        Ok(entries)
    }
}

struct Entry {
    path: PathBuf,
    len: u64,
    used: SystemTime,
    temp: bool,
}

/// Removes the file if it is there; returns whether it was.
fn remove(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::time::Duration;

    use super::*;

    fn image(bytes: &[u8]) -> RemoteImage {
        RemoteImage {
            mime_type: "image/png".into(),
            bytes: bytes.to_vec(),
        }
    }

    fn cache() -> (tempfile::TempDir, ImageCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = ImageCache::new(dir.path().join("remote-images"));
        assert_eq!(cache.size().unwrap(), 0, "nothing there yet");
        (dir, cache)
    }

    #[test]
    fn keeps_images_by_url_until_cleared() {
        let (_dir, cache) = cache();
        let url = "https://x.example/a.png";
        assert!(cache.get(url).unwrap().is_none());
        cache.put(url, &image(b"\n\npng")).unwrap();
        let got = cache.get(url).unwrap().unwrap();
        assert_eq!(
            (got.mime_type.as_str(), got.bytes.as_slice()),
            ("image/png", &b"\n\npng"[..])
        );
        assert!(cache.get("https://x.example/b.png").unwrap().is_none());
        assert!(
            cache.get("HTTPS://X.example/a.png").unwrap().is_some(),
            "normalized"
        );

        let size = cache.size().unwrap();
        assert_eq!(size, "image/png\n\n\npng".len() as u64);
        assert_eq!(cache.clear().unwrap(), size);
        assert_eq!(cache.size().unwrap(), 0);
        assert!(cache.get(url).unwrap().is_none());
    }

    #[test]
    fn drops_what_is_not_a_cached_image() {
        let (_dir, cache) = cache();
        let url = "https://x.example/a.png";
        cache.put(url, &image(b"")).unwrap();
        fs::write(cache.path(url), b"text/html\n<p>").unwrap();
        assert!(cache.get(url).unwrap().is_none());
        assert!(!cache.path(url).exists());
    }

    #[test]
    fn trims_the_images_used_longest_ago() {
        let (_dir, cache) = cache();
        let now = SystemTime::now();
        for (at, url) in ["a", "b", "c"].iter().enumerate() {
            cache.put(url, &image(&[0; 10])).unwrap();
            let file = File::options().write(true).open(cache.path(url)).unwrap();
            file.set_modified(now - Duration::from_secs(100 - at as u64))
                .unwrap();
        }
        // Using it makes it the newest.
        assert!(cache.get("a").unwrap().is_some());
        let each = cache.size().unwrap() / 3;
        assert_eq!(cache.trim(2 * each).unwrap(), each);
        assert!(cache.get("b").unwrap().is_none());
        assert!(cache.get("a").unwrap().is_some());
        assert!(cache.get("c").unwrap().is_some());
    }
}
