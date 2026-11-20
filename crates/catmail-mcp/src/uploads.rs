//! Files put to the server over HTTP for a draft, so their content doesn't have to pass
//! through the model as base64 in a tool's arguments: `prepare_upload` gives a URL,
//! the agent's own process puts the file's bytes there (`curl -T`), and the draft tools
//! take the upload's handle in place of the content.
//!
//! The URL is the only credential the upload needs: the model never sees its token
//! (it is in the client's configuration), so it couldn't send it from a shell. A handle
//! is random, belongs to the token that prepared it, takes one file within
//! [`UPLOAD_WITHIN`], and is gone once a call that used it succeeds. Kept in memory: a
//! restart of the host forgets them, as it does questions asked before sending.
//!
//! What a token's uploads hold counts the files being put too, as large as they may
//! grow: otherwise each of many puts begun together could buffer a whole file.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use hyper::body::Bytes;

/// What the files given to a draft in one call may add up to, decoded, and how large a
/// file put to an upload may be. Base64 makes them a third larger in a request
/// (`http::MAX_REQUEST_BYTES`).
pub(crate) const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;
/// Where uploads are put, below the MCP endpoint: `/mcp/uploads/<handle>`.
pub(crate) const PATH: &str = "/mcp/uploads/";
/// How long after it is prepared an upload may be put and used.
pub(crate) const UPLOAD_WITHIN: Duration = Duration::from_secs(15 * 60);
/// What a token may have prepared and not used at once: every upload is a slot in
/// memory until it expires.
const PREPARED_PER_TOKEN: usize = 16;
/// What the files a token has put and not used, and those being put, may add up to: two
/// calls' worth, so that one which failed doesn't keep it from preparing the files of
/// another.
const HELD_PER_TOKEN: usize = 2 * MAX_FILE_BYTES;

#[derive(Default)]
pub(crate) struct Uploads(Mutex<HashMap<String, Upload>>);

struct Upload {
    /// The name of the token that prepared it.
    token: String,
    prepared_at: Instant,
    state: State,
}

enum State {
    Waiting,
    /// Its file is being put ([`Receiving`]): no one else may put one meanwhile. Holds
    /// as much as the file may grow to, `reserved`, and outlives the upload's expiry
    /// until the put ends, so that its slot and room aren't given out again while the
    /// body is still buffered.
    Receiving {
        reserved: usize,
    },
    Received(Bytes),
}

/// Why a token may prepare no more uploads now.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TooMany;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PutRefused {
    /// No upload has the handle: made up, expired, used, or from before a restart.
    Unknown,
    /// Its file was put already, or is being put.
    Taken,
    /// Larger than [`MAX_FILE_BYTES`].
    TooLarge,
    /// The token's uploads, with those being put, hold too much already: `room` is what
    /// is left.
    Full { room: usize },
}

/// Why an upload given to a tool has no file for it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Missing {
    /// None of the token's uploads has the handle.
    Unknown,
    /// Nothing was put to its URL yet.
    NotPut,
}

impl Uploads {
    /// A new upload for `token`, returning its handle.
    pub fn prepare(&self, token: &str, now: Instant) -> Result<String, TooMany> {
        let mut uploads = self.live(now);
        if uploads.values().filter(|u| u.token == token).count() >= PREPARED_PER_TOKEN {
            return Err(TooMany);
        }
        let handle = crate::random_handle();
        uploads.insert(
            handle.clone(),
            Upload {
                token: token.to_owned(),
                prepared_at: now,
                state: State::Waiting,
            },
        );
        Ok(handle)
    }

