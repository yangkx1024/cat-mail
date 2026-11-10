//! The catmail engine and the [`MailClient`] facade.
//!
//! Frontends (CLI, GUI) depend on this crate only. Every public async method is
//! executor-agnostic: work runs on a tokio runtime owned by (or handed to) the client,
//! so the futures can be awaited from a UI toolkit's main loop as well as from tokio.

#![forbid(unsafe_code)]

mod body;
mod client;
mod config;
mod connection;
pub mod display;
mod draft_sync;
mod drafts;
mod engine;
mod error;
mod filename;
mod folders;
mod gc;
mod image_cache;
mod images;
pub mod logging;
/// What renderers of [`MessageBody::html`] need to resolve and block its images.
pub mod html {
    pub use catmail_html::{
        DarkPalette, Image, Served, ServedImage, embed_inline_images, serve_images,
    };
    pub use catmail_proto::http::RemoteImage;
}
mod ops;
mod pool;
mod prefetch;
mod rebuild;
mod runtime;
mod search;
mod secrets;
mod send;
mod sleep;
mod sync;
mod system;
mod version;
mod watch;

pub use catmail_proto::TlsConfig;
/// Files and directories for the user alone, as the data directory's are: for what
/// frontends keep elsewhere (MCP tokens in the config directory).
pub use catmail_store::{create_private_dir, private_file_options};
pub use catmail_types::*;
pub use client::{DraftEditor, EVENT_CAPACITY, MailClient, MailClientBuilder, Verify};
pub use config::{Config, DataDirLock};
pub use drafts::{EditMark, format_addresses, parse_addresses};
pub use engine::{DEFAULT_DRAFT_UPLOAD_DELAY, EngineTuning};
pub use error::{Error, HttpError, ImapError, Result, SmtpError, StoreError};
pub use filename::safe_filename;
pub use gc::DEFAULT_GC_INTERVAL;
pub use images::RemoteImageStream;
pub use prefetch::BodyPrefetch;
pub use secrecy::{ExposeSecret, SecretString};
pub use secrets::{KeyringSecrets, MemorySecrets, SecretStore};
/// What [`MailClient::subscribe`]'s receiver returns when it falls behind or the engine
/// is gone.
pub use tokio::sync::broadcast::error::RecvError;
pub use version::VERSION;
pub use watch::SyncSchedule;
