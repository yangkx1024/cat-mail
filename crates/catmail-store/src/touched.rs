//! What a change did, for change events: beneath the repositories, since any of them
//! may change messages or queued changes, and the cascades between them add theirs up.

use std::collections::BTreeSet;

use catmail_types::{AccountId, FolderId, PendingOpId};

/// What a change did, for change events; for a change to messages, also the ops it
/// queued, for its caller to follow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Touched {
    /// Folders it changed messages in: where they are, where they went, and the labels
    /// it added to them or took off.
    pub folders: BTreeSet<(AccountId, FolderId)>,
    /// The other folders that list those messages (their labels, Starred): their lists
    /// change with them, though nothing was done in them.
    pub shown: BTreeSet<(AccountId, FolderId)>,
    /// Accounts some of whose queued changes it made fail or turned into conflicts.
    pub refused: BTreeSet<AccountId>,
    /// The ops a change to messages queued or redirected (a move still on its way):
    /// what became of them is in the queue, even once their message's row is gone.
    /// No event's: their folders are in `folders`.
    pub ops: BTreeSet<PendingOpId>,
}

impl Touched {
    pub fn new() -> Self {
        Self::default()
    }

    /// Nothing to announce.
    pub fn is_empty(&self) -> bool {
        self.folders.is_empty() && self.shown.is_empty() && self.refused.is_empty()
    }

    pub fn merge(&mut self, other: Touched) {
        self.folders.extend(other.folders);
        self.shown.extend(other.shown);
        self.refused.extend(other.refused);
        self.ops.extend(other.ops);
    }

    /// Every folder whose list of messages it changed, once.
    pub fn lists(&self) -> impl Iterator<Item = (AccountId, FolderId)> + '_ {
        self.folders.union(&self.shown).copied()
    }
}

#[cfg(test)]
impl Touched {
    /// The rest, for comparing with what a test expects of the events.
    pub(crate) fn without_ops(self) -> Self {
        Self {
            ops: BTreeSet::new(),
            ..self
        }
    }
}

impl FromIterator<(AccountId, FolderId)> for Touched {
    fn from_iter<I: IntoIterator<Item = (AccountId, FolderId)>>(folders: I) -> Self {
        Self {
            folders: folders.into_iter().collect(),
            ..Self::default()
        }
    }
}

impl<const N: usize> From<[(AccountId, FolderId); N]> for Touched {
    fn from(folders: [(AccountId, FolderId); N]) -> Self {
        folders.into_iter().collect()
    }
}
