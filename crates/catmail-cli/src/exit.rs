//! Exit codes beyond success and failure, so that scripts can tell "try again later"
//! and "may have been delivered" apart from other failures.

use std::fmt;

/// How a command that didn't do everything ended. Ordered by how much it needs the
/// user: a command that ends several ways exits with the greatest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Outcome {
    /// Nothing was refused or lost: what couldn't be done is kept, and `catmail sync`,
    /// `ops flush` or `outbox flush` does it later. Exits with 3.
    Queued,
    /// Anything else, from a server refusal to a typo. Exits with 1, as do errors
    /// that aren't [`Exit`]s.
    Failed,
    /// A message may or may not have been delivered; only the user can tell. Exits
    /// with 4.
    Unknown,
}

impl Outcome {
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Failed => 1,
            Self::Queued => 3,
            Self::Unknown => 4,
        }
    }
}

/// An error that exits with its [`Outcome`]'s code.
#[derive(Debug)]
pub(crate) struct Exit {
    pub(crate) outcome: Outcome,
    message: String,
}

impl Exit {
    pub(crate) fn new(outcome: Outcome, message: impl Into<String>) -> Self {
        Self {
            outcome,
            message: message.into(),
        }
    }
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Exit {}

/// How a command that failed with `error` ended.
pub(crate) fn outcome(error: &anyhow::Error) -> Outcome {
    error
        .downcast_ref::<Exit>()
        .map_or(Outcome::Failed, |e| e.outcome)
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    #[test]
    fn code_survives_context() {
        let error: anyhow::Result<()> = Err(Exit::new(Outcome::Unknown, "lost").into());
        let error = error.context("sending").unwrap_err();
        assert_eq!(outcome(&error), Outcome::Unknown);
        assert_eq!(outcome(&anyhow::anyhow!("other")), Outcome::Failed);
        assert!(Outcome::Queued < Outcome::Failed && Outcome::Failed < Outcome::Unknown);
    }
}
