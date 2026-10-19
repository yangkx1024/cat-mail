use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};
use std::time::{Duration, SystemTime};

use catmail_types::{BlobHash, GcReport};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

/// Content-addressed file storage for raw messages and attachments.
///
/// A blob with hash `abcdef…` lives at `<root>/ab/cdef…`. Writes go to `<root>/tmp` first
/// and are renamed into place, never over an existing file, so readers never observe a
/// partial blob. Identical content is stored once.
///
/// # GC contract
///
/// There is a window between `put` returning and the database row that references the
/// hash being committed, and `put` may hand out a hash that already existed. A collector
/// must therefore only delete a blob that is both unreferenced *and* has an mtime older
/// than a grace period well above any write transaction's duration, re-checking the
/// mtime immediately before deleting. `put` refreshes the mtime of an existing blob to
/// restart that grace period. [`collect_garbage`](Self::collect_garbage) does all that.
pub struct BlobStore {
    root: PathBuf,
    /// Shared while `put` refreshes or creates a blob, exclusive while GC re-checks one
    /// blob's age and deletes it. Otherwise `put` could refresh a blob right after GC
    /// found it old, and hand out a hash GC is about to delete.
    gc: RwLock<()>,
}

impl BlobStore {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("tmp"))?;
        Ok(Self {
            root,
            gc: RwLock::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Streams `reader` into the store and returns the content hash.
    pub fn put(&self, mut reader: impl Read) -> io::Result<BlobHash> {
        let mut tmp = NamedTempFile::new_in(self.root.join("tmp"))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            hasher.update(&buf[..n]);
            tmp.write_all(&buf[..n])?;
        }
        let hash = BlobHash::parse(&hex::encode(hasher.finalize()))
            .expect("hex-encoded SHA-256 is a valid BlobHash");

        let dest = self.path(&hash);
        let _gc = self.gc.read().unwrap_or_else(PoisonError::into_inner);
        // Already stored: keep it alive for the GC grace period. The temp file is removed
        // on drop. If it vanished in between, fall through and store it again.
        match touch(&dest) {
            Ok(()) => return Ok(hash),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        tmp.as_file().sync_all()?;
        fs::create_dir_all(dest.parent().expect("blob path has a parent"))?;
        // Never replace a blob: a concurrent `put` of the same content may be storing it
        // too, and on Windows replacing a file that is being replaced or is open fails.
        match tmp.persist_noclobber(&dest) {
            Ok(_) => Ok(hash),
            // Same hash, same content: whoever stored it first did our job. (The error
            // still owns the temp file and removes it.)
            Err(_) if dest.is_file() => {
                touch(&dest)?;
                Ok(hash)
            }
            Err(e) => Err(e.error),
        }
    }

    pub fn put_bytes(&self, bytes: &[u8]) -> io::Result<BlobHash> {
        self.put(bytes)
    }

    pub fn path(&self, hash: &BlobHash) -> PathBuf {
        let (prefix, rest) = hash.as_str().split_at(2);
        self.root.join(prefix).join(rest)
    }

    pub fn contains(&self, hash: &BlobHash) -> bool {
        self.path(hash).is_file()
    }

    /// The whole content; `NotFound` if it is missing.
    pub fn read(&self, hash: &BlobHash) -> io::Result<Vec<u8>> {
        fs::read(self.path(hash))
    }

    pub fn open_blob(&self, hash: &BlobHash) -> io::Result<File> {
        File::open(self.path(hash))
    }

    /// Returns whether a blob was removed.
    pub fn remove(&self, hash: &BlobHash) -> io::Result<bool> {
        remove_if_exists(&self.path(hash))
    }

    /// Deletes blobs that are not in `referenced` and were last written or refreshed
    /// more than `grace` ago, and temp files left behind by interrupted writes.
    ///
    /// `referenced` must be read from the database *before* this is called: a blob
    /// referenced by a row committed later was `put` within the grace period (which
    /// must be far longer than any write transaction), so it is still too young.
    pub fn collect_garbage(
        &self,
        referenced: &HashSet<BlobHash>,
        grace: Duration,
    ) -> io::Result<GcReport> {
        let mut report = GcReport::default();
        for hash in self.gc_candidates(referenced, grace)? {
            if let Some(len) = self.remove_if_older(&hash, grace)? {
                report.removed += 1;
                report.bytes += len;
            }
        }
        for file in fs::read_dir(self.root.join("tmp"))? {
            let path = file?.path();
            if let Some(len) = removable(&path, grace)?
                && remove_if_exists(&path)?
            {
                report.bytes += len;
            }
        }
        Ok(report)
    }

    /// Unreferenced blobs that looked older than `grace` at the time.
    fn gc_candidates(
        &self,
        referenced: &HashSet<BlobHash>,
        grace: Duration,
    ) -> io::Result<Vec<BlobHash>> {
        let mut candidates = Vec::new();
        for dir in fs::read_dir(&self.root)? {
            let dir = dir?;
            let Some(prefix) = dir.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if prefix.len() != 2 || !dir.file_type()?.is_dir() {
                continue; // `tmp`, or something that isn't ours
            }
            for file in fs::read_dir(dir.path())? {
                let file = file?;
                let name = file.file_name();
                let Some(hash) = name
                    .to_str()
                    .and_then(|rest| BlobHash::parse(&format!("{prefix}{rest}")))
                else {
                    continue;
                };
                if !referenced.contains(&hash) && removable(&file.path(), grace)?.is_some() {
                    candidates.push(hash);
                }
            }
        }
        Ok(candidates)
    }

    /// Removes the blob if it is still older than `grace`: `put` may have refreshed it
    /// since it became a candidate. Returns its size if removed.
    fn remove_if_older(&self, hash: &BlobHash, grace: Duration) -> io::Result<Option<u64>> {
        let _gc = self.gc.write().unwrap_or_else(PoisonError::into_inner);
        let Some(len) = removable(&self.path(hash), grace)? else {
            return Ok(None);
        };
        Ok(self.remove(hash)?.then_some(len))
    }
}

/// The file's size, if it was last modified more than `grace` ago. A file that is
/// already gone, or whose mtime lies in the future, is not removable.
fn removable(path: &Path, grace: Duration) -> io::Result<Option<u64>> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let age = SystemTime::now()
        .duration_since(meta.modified()?)
        .unwrap_or_default();
    Ok((meta.is_file() && age > grace).then_some(meta.len()))
}

