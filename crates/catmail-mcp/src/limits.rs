//! What one token may use: tool calls a minute, and downloads from the mail servers a
//! day. A busy agent shouldn't get the account locked: servers cap what an account
//! downloads a day (Gmail, `OverLimit`), and the app shares that cap. Kept in memory, by
//! token name: restarting the host starts every token afresh.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a download budget lasts, from the first download it pays for.
const BUDGET_PERIOD: Duration = Duration::from_secs(24 * 60 * 60);

/// What each token may use. The same for every token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenLimits {
    /// Tool calls a minute, which may come all at once.
    pub calls_per_minute: u32,
    /// Bytes downloaded from the mail servers for a token's calls (message bodies and
    /// attachments not downloaded yet), over 24 hours from the first.
    pub download_per_day: u64,
}

impl Default for TokenLimits {
    fn default() -> Self {
        Self {
            calls_per_minute: 60,
            // A tenth of what Gmail lets an account download over IMAP a day.
            download_per_day: 250 * 1024 * 1024,
        }
    }
}

/// What a token was refused, and when it can have it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverLimit {
    Calls { wait: Duration },
    Downloads { budget: u64, wait: Duration },
}

impl std::fmt::Display for OverLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            OverLimit::Calls { wait } => write!(
                f,
                "too many calls; try again in {}s",
                wait.as_secs_f64().ceil().max(1.0)
            ),
            OverLimit::Downloads { budget, wait } => write!(
                f,
                "this token may have {} downloaded from the mail servers a day, and \
                 that would take it over; more in {}h. Messages read already cost \
                 nothing",
                catmail_core::display::size(budget),
                (wait.as_secs_f64() / 3600.0).ceil().max(1.0)
            ),
        }
    }
}

pub(crate) struct Limits {
    limits: TokenLimits,
    used: Mutex<HashMap<String, Used>>,
}

/// What a token has used.
struct Used {
    /// Calls it may make at once now, refilled as time passes.
    calls: f64,
    calls_at: Instant,
    /// Since `budget_from`, if that was under [`BUDGET_PERIOD`] ago.
    downloaded: u64,
    budget_from: Option<Instant>,
}

impl Limits {
    pub fn new(limits: TokenLimits) -> Self {
        Self {
            limits,
            used: Mutex::default(),
        }
    }

    fn with<T>(&self, token: &str, now: Instant, f: impl FnOnce(&mut Used) -> T) -> T {
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        let used = used.entry(token.to_owned()).or_insert_with(|| Used {
            calls: f64::from(self.limits.calls_per_minute),
            calls_at: now,
            downloaded: 0,
            budget_from: None,
        });
        f(used)
    }

    /// Counts a call of `token`'s, unless it has made too many.
    pub fn call(&self, token: &str, now: Instant) -> Result<(), OverLimit> {
        let most = f64::from(self.limits.calls_per_minute);
        let per_second = most / 60.0;
        self.with(token, now, |used| {
            let elapsed = now.saturating_duration_since(used.calls_at).as_secs_f64();
            used.calls = (used.calls + elapsed * per_second).min(most);
            used.calls_at = now;
            if used.calls >= 1.0 {
                used.calls -= 1.0;
                Ok(())
            } else if per_second > 0.0 {
                Err(OverLimit::Calls {
                    wait: Duration::from_secs_f64((1.0 - used.calls) / per_second),
                })
            } else {
                Err(OverLimit::Calls {
                    wait: Duration::MAX,
                })
            }
        })
    }

    /// Takes `bytes` out of `token`'s download budget, unless they don't fit in what is
    /// left of it.
    pub fn download(&self, token: &str, bytes: u64, now: Instant) -> Result<(), OverLimit> {
        let budget = self.limits.download_per_day;
        self.with(token, now, |used| {
            let from = match used.budget_from {
                Some(from) if now.saturating_duration_since(from) < BUDGET_PERIOD => from,
                _ => {
                    used.downloaded = 0;
                    now
                }
            };
            if used.downloaded.saturating_add(bytes) > budget {
                return Err(OverLimit::Downloads {
                    budget,
                    wait: BUDGET_PERIOD.saturating_sub(now.saturating_duration_since(from)),
                });
            }
            used.budget_from = Some(from);
            used.downloaded += bytes;
            Ok(())
        })
    }

    /// Gives back what [`download`](Self::download) took for a download that failed.
    pub fn refund(&self, token: &str, bytes: u64, now: Instant) {
        self.with(token, now, |used| {
            used.downloaded = used.downloaded.saturating_sub(bytes);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(calls_per_minute: u32, download_per_day: u64) -> Limits {
        Limits::new(TokenLimits {
            calls_per_minute,
            download_per_day,
        })
    }

    #[test]
    fn calls_come_at_once_up_to_the_limit_then_as_it_refills() {
        let limits = limits(60, 0);
        let start = Instant::now();
        for _ in 0..60 {
            limits.call("a", start).unwrap();
        }
        let Err(OverLimit::Calls { wait }) = limits.call("a", start) else {
            panic!("a 61st call at once");
        };
        assert_eq!(wait, Duration::from_secs(1));
        // Another token has its own.
        limits.call("b", start).unwrap();

        let later = start + Duration::from_millis(1500);
        limits.call("a", later).unwrap();
        assert!(limits.call("a", later).is_err());
        // Never more than the limit at once, however long it waited.
        let much_later = start + Duration::from_secs(3600);
        for _ in 0..60 {
            limits.call("a", much_later).unwrap();
        }
        assert!(limits.call("a", much_later).is_err());
    }

    #[test]
    fn downloads_fit_in_a_budget_that_lasts_a_day_from_the_first() {
        let limits = limits(60, 100);
        let start = Instant::now();
        limits.download("a", 60, start).unwrap();
        let hour = Duration::from_secs(3600);
        let Err(OverLimit::Downloads { budget, wait }) = limits.download("a", 41, start + hour)
        else {
            panic!("over the budget");
        };
        assert_eq!((budget, wait), (100, 23 * hour));
        limits.download("a", 40, start + hour).unwrap();
        limits.download("b", 100, start + hour).unwrap();
        assert!(limits.download("a", 1, start + 23 * hour).is_err());

        // A day after the first, a new budget.
        limits.download("a", 100, start + 24 * hour).unwrap();
        assert!(limits.download("a", 1, start + 25 * hour).is_err());
        limits.refund("a", 30, start + 25 * hour);
        limits.download("a", 30, start + 25 * hour).unwrap();
    }

    #[test]
    fn what_the_agent_is_told() {
        let calls = OverLimit::Calls {
            wait: Duration::from_millis(200),
        };
        assert_eq!(calls.to_string(), "too many calls; try again in 1s");
        let downloads = OverLimit::Downloads {
            budget: 250 * 1024 * 1024,
            wait: Duration::from_secs(90 * 60),
        };
        assert!(downloads.to_string().contains("250.0 MB"), "{downloads}");
        assert!(downloads.to_string().contains("in 2h"), "{downloads}");
    }
}
