//! The connection an account's searches on the server use, and what they found out about
//! the server: held by the engine for each account, used by [`search`](crate::search).

use std::time::Instant;

use catmail_proto::imap::Session;
use catmail_types::AccountId;
use tokio::sync::MutexGuard;

use crate::Error;

/// The connection an account's searches use.
#[derive(Default)]
pub(crate) struct SearchSlot(tokio::sync::Mutex<Held>);

impl SearchSlot {
    /// Waits for the search using the connection, if one does.
    pub async fn lock(&self) -> MutexGuard<'_, Held> {
        self.0.lock().await
    }

    /// The connection, unless a search is using it.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, Held>> {
        self.0.try_lock().ok()
    }

    /// Closes the connection unless a search is using it.
    pub fn close_idle(&self) {
        if let Some(mut held) = self.try_lock() {
            held.take();
        }
    }

    /// Closes the connection unless a search is using it, or one used it after the one
    /// [`begin`](Held::begin) numbered `uses`. Whether it closed one.
    pub fn close_unused(&self, uses: u64) -> bool {
        self.try_lock()
            .is_some_and(|mut held| held.unused_since(uses) && held.take().is_some())
    }
}

#[derive(Default)]
pub(crate) struct Held {
    session: Option<Session>,
    /// Searches that used `session` so far: the idle close leaves it to a later one.
    uses: u64,
    /// When a search found the login rejected, or the account over its limit, which sync
    /// may not have seen yet (see `Engine::waits_out`): the server isn't asked again
    /// until a sync of every folder gets through after that, as each try counts against
    /// the account.
    refused: Option<Instant>,
    /// The server turned out unable to search text, or to ignore it: asking it again
    /// would only cost the round trips. One recorded on connecting as unable to isn't
    /// connected to at all (see `Folders::searched_on_server`): this is for one found out
    /// on a search's own first connection, and for one ignoring the text, which is only
    /// ever kept here.
    unsearchable: bool,
}

impl Held {
    /// Whether the account's server may be asked now; the log says why not. `waits_out`:
    /// whether sync waits out a rejected login or the account over its limit; `synced`:
    /// when a sync of every folder last got through. Forgets a refusal that a sync got
    /// past since.
    pub fn may_ask(
        &mut self,
        account: AccountId,
        waits_out: bool,
        synced: Option<Instant>,
    ) -> bool {
        if self.unsearchable {
            tracing::debug!(%account, "not asking the server: it can't search text");
            return false;
        }
        let synced_since = |at| synced.is_some_and(|synced| synced > at);
        if waits_out || self.refused.is_some_and(|at| !synced_since(at)) {
            tracing::debug!(%account, "not asking the server: it rejects the login, or the account is over its limit");
            return false;
        }
        self.refused = None;
        true
    }

    /// The connection, if there is one, lent to a search: it gives it back with
    /// [`ended`](Self::ended), which keeps it unless the search broke it.
    pub fn take(&mut self) -> Option<Session> {
        self.session.take()
    }

    /// Counts a use of the connection. Returns its number, for
    /// [`close_unused`](SearchSlot::close_unused).
    pub fn begin(&mut self) -> u64 {
        self.uses += 1;
        self.uses
    }

    /// Whether no search used the connection after the one numbered `uses`.
    fn unused_since(&self, uses: u64) -> bool {
        self.uses == uses
    }

    /// Takes back the connection lent with [`take`](Self::take), or made meanwhile, after
    /// a search ended: with `error`, and having found the server `unsearchable` or not.
    pub fn ended(&mut self, session: Option<Session>, error: Option<&Error>, unsearchable: bool) {
        let mut keep = !unsearchable;
        if let Some(e) = error {
            // Left in the middle of a command, or broken.
            if matches!(e, Error::Cancelled) || e.breaks_connection() {
                keep = false;
            }
            self.note_refusal(e);
        }
        if unsearchable {
            self.unsearchable = true;
        }
        self.session = session.filter(|_| keep);
    }

    /// Remembers a rejected login, or the account over its limit, if `e` is that.
    fn note_refusal(&mut self, e: &Error) {
        if matches!(e, Error::AuthFailed(_) | Error::OverLimit(_)) {
            self.refused = Some(Instant::now());
        }
    }
}
