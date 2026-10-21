//! Domain types shared by every cat-mail layer.
//!
//! This crate is pure data: no IO, no async, no storage details. Storage and protocol
//! crates map their own representations into these types, and frontends only ever see
//! these types (re-exported through `catmail-core`).

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
    Account, AccountOverview, AccountStatus, AuthKind, NewAccount, Security, ServerConfig,
    ServerProblem,
};
pub use draft::{Draft, DraftAttachment, DraftContent};
pub use error::{ErrorKind, ReportedError};
pub use event::Event;
pub use folder::{DeleteOutcome, Folder, FolderRole, archive_folder, special_folder};
pub use ids::{
    AccountId, AttachmentId, BlobHash, DraftAttachmentId, DraftId, FolderId, MessageId, OutboxId,
    PendingOpId,
};
pub use message::{Address, Attachment, FlagState, Flags, Message, MessageBody, MessageSummary};
pub use ops::{OpKind, OpStatus, PendingOp, ReplayReport};
pub use outbox::{OutboxEntry, OutboxStatus};
pub use search::SearchQuery;
pub use sync::{FolderSyncReport, GcReport, SyncReport};
