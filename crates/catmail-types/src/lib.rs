//! Domain types shared by every catmail layer.
//!
//! This crate is pure data: no IO, no async, no storage details. Storage and protocol
//! crates map their own representations into these types, and frontends only ever see
//! these types (re-exported through `catmail-core`).

#![forbid(unsafe_code)]

mod account;
mod draft;
mod error;
mod event;
mod folder;
mod ids;
mod message;
mod ops;
mod outbox;
mod search;
mod sync;

pub use account::{
    Account, AccountMode, AccountOverview, AccountStatus, Identity, NewAccount, NewIdentity,
    Security, SendProblem, ServerConfig, ServerProblem,
};
pub use draft::{Draft, DraftAttachment, DraftContent, Forwarding};
pub use error::{ErrorKind, ReportedError};
pub use event::Event;
pub use folder::{
    DeleteMode, DeleteOutcome, Folder, FolderKind, FolderRole, MovePlan, Unmovable,
    can_change_folder, can_create_in, special_folder,
};
pub use ids::{
    AccountId, AttachmentId, BlobHash, DraftAttachmentId, DraftId, FolderId, IdentityId, MessageId,
    OutboxId, PendingOpId,
};
pub use message::{
    Address, Attachment, FlagState, Flags, GmailLabel, Message, MessageBody, MessageCursor,
    MessageSummary,
};
pub use ops::{ChangeReport, OpKind, OpStatus, PendingOp, ReplayReport};
pub use outbox::{FlushReport, OutboxEntry, OutboxStatus};
pub use search::{SearchField, SearchQuery, SearchTerm};
pub use sync::{FolderSyncReport, GcReport, SyncReport};
