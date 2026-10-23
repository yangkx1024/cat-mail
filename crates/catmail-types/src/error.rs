use serde::{Deserialize, Serialize};

/// What kind of failure an error is, for frontends to pick an icon and an action
/// without reading its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The server couldn't be reached, or the connection broke. Trying again later may
    /// work.
    Network,
    /// The server rejected the login: the password needs fixing.
    Auth,
    /// The server refused the request, or its answer couldn't be read. The text says
    /// why. Not necessarily for good: an SMTP server may refuse for now (see
    /// `catmail_core::Error::is_transient`).
    Refused,
    /// The local copy no longer matches the server; syncing brings it up to date.
    OutOfSync,
    /// What the request names doesn't exist (any more), or its account was removed.
    NotFound,
    /// The request, or the account's settings, can't work as given.
    Invalid,
    /// The request clashes with what is stored or running: a draft being sent, an
    /// account that exists already.
    Conflict,
    /// Something went wrong on this machine: the database, the disk, the keyring.
    Internal,
}

/// An error as background work reports it to frontends: its kind and its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedError {
    pub kind: ErrorKind,
    pub message: String,
    /// Whether trying the same again later may work, with nothing changed meanwhile
    /// (`catmail_core::Error::is_transient`). The kind can't tell: a `Refused` SMTP
    /// server may refuse for now or for good.
    pub transient: bool,
}

impl std::fmt::Display for ReportedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
