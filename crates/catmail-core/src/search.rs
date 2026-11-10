//! Searching the servers for what local search can't find: the text of messages whose
//! bodies aren't downloaded ([`MailClient::search_server`](crate::MailClient::search_server)).
//!
//! Every location the local search covers is searched in turn, EXAMINE then UID SEARCH,
//! on a connection of the account's own: the pooled ones stay free for opening messages.
//! The one the connection has open already goes first, without EXAMINE. Accounts are
//! searched at once. A frontend may have the connection made ahead of a search
//! ([`connect_ahead`]), which then doesn't wait for the login. What a server finds is mapped back to the rows stored
//! for its folder in the same UIDVALIDITY, and only those without a body are kept: local
//! search finds the others itself. Messages the server has that sync hasn't brought yet
//! are left to sync.
//!
//! A search whose caller stopped waiting for it ([`Abandoned`]) stops after the command at
//! hand and leaves the connection to the next one, so that pauses in typing don't pile
//! searches up on the server. The connection is closed once no search used it for
//! [`IDLE`].
//!
//! A server that can't search the text (one refusing `CHARSET UTF-8`, as some do) or
//! can't be reached adds nothing: the local results are all there are. One that can't
//! search text at all ([`Session::uid_search_text`] or [`Session::heeds_text`] says) isn't
//! asked again. Nor are
//! accounts asked whose login was rejected, or that went over their limit, until a sync
//! gets through: each try would count against them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use catmail_proto::imap::Session;
use catmail_store::FolderSyncState;
use catmail_types::{AccountId, MessageId, SearchQuery, SearchTerm};
use futures_util::future::join_all;

use crate::engine::Engine;
use crate::{Error, Result, pool};

/// How long the search connection stays open after the last search: long enough for the
/// next query typed, or refined, to need no login.
const IDLE: Duration = Duration::from_secs(2 * 60);

/// The most messages taken from what one folder's search finds, of those whose bodies
/// aren't downloaded, the newest (highest UIDs): a word in every message would otherwise
/// carry tens of thousands of ids into every read of the results.
const MAX_PER_FOLDER: usize = 1000;

/// The connection an account's searches use.
#[derive(Default)]
pub(crate) struct SearchSlot(tokio::sync::Mutex<Held>);

#[derive(Default)]
struct Held {
    session: Option<Session>,
    /// Searches that used `session` so far: the idle close leaves it to a later one.
    uses: u64,
    /// When a search found the login rejected, or the account over its limit, which sync
    /// may not have seen yet (see [`Engine::waits_out`]): the server isn't asked again
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
    /// Whether the account's server may be asked now; the log says why not. Forgets a
    /// refusal that a sync got past since.
    fn may_ask(&mut self, engine: &Engine, account: AccountId) -> bool {
        if self.unsearchable {
            tracing::debug!(%account, "not asking the server: it can't search text");
            return false;
        }
        let synced_since = |at| engine.synced(account).is_some_and(|synced| synced > at);
        if engine.waits_out(account) || self.refused.is_some_and(|at| !synced_since(at)) {
            tracing::debug!(%account, "not asking the server: it rejects the login, or the account is over its limit");
            return false;
        }
        self.refused = None;
        true
    }

    /// Remembers a rejected login, or the account over its limit, if `e` is that.
    fn note_refusal(&mut self, e: &Error) {
        if matches!(e, Error::AuthFailed(_) | Error::OverLimit(_)) {
            self.refused = Some(Instant::now());
        }
    }
}

impl SearchSlot {
    /// Closes the connection unless a search is using it.
    pub fn close_idle(&self) {
        if let Ok(mut held) = self.0.try_lock() {
            held.session = None;
        }
    }
}

/// Set once the caller of a search stopped waiting for it: the engine's task runs on
/// regardless of the caller's future (see `Runtime::run`), so it looks.
#[derive(Clone, Default)]
pub(crate) struct Abandoned(Arc<AtomicBool>);

