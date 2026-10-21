use std::collections::BTreeSet;

use catmail_types::{FlagState, FolderRole};

use crate::mime::Headers;

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

/// Messages of the selected mailbox whose flags the server reported changed while
/// idling (see [`Session::changed_flags`](crate::imap::Session::changed_flags)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangedFlags {
    /// These, by UID, ascending; none if nothing changed.
    Uids(Vec<u32>),
    /// Some, but it can't tell which.
    Unknown,
}

impl Default for ChangedFlags {
    fn default() -> Self {
        Self::Uids(Vec::new())
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
