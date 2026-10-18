use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use catmail_types::BlobHash;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

/// Content-addressed file storage for raw messages and attachments.
///
/// A blob with hash `abcdef…` lives at `<root>/ab/cdef…`. Writes go to `<root>/tmp` first
/// and are renamed into place, so readers never observe a partial blob. Identical content
/// is stored once.
///
/// # GC contract
///
/// There is a window between `put` returning and the database row that references the
/// hash being committed, and `put` may hand out a hash that already existed. A collector
/// must therefore only delete a blob that is both unreferenced *and* has an mtime older
/// than a grace period well above any write transaction's duration, re-checking the
/// mtime immediately before deleting. `put` refreshes the mtime of an existing blob to
/// restart that grace period.
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("tmp"))?;
        Ok(Self { root })
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
        // Already stored: keep it alive for the GC grace period. The temp file is removed
        // on drop. If it vanished in between, fall through and store it again.
        match touch(&dest) {
            Ok(()) => return Ok(hash),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        tmp.as_file().sync_all()?;
        fs::create_dir_all(dest.parent().expect("blob path has a parent"))?;
        tmp.persist(&dest).map_err(|e| e.error)?;
        Ok(hash)
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

    pub fn open_blob(&self, hash: &BlobHash) -> io::Result<File> {
        File::open(self.path(hash))
    }

    /// Returns whether a blob was removed.
    pub fn remove(&self, hash: &BlobHash) -> io::Result<bool> {
        match fs::remove_file(self.path(hash)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
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
}
