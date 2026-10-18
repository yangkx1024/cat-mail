use std::collections::BTreeSet;
use std::fmt;

use catmail_types::{FlagState, FolderRole};

use crate::mime::Headers;

/// How to log in. Each variant maps to a SASL mechanism (or LOGIN), so supporting a new
/// kind of credential (OAuth2 in M6) adds a variant here and leaves sync code alone.
#[derive(Clone)]
#[non_exhaustive]
pub enum Credentials {
    Password { username: String, password: String },
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password { username, .. } => f
                .debug_struct("Password")
                .field("username", username)
                .finish_non_exhaustive(),
        }
    }
}

/// Server capabilities, upper-cased (`IMAP4REV1`, `AUTH=PLAIN`, `CONDSTORE`, …).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities(BTreeSet<String>);

impl Capabilities {
    pub fn has(&self, capability: &str) -> bool {
        self.0.contains(&capability.to_ascii_uppercase())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<S: AsRef<str>> FromIterator<S> for Capabilities {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        Self(
            iter.into_iter()
                .map(|c| c.as_ref().to_ascii_uppercase())
                .collect(),
        )
    }
}

/// One mailbox from LIST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedMailbox {
    /// The name exactly as the server sent it; use this in commands.
    pub raw_name: String,
    /// Decoded for display (modified UTF-7 → UTF-8). Equals `raw_name` if it was not
    /// valid modified UTF-7.
    pub name: String,
    pub delimiter: Option<char>,
    /// False for `\Noselect` / `\NonExistent` hierarchy nodes.
    pub selectable: bool,
    /// From a SPECIAL-USE attribute (RFC 6154), if the server sent one.
    pub special_use: Option<FolderRole>,
}

/// State reported by EXAMINE/SELECT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxStatus {
    pub uidvalidity: u32,
    pub uidnext: Option<u32>,
    pub exists: u32,
    /// `None` without CONDSTORE, or for a mailbox that reports NOMODSEQ.
    pub highest_modseq: Option<u64>,
}

/// A message as needed for list views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedMessage {
    pub state: FlagState,
    /// INTERNALDATE as unix seconds.
    pub internal_date: Option<i64>,
    pub size: Option<u32>,
    pub headers: Headers,
    pub has_attachments: bool,
}
