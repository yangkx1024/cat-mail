use serde::{Deserialize, Serialize};

use crate::{AccountId, FolderId};

/// What one sync of an account did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub account: AccountId,
    pub folders: Vec<FolderSyncReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSyncReport {
    pub folder: FolderId,
    pub name: String,
    /// Messages fetched for the first time.
    pub added: u32,
    /// Stored messages whose flags changed.
    pub updated: u32,
    /// Messages gone from the server.
    pub removed: u32,
    /// The server reported a new UIDVALIDITY, so the folder was refetched from scratch.
    pub uidvalidity_changed: bool,
    /// Set if this folder failed while the rest of the account synced.
    pub error: Option<String>,
}

impl FolderSyncReport {
    pub fn new(folder: FolderId, name: impl Into<String>) -> Self {
        Self {
            folder,
            name: name.into(),
            added: 0,
            updated: 0,
            removed: 0,
            uidvalidity_changed: false,
            error: None,
        }
    }
}

/// What a blob garbage collection removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    pub removed: u64,
    pub bytes: u64,
}