    /// Starts putting the file of `handle`, `declared` bytes long if the request says:
    /// no one else can while the [`Receiving`] lasts. It holds room for the file as
    /// large as it may be, so a declared length holds only that.
    pub fn begin<'a>(
        &'a self,
        handle: &'a str,
        declared: Option<usize>,
        now: Instant,
    ) -> Result<Receiving<'a>, PutRefused> {
        let mut uploads = self.live(now);
        let upload = uploads.get(handle).ok_or(PutRefused::Unknown)?;
        if !matches!(upload.state, State::Waiting) {
            return Err(PutRefused::Taken);
        }
        let room = room(&uploads, &upload.token).min(MAX_FILE_BYTES);
        let limit = match declared {
            Some(size) if size > MAX_FILE_BYTES => return Err(PutRefused::TooLarge),
            Some(size) if size > room => return Err(PutRefused::Full { room }),
            Some(size) => size,
            None if room == 0 => return Err(PutRefused::Full { room }),
            None => room,
        };
        let upload = uploads.get_mut(handle).expect("found above");
        upload.state = State::Receiving { reserved: limit };
        Ok(Receiving {
            uploads: self,
            handle,
            limit,
            until: upload.prepared_at + UPLOAD_WITHIN,
            put: false,
        })
    }

    /// The file put to `token`'s upload `handle`. It stays until [`used`](Self::used):
    /// a call that fails leaves it for the next.
    pub fn file(&self, handle: &str, token: &str, now: Instant) -> Result<Bytes, Missing> {
        let uploads = self.live(now);
        let upload = uploads
            .get(handle)
            .filter(|u| u.token == token)
            .ok_or(Missing::Unknown)?;
        match &upload.state {
            State::Received(content) => Ok(content.clone()),
            State::Waiting | State::Receiving { .. } => Err(Missing::NotPut),
        }
    }

    /// Forgets the uploads a call has used.
    pub fn used(&self, handles: &[String]) {
        let mut uploads = self.lock();
        for handle in handles {
            uploads.remove(handle);
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Upload>> {
        self.0.lock().expect("uploads")
    }

    /// The uploads, without those expired: each use clears them, so what they hold isn't
    /// kept until the next `prepare_upload`. One being put stays until its
    /// [`Receiving`] ends.
    fn live(&self, now: Instant) -> MutexGuard<'_, HashMap<String, Upload>> {
        let mut uploads = self.lock();
        uploads.retain(|_, u| {
            now.duration_since(u.prepared_at) < UPLOAD_WITHIN
                || matches!(u.state, State::Receiving { .. })
        });
        uploads
    }
}

/// A file being put to an upload's URL. Dropped without [`put`](Self::put) (the body
/// broke off, was too large, took until the upload expired, or the request was cut
/// off), the upload can be put again until it expires.
pub(crate) struct Receiving<'a> {
    uploads: &'a Uploads,
    handle: &'a str,
    /// How large the file may be, and what it holds of the token's room meanwhile: its
    /// declared length, or what the token's uploads have room for, at most a file's
    /// most.
    pub limit: usize,
    /// When the upload expires: the file has to arrive whole before.
    pub until: Instant,
    put: bool,
}

impl Receiving<'_> {
    /// Why a file larger than [`limit`](Self::limit) is refused. Only one without a
    /// declared length can be: a body is never longer than it says.
    pub fn too_large(&self) -> PutRefused {
        if self.limit == MAX_FILE_BYTES {
            PutRefused::TooLarge
        } else {
            PutRefused::Full { room: self.limit }
        }
    }

    /// Keeps the file and returns the name of the upload's token. Its room was held since
    /// [`Uploads::begin`], so the token's uploads can't have come to hold too much; a file
    /// that came by [`until`](Self::until) is kept even if the upload expired just after,
    /// and the next use clears it.
    pub fn put(mut self, content: Bytes) -> Result<String, PutRefused> {
        let mut uploads = self.uploads.lock();
        let upload = uploads.get_mut(self.handle).ok_or(PutRefused::Unknown)?;
        debug_assert!(content.len() <= self.limit, "`Limited` keeps to the limit");
        upload.state = State::Received(content);
        self.put = true;
        Ok(upload.token.clone())
    }
}

impl Drop for Receiving<'_> {
    fn drop(&mut self) {
        if self.put {
            return;
        }
        // Not `lock`: this may run while a panic unwinds.
        if let Ok(mut uploads) = self.uploads.0.lock()
            && let Some(upload) = uploads.get_mut(self.handle)
        {
            // Expired, the next use clears it.
            upload.state = State::Waiting;
        }
    }
}

