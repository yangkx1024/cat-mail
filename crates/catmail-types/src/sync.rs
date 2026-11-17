use serde::{Deserialize, Serialize};

use crate::{AccountId, FolderId, ReportedError};

/// What one sync of an account did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub account: AccountId,
    pub folders: Vec<FolderSyncReport>,
    /// Drafts the server refused to store in its Drafts folder, uploaded before the
    /// folders synced; [`Draft::upload_error`](crate::Draft::upload_error) says why.
    pub drafts_failed: u32,
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
    /// Queued local changes made on the server first.
    pub ops_applied: u32,
    /// Queued local changes the server refused; they were undone locally.
    pub ops_failed: u32,
    /// The server reported a new UIDVALIDITY, so the folder was refetched from scratch.
    pub uidvalidity_changed: bool,
    /// Set if this folder failed while the rest of the account synced.
    pub error: Option<ReportedError>,
}

impl SyncReport {
    /// Queued local changes the server refused, in every folder.
    pub fn changes_refused(&self) -> u32 {
        self.folders.iter().map(|f| f.ops_failed).sum()
    }
}

impl FolderSyncReport {
    pub fn new(folder: FolderId, name: impl Into<String>) -> Self {
        Self {
            folder,
            name: name.into(),
            added: 0,
            updated: 0,
            removed: 0,
            ops_applied: 0,
            ops_failed: 0,
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
