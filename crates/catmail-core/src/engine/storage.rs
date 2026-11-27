//! What the engine keeps between runs: the database, the blob store and the cache of
//! remote images, all in the data directory, and the passwords, in the keyring.
//!
//! Rows refer to what is kept outside the database, blobs and keyring entries, so that is
//! written first and the row second: whoever can see the row finds what it refers to.
//! What no row refers to any more is cleaned up after: a keyring entry as its row is
//! deleted ([`forget_secret`](Storage::forget_secret)), a blob by garbage collection once
//! it is old enough ([`collect_garbage`](Storage::collect_garbage)), since another row may
//! come to refer to it meanwhile (see [`BlobStore`]).
//!
//! Blocking work takes a [`Storage`] rather than the whole engine. Each handle holds the
//! data directory locked, so nothing writes into it once another engine may have it: the
//! lock goes with the last handle ([`released`](Storage::released)).

use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use catmail_proto::http::RemoteImage;
use catmail_store::{BlobStore, Db, Store};
use catmail_types::{BlobHash, GcReport};
use secrecy::SecretString;
use tokio::sync::watch;
use uuid::Uuid;

use crate::engine::image_cache::{ImageCache, MAX_CACHE_BYTES};
use crate::engine::secrets::SecretStore;
use crate::task::Closing;
use crate::{DataDirLock, Result};

/// How long an unreferenced blob survives. Far longer than the gap between storing a
/// blob and committing the row that refers to it, which is what GC must not cut into.
const BLOB_GRACE: Duration = Duration::from_secs(60 * 60);

/// A handle on what the engine keeps; clones share it. Its methods block.
#[derive(Clone)]
pub(crate) struct Storage(Arc<Kept>);

struct Kept {
    store: Store,
    blobs: BlobStore,
    images: ImageCache,
    secrets: Arc<dyn SecretStore>,
    /// One engine per data directory, so sync workers, the outbox and events are never
    /// split across processes. Other frontends will reach the running engine over IPC.
    _lock: DataDirLock,
    /// Never sent to: dropped with the last handle (see [`Storage::released`]).
    released: watch::Sender<()>,
}

impl Drop for Kept {
    fn drop(&mut self) {
        // The last line of a clean exit: without it, the log ending means the process was
        // killed or crashed.
        tracing::info!("engine closed");
    }
}

impl Storage {
    pub fn new(
        lock: DataDirLock,
        store: Store,
        blobs: BlobStore,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        let images = ImageCache::new(lock.config().image_cache_dir());
        Self(Arc::new(Kept {
            store,
            blobs,
            images,
            secrets,
            _lock: lock,
            released: watch::Sender::new(()),
        }))
    }

    /// Ends once every handle is dropped, and with them the lock on the data directory:
    /// another engine can open it then.
    pub fn released(&self) -> Closing {
        Closing::new(self.0.released.subscribe())
    }