impl Abandoned {
    fn is_set(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Sets it when dropped: kept by the caller's future.
    pub fn on_drop(&self) -> impl Drop + use<> {
        struct Guard(Abandoned);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.0.store(true, Ordering::Relaxed);
            }
        }
        Guard(self.clone())
    }
}

/// Searches the servers of `query`'s accounts (of every account with none) for its text,
/// within its folder if it has one, and returns the messages found whose bodies aren't
/// downloaded, by id; what it found until then once `abandoned`.
pub(crate) async fn on_servers(
    engine: Arc<Engine>,
    query: SearchQuery,
    abandoned: Abandoned,
) -> Result<Vec<MessageId>> {
    let terms = SearchTerm::parse(&query.text);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let (accounts, folder) = (query.accounts, query.folder);
    let accounts = engine
        .read(move |db| db.folders().searched_on_server(accounts.as_deref(), folder))
        .await?;
    let found = join_all(
        accounts
            .into_iter()
            .map(|(account, folders)| in_account(&engine, account, folders, &terms, &abandoned)),
    )
    .await;
    let mut found: Vec<MessageId> = found.into_iter().flatten().collect();
    found.sort_unstable();
    Ok(found)
}

/// Logs in to the servers [`on_servers`] would ask about `account`'s messages (every
/// account's with `None`) ahead of a search, so that it doesn't wait for the login: the
/// connection is the searches' own, closed after [`IDLE`] unless one used it. An account
/// whose connection is open, or in use (a search under way has one, or is making one),
/// is left alone. Failures are only logged; a rejected login keeps searches from trying
/// again, as their own would.
pub(crate) async fn connect_ahead(engine: Arc<Engine>, account: Option<AccountId>) -> Result<()> {
    let accounts = engine
        .read(move |db| {
            db.folders()
                .searched_on_server(account.as_ref().map(std::slice::from_ref), None)
        })
        .await?;
    join_all(
        accounts
            .into_iter()
            .map(|(account, _)| connect_account_ahead(&engine, account)),
    )
    .await;
    Ok(())
}

async fn connect_account_ahead(engine: &Arc<Engine>, account: AccountId) {
    let slot = engine.search_slot(account);
    let Ok(mut held) = slot.0.try_lock() else {
        return;
    };
    if !held.may_ask(engine, account) {
        return;
    }
    // Held meanwhile: a search starting now waits for this connection, not one of its own.
    // One open already is left as it is.
    let mut cancellation = engine.cancellation(account);
    let result = tokio::select! {
        result = pool::on_session(engine, account, &mut held.session, async |_| Ok(())) => result,
        () = cancellation.cancelled() => return,
    };
    if let Err(e) = result {
        tracing::info!(%account, error = %e, kind = ?e.kind(),
            "couldn't connect ahead of a search");
        held.note_refusal(&e);
        return;
    }
    held.uses += 1;
    let uses = held.uses;
    drop(held);
    close_when_idle(engine, account, uses);
}

