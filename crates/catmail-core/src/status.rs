//! What the latest attempts to get through to an account's servers found
//! ([`AccountStatus`]), and the failures in a row that background sync and the outbox
//! time their retries by. The engine keeps one per account, locked, and announces what
//! changes (see `Engine::report`).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use catmail_types::{
    AccountStatus, ErrorKind, IdentityId, ReportedError, SendProblem, ServerProblem,
};

use crate::Error;
use crate::task::{backoff, now};

/// [`AccountStatus`], with the send problems by identity, and the failures since the
/// computer last woke of each server.
#[derive(Debug)]
pub(crate) struct Status {
    syncing: bool,
    sync: Option<ServerProblem>,
    send: BTreeMap<IdentityId, SendProblem>,
    sync_after_wake: AfterWake,
    send_after_wake: BTreeMap<IdentityId, AfterWake>,
    /// When the account's state was made, as if its servers were tried then.
    made: Instant,
}

/// Failures in a row of one server since the computer last woke (see
/// [`Failures::after_wake`]).
#[derive(Debug)]
struct AfterWake {
    /// When a wake was last looked for.
    checked: Instant,
    failures: Option<u32>,
}

impl AfterWake {
    fn new(checked: Instant) -> Self {
        Self {
            checked,
            failures: None,
        }
    }

    /// Takes in how an attempt ended, whether it `reached` the server or not;
    /// `woke_after(at)` is whether the computer woke after `at`. Returns the failures
    /// since the wake.
    fn saw(&mut self, reached: bool, woke_after: impl FnOnce(Instant) -> bool) -> Option<u32> {
        // Taken before looking: a wake in between is seen again next time, not never.
        let now = Instant::now();
        if woke_after(self.checked) {
            self.failures = Some(0);
        }
        self.checked = now;
        self.failures = if reached {
            None
        } else {
            self.failures.map(|n| n + 1)
        };
        self.failures
    }
}

/// Failures in a row to get through to a server, as [`Status::record`] counts them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Failures {
    /// This one included; 0 for a success.
    pub in_a_row: u32,
    /// Those since the computer last woke, if none got through to the server since (a
    /// rejected login did, as did an over-limit reply): they are tried again sooner, as
    /// the network is often back only a few seconds after the computer. Those before the
    /// wake say nothing about the network after.
    pub after_wake: Option<u32>,
}

impl Failures {
    /// How long to wait before trying again: from `first`, doubling up to `max`. After
    /// the computer woke, from `soon` first, doubling while that is sooner than `first`
    /// (and `max`), then from `first` as usual.
    pub fn retry_in(self, soon: Duration, first: Duration, max: Duration) -> Duration {
        let Some(failures) = self.after_wake else {
            return backoff(first, self.in_a_row, max);
        };
        let cap = first.min(max);
        let quick = |n| backoff(soon, n, cap);
        // Bounded for a zero `soon`, which never reaches `cap`.
        let quick_tries = (1..=u32::BITS).take_while(|&n| quick(n) < cap).count() as u32;
        if failures <= quick_tries {
            quick(failures)
        } else {
            backoff(first, failures - quick_tries, max)
        }
    }
}

/// What an attempt that [`Status::record`] records was doing.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Activity<'a> {
    Sync,
    /// A sync begun then that syncs every folder whenever that is due: a step of
    /// background sync, or `sync_account`. Every folder is due once a sync of them all
    /// was asked for (see [`Status::begin_syncing`]), so one begun after that answers it:
    /// ends [`AccountStatus::syncing`] unless asked again since. Any sync that finds the
    /// login rejected ends it too.
    SyncBegun(Instant),
    /// As this identity of the account, with the address it sends as.
    Send(IdentityId, &'a str),
}

/// What [`Status::record`] changed.
pub(crate) struct Recorded {
    /// Whether the [`AccountStatus`] did.
    pub changed: bool,
    /// Whether the problem getting through did: what is logged.
    pub problem_changed: bool,
    /// For a success, whether background sync waited out the failure before it (a
    /// rejected login, an account over its limit).
    pub waited: bool,
    pub failures: Failures,
}

impl Status {
    pub fn new() -> Self {
        let made = Instant::now();
        Self {
            syncing: false,
            sync: None,
            send: BTreeMap::new(),
            sync_after_wake: AfterWake::new(made),
            send_after_wake: BTreeMap::new(),
            made,
        }
    }

