//! The bodies being fetched for someone waiting on them: held by the engine, so that
//! fetches of the same body by [`body`](crate::body) and the background prefetch don't race.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use catmail_types::MessageId;
use tokio::sync::watch;

use crate::task::lock;

/// Bodies being fetched for someone waiting on them (a message opened, new mail fetched
/// as it arrives), so that another fetch of the same body waits for that one rather than
/// fetching it too, on another connection. Not the background prefetch's, which stores
/// a batch at a time: it leaves these out instead.
#[derive(Default)]
pub(crate) struct Downloading(Claims);

/// The bodies claimed, each with what ends when its claim is released.
type Claims = Arc<Mutex<HashMap<MessageId, watch::Receiver<()>>>>;

/// Whose fetch of a body it is, as [`Downloading::claim`] says.
pub(crate) enum Claim {
    /// This caller's, until the claim is dropped: once the body is stored, or the fetch
    /// failed or was given up on.
    Mine(Claimed),
    /// Another's, released (the body stored, or not after all) once this sees a change,
    /// which is never sent: the sender is dropped.
    Theirs(watch::Receiver<()>),
}

/// A body claimed for fetching (see [`Downloading`]); released when dropped.
pub(crate) struct Claimed {
    set: Claims,
    id: MessageId,
    /// Dropped after the claim is released: those waiting wake to find it gone.
    _done: watch::Sender<()>,
}

impl Drop for Claimed {
    fn drop(&mut self) {
        lock(&self.set).remove(&self.id);
    }
}

impl Downloading {
    /// Claims the body of `id` for fetching, unless another fetch has it.
    pub fn claim(&self, id: MessageId) -> Claim {
        let mut set = lock(&self.0);
        if let Some(theirs) = set.get(&id) {
            return Claim::Theirs(theirs.clone());
        }
        let (done, waiting) = watch::channel(());
        set.insert(id, waiting);
        Claim::Mine(Claimed {
            set: self.0.clone(),
            id,
            _done: done,
        })
    }

    /// Whether some fetch has the body of `id` claimed.
    pub fn claimed(&self, id: MessageId) -> bool {
        lock(&self.0).contains_key(&id)
    }
}