/// What one account's server found in `folders`; nothing if it couldn't be asked, and
/// what it found until then if it failed on the way. Failures are only logged.
async fn in_account(
    engine: &Arc<Engine>,
    account: AccountId,
    folders: Vec<FolderSyncState>,
    terms: &[SearchTerm],
    abandoned: &Abandoned,
) -> Vec<MessageId> {
    let mut cancellation = engine.cancellation(account);
    let slot = engine.search_slot(account);
    // An abandoned search finishes its command first.
    let mut held = slot.0.lock().await;
    if abandoned.is_set() || !held.may_ask(engine, account) {
        return Vec::new();
    }
    held.uses += 1;
    let uses = held.uses;
    let began = Instant::now();
    let mut progress = Progress::default();
    let result = tokio::select! {
        result = in_folders(engine, account, &mut held.session, &folders, terms, abandoned, &mut progress) => result,
        () = cancellation.cancelled() => Err(Error::Cancelled),
    };
    // Which syntax the server got, known once connected; never the text searched.
    let syntax = held
        .session
        .as_ref()
        .map(|s| tracing::field::debug(s.text_search()));
    let charset = terms.iter().any(|t| !t.text.is_ascii());
    let error = result.err();
    if let Some(e) = &error {
        // Left in the middle of a command, or broken.
        if matches!(e, Error::Cancelled) || e.breaks_connection() {
            held.session = None;
        }
        held.note_refusal(e);
    }
    if progress.unsearchable {
        held.unsearchable = true;
        held.session = None;
    }
    // What tells "the server found nothing" (matched) from "it found only what is here"
    // (found), and where the time went: opening folders or the server searching them.
    tracing::info!(%account, searched = progress.searched, of = folders.len(),
        matched = progress.matched, found = progress.found.len(), syntax, charset,
        terms = terms.len(), opened = progress.opened, examine = ?progress.examine,
        search = ?progress.search,
        abandoned = abandoned.is_set(), error = error.as_ref().map(tracing::field::display),
        kind = error.as_ref().map(|e| tracing::field::debug(e.kind())),
        took = ?began.elapsed(), "searched the server");
    drop(held);
    close_when_idle(engine, account, uses);
    progress.found
}

/// What the search of an account came to so far: kept when it fails on the way.
#[derive(Default)]
struct Progress {
    found: Vec<MessageId>,
    /// Folders searched, and the messages the server matched in them, with a body or not.
    searched: usize,
    matched: usize,
    /// Folders of those that had to be opened (the others were open already), the time
    /// spent opening them, and the server searching all of them.
    opened: usize,
    examine: Duration,
    search: Duration,
    /// The server turned out unable to search text.
    unsearchable: bool,
}