    /// Records how an attempt for `activity` ended, `e` if it failed. `answered(began)`
    /// is whether a sync of every folder begun then answers the last ask for one (see
    /// [`begin_syncing`](Self::begin_syncing)); `woke_after(at)`, whether the computer
    /// woke after `at`.
    pub fn record(
        &mut self,
        activity: Activity,
        e: Option<&Error>,
        answered: impl FnOnce(Instant) -> bool,
        woke_after: impl FnOnce(Instant) -> bool,
    ) -> Recorded {
        let made = self.made;
        let after_wake = match activity {
            Activity::Sync | Activity::SyncBegun(_) => &mut self.sync_after_wake,
            Activity::Send(identity, _) => self
                .send_after_wake
                .entry(identity)
                .or_insert_with(|| AfterWake::new(made)),
        };
        // A rejected login got through to the server: the network is back, and trying
        // again soon would only bring on more rejections, which servers count towards
        // locking the account. So did an over-limit reply, which retrying soon would only
        // add to.
        let reached = e.is_none_or(|e| matches!(e.kind(), ErrorKind::Auth | ErrorKind::OverLimit));
        let after_wake = after_wake.saw(reached, woke_after);
        let (changed, waited, in_a_row, problem_changed) = match activity {
            Activity::Sync | Activity::SyncBegun(_) => {
                let (changed, waited, failures) = record(&mut self.sync, e);
                let answered = match activity {
                    Activity::SyncBegun(began) => answered(began),
                    _ => false,
                };
                // Nor is it answered while the login is rejected, which background sync
                // waits out (see `Engine::wake`), whenever the rejected attempt began.
                let done = self.syncing && (answered || self.login_rejected());
                if done {
                    self.syncing = false;
                }
                (changed || done, waited, failures, changed)
            }
            Activity::Send(identity, email) => {
                let mut slot = self.send.remove(&identity).map(|p| p.problem);
                let recorded = record(&mut slot, e);
                if let Some(problem) = slot {
                    let email = email.to_owned();
                    let problem = SendProblem {
                        identity,
                        email,
                        problem,
                    };
                    self.send.insert(identity, problem);
                }
                let (changed, waited, failures) = recorded;
                (changed, waited, failures, changed)
            }
        };
        Recorded {
            changed,
            problem_changed,
            waited,
            failures: Failures {
                in_a_row,
                after_wake,
            },
        }
    }

    /// Puts the account in syncing, until an attempt begun after this ends: returns
    /// whether it did. Not while it is syncing already, nor while the server rejects the
    /// login, which waits as long as ever.
    pub fn begin_syncing(&mut self) -> bool {
        if self.syncing || self.login_rejected() {
            return false;
        }
        self.syncing = true;
        true
    }

    /// Drops what was found about sending as an identity that is gone. Returns whether the
    /// [`AccountStatus`] changed.
    pub fn forget_identity(&mut self, identity: IdentityId) -> bool {
        self.send_after_wake.remove(&identity);
        self.send.remove(&identity).is_some()
    }

    /// Whether the latest attempt to sync found the login rejected.
    pub fn login_rejected(&self) -> bool {
        self.sync
            .as_ref()
            .is_some_and(ServerProblem::login_rejected)
    }

    /// Whether the latest attempt to sync found a failure background sync waits out
    /// ([`ServerProblem::waited_out`]).
    pub fn waits_out(&self) -> bool {
        self.sync.as_ref().is_some_and(ServerProblem::waited_out)
    }

    /// Whether background sync ignores wakes for now (see `Engine::ignores_wakes`).
    pub fn ignores_wakes(&self, logged_in: bool) -> bool {
        self.sync.as_ref().is_some_and(|problem| {
            (problem.login_rejected() && !logged_in) || (problem.over_limit() && !self.syncing)
        })
    }

    /// What frontends show.
    pub fn shown(&self) -> AccountStatus {
        AccountStatus {
            syncing: self.syncing,
            sync: self.sync.clone(),
            send: self.send.values().cloned().collect(),
        }
    }
}

/// Records an attempt's outcome (`None`: success) in what is known of a server. Returns
/// whether that changed, whether a success follows a failure background sync waits out
/// (a rejected login, an account over its limit), and the failures in a row.
fn record(slot: &mut Option<ServerProblem>, e: Option<&Error>) -> (bool, bool, u32) {
    match e {
        None => {
            let cleared = slot.take();
            let waited = cleared.as_ref().is_some_and(ServerProblem::waited_out);
            (cleared.is_some(), waited, 0)
        }
        Some(e) => {
            let error = ReportedError::from(e);
            // Not each failure: what is wrong is what frontends show.
            match slot {
                Some(problem) => {
                    let changed = problem.error != error;
                    problem.failures += 1;
                    problem.error = error;
                    (changed, false, problem.failures)
                }
                None => {
                    *slot = Some(ServerProblem {
                        error,
                        since: now(),
                        failures: 1,
                    });
                    (true, false, 1)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_after_a_wake_are_tried_again_soon_then_as_usual() {
        let secs = Duration::from_secs;
        let retry_in = |after_wake| {
            let failures = Failures {
                in_a_row: 9,
                after_wake,
            };
            failures.retry_in(secs(2), secs(30), secs(900))
        };
        let mut after_wake = AfterWake::new(Instant::now());
        // Failing since before the computer slept.
        assert_eq!(after_wake.saw(false, |_| false), None);
        assert_eq!(retry_in(None), secs(900));

        let mut woke = true;
        let delays: Vec<_> = (0..7)
            .map(|_| retry_in(after_wake.saw(false, |_| std::mem::take(&mut woke))))
            .collect();
        assert_eq!(delays, [2, 4, 8, 16, 30, 60, 120].map(secs));

        // Through again: failures later back off as usual.
        assert_eq!(after_wake.saw(true, |_| false), None);
        assert_eq!(after_wake.saw(false, |_| false), None);

        // Never longer than `max`, even when it is shorter than `first`.
        let failures = Failures {
            in_a_row: 1,
            after_wake: Some(1),
        };
        assert_eq!(failures.retry_in(secs(2), secs(30), secs(1)), secs(1));
    }
}
