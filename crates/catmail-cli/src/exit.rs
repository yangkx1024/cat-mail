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

/// 128 + SIGPIPE, as a shell reports a program that signal killed. Under `pipefail`,
/// `catmail search x | head` fails as `cat file | head` does.
const STDOUT_CLOSED: u8 = 141;

/// What a command that ended with `result` exits with; `stdout_closed` if whoever read
/// its output stopped before the end. How the command failed comes first: a reader
/// that went away must not hide that a message may have been delivered.
pub(crate) fn code(result: &anyhow::Result<()>, stdout_closed: bool) -> u8 {
    match result {
        Err(e) => outcome(e).code(),
        Ok(()) if stdout_closed => STDOUT_CLOSED,
        Ok(()) => 0,
    }
}

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

    #[test]
    fn a_failure_outranks_a_closed_stdout() {
        let unknown = Err(Exit::new(Outcome::Unknown, "lost").into());
        assert_eq!(code(&unknown, true), 4);
        assert_eq!(code(&Err(anyhow::anyhow!("other")), true), 1);
        assert_eq!(code(&Ok(()), true), 141);
        assert_eq!(code(&Ok(()), false), 0);
    }
}