/// Searches `folders` one by one on the connection in `session` (connecting if it is
/// empty), adding what each finds to `progress`, until the search is abandoned.
async fn in_folders(
    engine: &Arc<Engine>,
    account: AccountId,
    session: &mut Option<Session>,
    folders: &[FolderSyncState],
    terms: &[SearchTerm],
    abandoned: &Abandoned,
    progress: &mut Progress,
) -> Result<()> {
    // The one the connection has open first, which needs no EXAMINE: the last searched,
    // so each search of several folders opens one fewer.
    let open = session.as_ref().and_then(|s| s.examined_mailbox());
    let mut folders: Vec<&FolderSyncState> = folders.iter().collect();
    folders.sort_by_key(|f| open.is_none_or(|(name, _)| f.remote_name != name));
    for state in folders {
        if abandoned.is_set() {
            return Ok(());
        }
        // Owned, not borrowed: see `pool::with_session`.
        let (remote, synced, terms) =
            (state.remote_name.clone(), state.uidvalidity, terms.to_vec());
        let abandoned = abandoned.clone();
        let began = Instant::now();
        let searched = pool::on_session(engine, account, session, async move |s| {
            let began = Instant::now();
            // Its UIDVALIDITY holds while it is open; what else EXAMINE would bring
            // (EXISTS) the connection keeps up to date. Not if sync found the folder
            // rebuilt since: only a command lets the server end the connection, as it
            // should then, or EXAMINE tell of the new generation.
            let examined = s
                .examined_mailbox()
                .filter(|&(name, uidvalidity)| name == remote && synced == Some(uidvalidity))
                .map(|(_, uidvalidity)| uidvalidity);
            let (uidvalidity, examine) = match examined {
                Some(uidvalidity) => (uidvalidity, None),
                None => match s.examine(&remote).await {
                    Ok(status) => (status.uidvalidity, Some(began.elapsed())),
                    Err(e) if !e.is_fatal() => return Ok(Searched::Unopened(e.into())),
                    Err(e) => return Err(e.into()),
                },
            };
            if synced != Some(uidvalidity) {
                return Ok(Searched::Rebuilt);
            }
            // Searching the text may scan the whole folder.
            if abandoned.is_set() {
                return Ok(Searched::Abandoned);
            }
            let began = Instant::now();
            let refused = |e: catmail_proto::imap::Error| -> Result<Searched> {
                match e.is_fatal() {
                    false => Ok(Searched::Refused(e.into())),
                    true => Err(e.into()),
                }
            };
            let uids = match s.uid_search_text(&terms).await {
                Ok(Some(uids)) => uids,
                Ok(None) => return Ok(Searched::Unsearchable),
                Err(e) => return refused(e),
            };
            // Making sure of them may take another search of the text.
            if abandoned.is_set() {
                return Ok(Searched::Abandoned);
            }
            match s.heeds_text(&terms, &uids).await {
                Ok(true) => Ok(Searched::Found {
                    uids,
                    examine,
                    search: began.elapsed(),
                }),
                Ok(false) => Ok(Searched::Unsearchable),
                Err(e) => refused(e),
            }
        })
        .await?;
        let folder = &state.name;
        match searched {
            Searched::Found {
                uids,
                examine,
                search,
            } => {
                let matched = uids.len();
                let (id, uidvalidity) = (state.id, synced.unwrap_or_default());
                let ids = engine
                    .read(move |db| {
                        let messages = db.messages();
                        messages.without_body(id, uidvalidity, &uids, MAX_PER_FOLDER)
                    })
                    .await?;
                tracing::debug!(%account, folder, matched, without_body = ids.len(), ?examine,
                    ?search, took = ?began.elapsed(), "searched a folder on the server");
                progress.searched += 1;
                progress.matched += matched;
                if let Some(examine) = examine {
                    progress.opened += 1;
                    progress.examine += examine;
                }
                progress.search += search;
                progress.found.extend(ids);
            }
            Searched::Abandoned => return Ok(()),
            // Its messages are about to be synced again.
            Searched::Rebuilt => {
                tracing::debug!(%account, folder, "folder rebuilt on the server; not searched");
            }
            Searched::Unopened(e) => {
                tracing::debug!(%account, folder, error = %e, "couldn't open the folder to search it");
            }
            // As it would be in every other folder.
            Searched::Refused(e) => {
                tracing::info!(%account, folder, error = %e,
                    "the server refused to search; no more of its folders searched");
                return Ok(());
            }
            Searched::Unsearchable => {
                tracing::info!(%account, folder, "the server can't search text; not asked again");
                progress.unsearchable = true;
                return Ok(());
            }
        }
    }
    Ok(())
}

/// What came of searching a folder.
enum Searched {
    /// The UIDs matching, ascending, and how long opening the folder (`None` if it was
    /// open already) and searching it took.
    Found {
        uids: Vec<u32>,
        examine: Option<Duration>,
        search: Duration,
    },
    /// Abandoned once the folder was open, or before making sure of what was found.
    Abandoned,
    /// Its UIDVALIDITY on the server isn't the one synced.
    Rebuilt,
    /// The server wouldn't open it.
    Unopened(Error),
    /// The server wouldn't search it.
    Refused(Error),
    /// The server can't search text, in any folder.
    Unsearchable,
}

/// Closes the account's search connection after [`IDLE`], unless another search used it
/// after the one numbered `uses`. Holds the engine weakly meanwhile.
fn close_when_idle(engine: &Arc<Engine>, account: AccountId, uses: u64) {
    let (weak, mut closing): (Weak<Engine>, _) = (Arc::downgrade(engine), engine.closing());
    tokio::spawn(async move {
        tokio::select! {
            () = tokio::time::sleep(IDLE) => {}
            () = closing.closed() => return,
        }
        let Some(slot) = weak.upgrade().map(|engine| engine.search_slot(account)) else {
            return;
        };
        if let Ok(mut held) = slot.0.try_lock()
            && held.uses == uses
            && held.session.take().is_some()
        {
            tracing::debug!(%account, "closed the idle search connection");
        }
    });
}