    pub fn read<T>(&self, f: impl FnOnce(&Db<'_>) -> catmail_store::Result<T>) -> Result<T> {
        Ok(self.0.store.read(f)?)
    }

    /// Emit events after this returns.
    pub fn write<T>(&self, f: impl FnOnce(&Db<'_>) -> catmail_store::Result<T>) -> Result<T> {
        Ok(self.0.store.write(f)?)
    }

    /// [`write`](Self::write), on disk even across a power cut; see
    /// [`Store::write_durable`].
    pub fn write_durable<T>(
        &self,
        f: impl FnOnce(&Db<'_>) -> catmail_store::Result<T>,
    ) -> Result<T> {
        Ok(self.0.store.write_durable(f)?)
    }

    /// Puts `data` in the blob store, for a row committed soon after to refer to: GC
    /// takes it otherwise. One stored already has its grace period restarted.
    pub fn put(&self, data: impl Read) -> Result<BlobHash> {
        Ok(self.0.blobs.put(data)?)
    }

    /// [`put`](Self::put), of data in memory: content stored already is only hashed.
    pub fn put_bytes(&self, data: &[u8]) -> Result<BlobHash> {
        Ok(self.0.blobs.put_bytes(data)?)
    }

    /// Puts `data` in the blob store, then records it with `record` in a write
    /// transaction. `None` if `record` says the row it belongs to is gone: the blob is
    /// left for GC.
    pub fn keep(
        &self,
        data: &[u8],
        record: impl FnOnce(&Db<'_>, &BlobHash) -> catmail_store::Result<bool>,
    ) -> Result<Option<BlobHash>> {
        let hash = self.put_bytes(data)?;
        Ok(self.write(|db| record(db, &hash))?.then_some(hash))
    }

    /// The blob's content, `None` if it has gone missing from disk.
    pub fn blob(&self, hash: &BlobHash) -> Result<Option<Vec<u8>>> {
        match self.0.blobs.read(hash) {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The blob's size, its GC grace period restarted, as [`put`](Self::put) would; `None`
    /// if it has gone missing from disk.
    pub fn refresh(&self, hash: &BlobHash) -> Result<Option<u64>> {
        Ok(self.0.blobs.refresh(hash)?)
    }

    /// The file the blob's content is in, which stays as it is while a row refers to it.
    pub fn blob_path(&self, hash: &BlobHash) -> PathBuf {
        self.0.blobs.path(hash)
    }

    /// Deletes the blobs no row refers to that are older than [`BLOB_GRACE`], and gives
    /// the space deleted rows left in the database back to the file system.
    pub fn collect_garbage(&self) -> Result<GcReport> {
        // References first: see BlobStore::collect_garbage.
        let referenced = self.read(|db| db.blob_references())?;
        let report = self.0.blobs.collect_garbage(&referenced, BLOB_GRACE)?;
        let released = self.0.store.release_free_pages()?;
        if released > 0 {
            tracing::info!(bytes = released, "released free space in the database");
        }
        Ok(report)
    }

    /// Stores `password` in the keyring under a new key for a `kind` (`account`,
    /// `identity`) with that address, then runs `insert`, which stores the row naming the
    /// key; deletes the key again if that fails. Secret first, row second: whoever can see
    /// the row can also see its secret, so a concurrent removal always finds and deletes
    /// it. The random key keeps data directories apart.
    pub fn insert_with_secret<T>(
        &self,
        kind: &str,
        email: &str,
        password: &SecretString,
        insert: impl FnOnce(&str) -> Result<T>,
    ) -> Result<T> {
        let key = format!("{kind}:{email}:{}", Uuid::new_v4().simple());
        self.0.secrets.set(&key, password)?;
        let inserted = insert(&key);
        if inserted.is_err()
            && let Err(e) = self.0.secrets.delete(&key)
        {
            tracing::warn!(error = %e, "failed to delete password after {kind} insert failed");
        }
        inserted
    }

    /// The password in the keyring entry `key`.
    pub fn secret(&self, key: &str) -> Result<Option<SecretString>> {
        self.0.secrets.get(key)
    }

    /// Deletes the keyring entry of a row already deleted: left behind, it would only be
    /// clutter.
    pub fn forget_secret(&self, key: &str) {
        if let Err(e) = self.0.secrets.delete(key) {
            tracing::warn!(error = %e, "failed to delete stored password");
        }
    }

    /// Those of `urls` whose images are cached.
    pub fn cached_images(&self, urls: &[String]) -> Vec<(String, RemoteImage)> {
        self.0.images.get_many(urls)
    }

    pub fn cache_image(&self, url: &str, image: &RemoteImage) -> Result<()> {
        Ok(self.0.images.put(url, image)?)
    }

    /// Drops the images used longest ago, down to [`MAX_CACHE_BYTES`]; returns the bytes
    /// freed.
    pub fn trim_images(&self) -> Result<u64> {
        Ok(self.0.images.trim(MAX_CACHE_BYTES)?)
    }

    pub fn image_cache_size(&self) -> Result<u64> {
        Ok(self.0.images.size()?)
    }

    /// Returns the bytes freed.
    pub fn clear_images(&self) -> Result<u64> {
        Ok(self.0.images.clear()?)
    }
}
