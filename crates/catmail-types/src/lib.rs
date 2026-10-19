//! Domain types shared by every cat-mail layer.
//!
//! This crate is pure data: no IO, no async, no storage details. Storage and protocol
//! crates map their own representations into these types, and frontends only ever see
//! these types (re-exported through `catmail-core`).

mod account;
mod event;
mod folder;
mod ids;
mod message;
mod search;
mod sync;

pub use account::{Account, AuthKind, NewAccount, Security, ServerConfig};
pub use event::Event;
pub use folder::{Folder, FolderRole};
pub use ids::{AccountId, AttachmentId, BlobHash, FolderId, MessageId};
pub use message::{Address, Attachment, FlagState, Flags, Message, MessageBody, MessageSummary};
pub use search::SearchQuery;
pub use sync::{FolderSyncReport, GcReport, SyncReport};
