//! What the engine is set to do, as the builder was told: the sizes and intervals it
//! works in, and how it syncs and fetches bodies in the background. Plain data, read by
//! the modules that do the work.

use std::time::Duration;

/// What the builder set for the engine, besides its storage: what it does in the
/// background, and how.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EngineSettings {
    /// Set if every account is kept in sync in the background (see `watch`).
    pub sync_in_background: Option<SyncSchedule>,
    /// Set if bodies are fetched in the background (see `prefetch`).
    pub prefetch: Option<BodyPrefetch>,
    /// How long background sync leaves a draft alone after it changed before uploading
    /// it (see [`Engine::held_drafts`](crate::engine::Engine::held_drafts)).
    pub draft_upload_delay: Duration,
    pub tuning: EngineTuning,
}

/// [`EngineSettings::draft_upload_delay`] unless the builder sets it.
const DEFAULT_DRAFT_UPLOAD_DELAY: Duration = Duration::from_secs(15);

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            sync_in_background: None,
            prefetch: None,
            draft_upload_delay: DEFAULT_DRAFT_UPLOAD_DELAY,
            tuning: EngineTuning::default(),
        }
    }
}

/// Sizes and intervals the engine works in, which tests and users on metered connections
/// may want other than the defaults. Set with
/// [`MailClientBuilder::tuning`](crate::MailClientBuilder::tuning).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineTuning {
    /// New messages fetched per UID FETCH, and stored per transaction [200].
    pub fetch_batch: usize,
    /// Queued changes made on the server per command: UID sets of scattered messages
    /// get long [500].
    pub replay_batch: usize,
    /// Messages up to this size (RFC822.SIZE) are fetched whole when opened; above it,
    /// attachments are left on the server until opened, and text parts above it are not
    /// fetched at all [2 MiB].
    pub whole_message_limit: u32,
    /// A connection kept for on-demand fetches is sent a NOOP after this long unused, so
    /// that servers and NAT boxes don't close it as idle, and one that died is closed
    /// before it is needed [4 minutes].
    pub keepalive: Duration,
    /// How long after the client opens the bodies stored by an older version of the
    /// parsing and sanitizing are made again, so that search finds them by their text as
    /// parsed now [2 minutes: after the first sync has started, which it then holds up
    /// for only a moment at a time]. A client closed before then leaves them for the
    /// next one; opening a message rebuilds its body anyway.
    pub rebuild_delay: Duration,
}

impl Default for EngineTuning {
    fn default() -> Self {
        Self {
            fetch_batch: 200,
            replay_batch: 500,
            whole_message_limit: 2 * 1024 * 1024,
            // Under five minutes: quiet connections lasted that long when the pool still
            // dropped them at that age. Most servers wait far longer (Gmail some 30
            // minutes); NetEase closes one quiet for a minute, which the pool leaves be
            // (#227).
            keepalive: Duration::from_secs(4 * 60),
            rebuild_delay: Duration::from_secs(2 * 60),
        }
    }
}

impl EngineTuning {
    /// With the batch sizes at least 1, which chunking needs.
    pub(crate) fn checked(self) -> Self {
        Self {
            fetch_batch: self.fetch_batch.max(1),
            replay_batch: self.replay_batch.max(1),
            ..self
        }
    }
}

/// When background sync does what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncSchedule {
    /// Every folder is synced this often; in between only INBOX is watched
    /// [15 minutes].
    pub full_sync: Duration,
    /// How often INBOX is checked on a server that can't announce changes (no IDLE)
    /// [2 minutes].
    pub poll: Duration,
    /// While waiting other than in IDLE (between checks, or before trying again), the
    /// connection is sent a NOOP this often, so that it is still open for the next step
    /// [45 seconds: NetEase closes one quiet for a minute]. Between polls, a change to
    /// INBOX the server reports in answer to the NOOP ends the wait early.
    pub keepalive: Duration,
    /// How long one IDLE lasts before it is started over, so that a connection that died
    /// without a word (a NAT box forgetting it, say) is noticed [10 minutes].
    pub idle: Duration,
    /// How long to wait after a failure before trying again, doubling with each failure
    /// in a row, up to `full_sync` [30 seconds]. Queued local changes try again at once,
    /// except after the server rejected the login: that waits `full_sync`, whatever is
    /// queued, or until a sync asked for gets through. An account over its download limit
    /// waits `over_limit`.
    pub retry: Duration,
    /// After the computer woke, how long to wait after the first failure, doubling while
    /// that is sooner than `retry`; failures then back off from `retry` as usual
    /// [2 seconds]. The network is often back only a few seconds after the computer.
    pub retry_after_wake: Duration,
    /// How long to wait after the server said the account went over its download limit
    /// [1 hour]: Gmail usually lifts it after an hour, and both it and NetEase within a
    /// day; each try in between may count towards it. Queued local changes wait too,
    /// but a sync asked for or waking from sleep tries again at once.
    pub over_limit: Duration,
}

impl Default for SyncSchedule {
    fn default() -> Self {
        Self {
            full_sync: Duration::from_secs(15 * 60),
            poll: Duration::from_secs(2 * 60),
            keepalive: Duration::from_secs(45),
            idle: Duration::from_secs(10 * 60),
            retry: Duration::from_secs(30),
            retry_after_wake: Duration::from_secs(2),
            over_limit: Duration::from_secs(60 * 60),
        }
    }
}

/// How the background prefetch of bodies goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyPrefetch {
    /// Bytes fetched per account and day (UTC) on a Gmail server, at most [1 GiB]. Gmail
    /// suspends IMAP for an hour up to a day past 2500 MB downloaded in one, counting
    /// other clients and messages opened, and the day it counts needn't be UTC's.
    pub gmail_daily_budget: u64,
    /// The same on other servers [200 MiB]. Most don't publish a limit; NetEase's (163,
    /// 126) is known to exist, and to be hit by syncing a few thousand messages.
    pub daily_budget: u64,
    /// Messages up to this size (RFC822.SIZE) are fetched whole, attachments and all;
    /// above it, only their text [256 KiB]. Most mail is smaller, and a message fetched
    /// whole opens offline with everything in it.
    pub whole_limit: u32,
    /// Bodies stored in one transaction and announced together [20].
    pub batch: usize,
    /// How long to wait after the connection or the login failed before trying again,
    /// doubling with each failure in a row, up to 30 minutes [30 seconds].
    pub retry: Duration,
}

impl Default for BodyPrefetch {
    fn default() -> Self {
        Self {
            gmail_daily_budget: 1024 * 1024 * 1024,
            daily_budget: 200 * 1024 * 1024,
            whole_limit: 256 * 1024,
            batch: 20,
            retry: Duration::from_secs(30),
        }
    }
}
