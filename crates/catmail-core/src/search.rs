//! Searching the servers for what local search can't find: the text of messages whose
//! bodies aren't downloaded ([`MailClient::search_server`](crate::MailClient::search_server)).
//!
//! Every location the local search covers is searched in turn, EXAMINE then UID SEARCH,
//! on a connection of the account's own: the pooled ones stay free for opening messages.
//! Accounts are searched at once. What a server finds is mapped back to the rows stored
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
//! can't be reached adds nothing: the local results are all there are. Nor are accounts
//! asked whose login was rejected, or that went over their limit, until a sync gets
//! through: each try would count against them.

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

/// Searches the servers of `query`'s account (of every account with none) for its text,
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
    let (account, folder) = (query.account, query.folder);
    let accounts = engine
        .read(move |db| db.folders().searched_on_server(account, folder))
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
    if abandoned.is_set() {
        return Vec::new();
    }
    let synced_since = |at| engine.synced(account).is_some_and(|synced| synced > at);
    if engine.waits_out(account) || held.refused.is_some_and(|at| !synced_since(at)) {
        tracing::debug!(%account, "not searching the server: it rejects the login, or the account is over its limit");
        return Vec::new();
    }
    held.refused = None;
    held.uses += 1;
    let uses = held.uses;
    let began = Instant::now();
    let mut progress = Progress::default();
    let result = tokio::select! {
        result = in_folders(engine, account, &mut held.session, &folders, terms, abandoned, &mut progress) => result,
        () = cancellation.cancelled() => Err(Error::Cancelled),
    };
    // Which syntax the server got, known once connected; never the text searched.
    let syntax = held.session.as_ref().map(|s| match s.is_gmail() {
        true => "X-GM-RAW",
        false => "TEXT",
    });
    let charset = terms.iter().any(|t| !t.text.is_ascii());
    let error = result.err();
    if let Some(e) = &error {
        // Left in the middle of a command, or broken.
        if matches!(e, Error::Cancelled) || e.breaks_connection() {
            held.session = None;
        }
        if matches!(e, Error::AuthFailed(_) | Error::OverLimit(_)) {
            held.refused = Some(Instant::now());
        }
    }
    // What tells "the server found nothing" (matched) from "it found only what is here"
    // (found), and where the time went: opening folders or the server searching them.
    tracing::info!(%account, searched = progress.searched, of = folders.len(),
        matched = progress.matched, found = progress.found.len(), syntax, charset,
        terms = terms.len(), examine = ?progress.examine, search = ?progress.search,
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
    /// Time spent opening the folders searched, and the server searching them.
    examine: Duration,
    search: Duration,
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
            let status = match s.examine(&remote).await {
                Ok(status) => status,
                Err(e) if !e.is_fatal() => return Ok(Searched::Unopened(e.into())),
                Err(e) => return Err(e.into()),
            };
            if !status.same_generation(synced) {
                return Ok(Searched::Rebuilt);
            }
            // Searching the text may scan the whole folder.
            if abandoned.is_set() {
                return Ok(Searched::Abandoned);
            }
            let (examine, began) = (began.elapsed(), Instant::now());
            match s.uid_search_text(&terms).await {
                Ok(uids) => Ok(Searched::Found {
                    uids,
                    examine,
                    search: began.elapsed(),
                }),
                Err(e) if !e.is_fatal() => Ok(Searched::Refused(e.into())),
                Err(e) => Err(e.into()),
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
                progress.examine += examine;
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
        }
    }
    Ok(())
}

/// What came of searching a folder.
enum Searched {
    /// The UIDs matching, ascending, and how long opening the folder and searching it
    /// took.
    Found {
        uids: Vec<u32>,
        examine: Duration,
        search: Duration,
    },
    /// Abandoned once the folder was open.
    Abandoned,
    /// Its UIDVALIDITY on the server isn't the one synced.
    Rebuilt,
    /// The server wouldn't open it.
    Unopened(Error),
    /// The server wouldn't search it.
    Refused(Error),
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