/// What the files `token` has put and not used, and those it is putting, have room for
/// still.
fn room(uploads: &HashMap<String, Upload>, token: &str) -> usize {
    let held: usize = uploads
        .values()
        .filter(|u| u.token == token)
        .map(|u| match &u.state {
            State::Received(content) => content.len(),
            State::Receiving { reserved } => *reserved,
            State::Waiting => 0,
        })
        .sum();
    HELD_PER_TOKEN.saturating_sub(held)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upload_takes_one_file_for_its_token_in_time() {
        let uploads = Uploads::default();
        let start = Instant::now();
        let handle = uploads.prepare("laptop", start).unwrap();
        assert_eq!(uploads.file(&handle, "laptop", start), Err(Missing::NotPut));
        let receiving = uploads.begin(&handle, None, start).unwrap();
        assert_eq!(receiving.limit, MAX_FILE_BYTES);
        assert_eq!(receiving.until, start + UPLOAD_WITHIN);
        assert_eq!(receiving.too_large(), PutRefused::TooLarge);
        assert_eq!(
            uploads.begin(&handle, None, start).err(),
            Some(PutRefused::Taken)
        );
        receiving.put(Bytes::from_static(b"receipt")).unwrap();
        assert_eq!(
            uploads.begin(&handle, None, start).err(),
            Some(PutRefused::Taken)
        );

        assert_eq!(uploads.file(&handle, "laptop", start).unwrap(), "receipt");
        // Still there for another call, until one that used it succeeds.
        assert!(uploads.file(&handle, "laptop", start).is_ok());
        assert_eq!(uploads.file(&handle, "phone", start), Err(Missing::Unknown));
        uploads.used(std::slice::from_ref(&handle));
        assert_eq!(
            uploads.file(&handle, "laptop", start),
            Err(Missing::Unknown)
        );
        assert_eq!(
            uploads.begin("made up", None, start).err(),
            Some(PutRefused::Unknown)
        );

        let late = uploads.prepare("laptop", start).unwrap();
        assert_eq!(
            uploads.begin(&late, None, start + UPLOAD_WITHIN).err(),
            Some(PutRefused::Unknown)
        );
    }

    #[test]
    fn a_put_that_failed_can_be_tried_again() {
        let uploads = Uploads::default();
        let now = Instant::now();
        let handle = uploads.prepare("laptop", now).unwrap();
        drop(uploads.begin(&handle, None, now).unwrap());
        assert!(uploads.begin(&handle, None, now).is_ok());
    }

    #[test]
    fn a_token_holds_only_so_much_with_the_files_being_put() {
        let uploads = Uploads::default();
        let now = Instant::now();
        let [first, second, third, fourth] =
            [(); 4].map(|()| uploads.prepare("laptop", now).unwrap());
        // Begun together without a length, each holds a whole file's room meanwhile.
        let a = uploads.begin(&first, None, now).unwrap();
        let b = uploads.begin(&second, None, now).unwrap();
        assert_eq!((a.limit, b.limit), (MAX_FILE_BYTES, MAX_FILE_BYTES));
        assert_eq!(
            uploads.begin(&third, None, now).err(),
            Some(PutRefused::Full { room: 0 })
        );
        // One that ends without its file gives it back. A declared length holds only that.
        drop(b);
        let c = uploads.begin(&third, Some(3), now).unwrap();
        assert_eq!(c.limit, 3);
        a.put(vec![0; MAX_FILE_BYTES - 1].into()).unwrap();
        let room = MAX_FILE_BYTES - 2;
        let d = uploads.begin(&fourth, None, now).unwrap();
        assert_eq!(d.limit, room);
        assert_eq!(d.too_large(), PutRefused::Full { room });
        drop(d);
        assert_eq!(
            uploads.begin(&fourth, Some(room + 1), now).err(),
            Some(PutRefused::Full { room })
        );
        assert_eq!(
            uploads.begin(&fourth, Some(MAX_FILE_BYTES + 1), now).err(),
            Some(PutRefused::TooLarge)
        );
        c.put(Bytes::from_static(b"abc")).unwrap();
        assert_eq!(uploads.begin(&fourth, None, now).unwrap().limit, room);

        // Another token's are its own.
        let other = uploads.prepare("phone", now).unwrap();
        assert_eq!(
            uploads.begin(&other, None, now).unwrap().limit,
            MAX_FILE_BYTES
        );

        uploads.used(&[first]);
        assert_eq!(
            uploads.begin(&fourth, None, now).unwrap().limit,
            MAX_FILE_BYTES
        );
    }

    #[test]
    fn a_token_prepares_only_so_many_and_expired_ones_are_forgotten() {
        let uploads = Uploads::default();
        let start = Instant::now();
        for _ in 0..PREPARED_PER_TOKEN {
            uploads.prepare("laptop", start).unwrap();
        }
        assert_eq!(uploads.prepare("laptop", start), Err(TooMany));
        let theirs = uploads.prepare("phone", start).unwrap();
        // Any use clears the expired.
        let later = start + UPLOAD_WITHIN;
        assert_eq!(uploads.file(&theirs, "phone", later), Err(Missing::Unknown));
        assert!(uploads.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_file_being_put_holds_its_slot_and_room_past_the_expiry() {
        let uploads = Uploads::default();
        let start = Instant::now();
        let [first, second] = [(); 2].map(|()| uploads.prepare("laptop", start).unwrap());
        let a = uploads.begin(&first, None, start).unwrap();
        let b = uploads.begin(&second, None, start).unwrap();

        // Expired, the two still being put count until their puts end.
        let later = start + UPLOAD_WITHIN;
        let new = uploads.prepare("laptop", later).unwrap();
        assert_eq!(
            uploads.begin(&new, None, later).err(),
            Some(PutRefused::Full { room: 0 })
        );
        for _ in 3..PREPARED_PER_TOKEN {
            uploads.prepare("laptop", later).unwrap();
        }
        assert_eq!(uploads.prepare("laptop", later), Err(TooMany));

        // Once the puts end, they are forgotten.
        drop((a, b));
        assert_eq!(uploads.file(&first, "laptop", later), Err(Missing::Unknown));
        assert_eq!(
            uploads.begin(&new, None, later).unwrap().limit,
            MAX_FILE_BYTES
        );
        assert!(uploads.prepare("laptop", later).is_ok());
    }
}
