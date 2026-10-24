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

    /// The largest message the server takes in APPEND, if it names one for all its
    /// mailboxes (`APPENDLIMIT=<n>`, RFC 7889). A bare `APPENDLIMIT` names one per mailbox,
    /// in STATUS, which this leaves out. 0 is no APPEND at all (RFC 7889 §5), unlike
    /// SMTP's `SIZE 0`.
    pub fn append_limit(&self) -> Option<u64> {
        self.0
            .iter()
            .find_map(|c| c.strip_prefix("APPENDLIMIT="))
            .and_then(|limit| limit.parse().ok())
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

impl MailboxStatus {
    /// Whether UIDs recorded under `expected` still name the same messages here. Never
    /// for a mailbox whose UIDVALIDITY wasn't known. Each caller decides what a new
    /// generation means; one that only takes UIDs from the mailbox as it is now has
    /// nothing to compare.
    pub fn same_generation(&self, expected: Option<u32>) -> bool {
        expected == Some(self.uidvalidity)
    }
}

/// What a FETCH of messages asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Items {
    Standard,
    /// Also Gmail's X-GM-MSGID, X-GM-THRID and X-GM-LABELS (X-GM-EXT-1). Only in a
    /// location (All Mail, Trash, Spam): in a label's mailbox, X-GM-LABELS leaves out
    /// that label.
    Gmail,
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
    /// X-GM-MSGID and X-GM-THRID, with [`Items::Gmail`].
    pub gm_msgid: Option<u64>,
    pub gm_thrid: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_limit_is_the_one_for_every_mailbox() {
        let caps = |list: &[&str]| list.iter().collect::<Capabilities>();
        assert_eq!(
            caps(&["IMAP4rev1", "appendlimit=35882577"]).append_limit(),
            Some(35_882_577)
        );
        assert_eq!(caps(&["APPENDLIMIT"]).append_limit(), None, "per mailbox");
        assert_eq!(
            caps(&["APPENDLIMIT=0"]).append_limit(),
            Some(0),
            "no APPEND"
        );
        assert_eq!(caps(&["IMAP4rev1"]).append_limit(), None);
    }
}