/// Returns whether the file was there.
fn remove_if_exists(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn touch(path: &Path) -> io::Result<()> {
    File::options()
        .write(true)
        .open(path)?
        .set_modified(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_puts_of_the_same_content_all_succeed() {
        // Two downloads of one message store the same blobs at the same time. Replacing
        // a file that another thread is replacing fails on Windows (os error 5).
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(dir.path()).unwrap();
        for round in 0..50u32 {
            let content = round.to_le_bytes();
            let start = std::sync::Barrier::new(8);
            let hashes: Vec<BlobHash> = std::thread::scope(|s| {
                let puts: Vec<_> = (0..8)
                    .map(|_| {
                        s.spawn(|| {
                            start.wait();
                            store.put_bytes(&content)
                        })
                    })
                    .collect();
                puts.into_iter()
                    .map(|p| p.join().unwrap().unwrap())
                    .collect()
            });
            assert!(hashes.windows(2).all(|w| w[0] == w[1]));
            assert_eq!(fs::read(store.path(&hashes[0])).unwrap(), content);
        }
    }

    #[test]
    fn put_read_remove() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::open(dir.path()).unwrap();

        let hash = blobs.put_bytes(b"hello").unwrap();
        // sha256("hello")
        assert_eq!(
            hash.as_str(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(
            blobs.path(&hash),
            dir.path().join("2c").join(&hash.as_str()[2..])
        );

        let mut content = String::new();
        blobs
            .open_blob(&hash)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "hello");

        // Same content dedupes to the same blob and leaves no temp files behind.
        assert_eq!(blobs.put(&b"hello"[..]).unwrap(), hash);
        assert_eq!(fs::read_dir(dir.path().join("tmp")).unwrap().count(), 0);

        assert!(blobs.remove(&hash).unwrap());
        assert!(!blobs.contains(&hash));
        assert!(!blobs.remove(&hash).unwrap());
    }

    #[test]
    fn putting_existing_content_refreshes_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::open(dir.path()).unwrap();
        let hash = blobs.put_bytes(b"hello").unwrap();

        let old = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        File::options()
            .write(true)
            .open(blobs.path(&hash))
            .unwrap()
            .set_modified(old)
            .unwrap();

        assert_eq!(blobs.put_bytes(b"hello").unwrap(), hash);
        let mtime = fs::metadata(blobs.path(&hash)).unwrap().modified().unwrap();
        assert!(mtime > old + std::time::Duration::from_secs(86_400));
        assert_eq!(fs::read(blobs.path(&hash)).unwrap(), b"hello");
    }

    #[test]
    fn large_blob_streams_across_buffer_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::open(dir.path()).unwrap();
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();

        let hash = blobs.put(&data[..]).unwrap();
        assert_eq!(hash.as_str(), hex::encode(Sha256::digest(&data)));
        assert_eq!(fs::read(blobs.path(&hash)).unwrap(), data);
    }

    fn set_mtime(path: &Path, secs_ago: u64) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
            .unwrap();
    }

    #[test]
    fn gc_removes_only_old_unreferenced_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::open(dir.path()).unwrap();
        let grace = Duration::from_secs(3600);
        let kept_ref = blobs.put_bytes(b"referenced").unwrap();
        let old = blobs.put_bytes(b"old and unreferenced").unwrap();
        let young = blobs.put_bytes(b"young").unwrap();
        for hash in [&kept_ref, &old] {
            set_mtime(&blobs.path(hash), 7200);
        }
        let stale_tmp = dir.path().join("tmp").join("partial");
        fs::write(&stale_tmp, b"half a blob").unwrap();
        set_mtime(&stale_tmp, 7200);
        fs::write(dir.path().join("stray.txt"), b"not ours").unwrap();

        let referenced = HashSet::from([kept_ref.clone()]);
        let report = blobs.collect_garbage(&referenced, grace).unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(
            report.bytes,
            (b"old and unreferenced".len() + b"half a blob".len()) as u64
        );
        assert!(blobs.contains(&kept_ref));
        assert!(!blobs.contains(&old));
        assert!(blobs.contains(&young));
        assert!(!stale_tmp.exists());
        assert!(dir.path().join("stray.txt").exists());

        // Putting the content again brings it back.
        assert_eq!(blobs.put_bytes(b"old and unreferenced").unwrap(), old);
        assert!(blobs.contains(&old));
        assert_eq!(
            blobs.collect_garbage(&referenced, grace).unwrap(),
            GcReport::default()
        );
    }

    #[test]
    fn gc_spares_a_blob_put_again_after_the_first_look() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::open(dir.path()).unwrap();
        let grace = Duration::from_secs(3600);
        let hash = blobs.put_bytes(b"x").unwrap();
        set_mtime(&blobs.path(&hash), 7200);
        let candidates = blobs.gc_candidates(&HashSet::new(), grace).unwrap();
        assert_eq!(candidates, std::slice::from_ref(&hash));

        // A row is about to refer to it again.
        blobs.put_bytes(b"x").unwrap();
        assert_eq!(blobs.remove_if_older(&hash, grace).unwrap(), None);
        assert!(blobs.contains(&hash));
    }
}
