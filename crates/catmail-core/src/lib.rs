//! The catmail engine and the [`MailClient`] facade.
//!
//! Frontends (CLI, GUI) depend on this crate only. Every public async method is
//! executor-agnostic: work runs on a tokio runtime owned by (or handed to) the client,
//! so the futures can be awaited from a UI toolkit's main loop as well as from tokio.

#![forbid(unsafe_code)]

mod accounts;
mod addresses;
mod answer;
mod body;
mod client;
mod config;
mod connection;
pub mod display;
mod downloading;
mod draft_sync;
mod drafts;
mod engine;
mod error;
mod events;
mod filename;
mod folders;
mod gc;
mod image_cache;
mod images;
pub mod logging;
/// What renderers of [`MessageBody::html`] need to resolve and block its images.
pub mod html {
    pub use catmail_html::{
        DarkPalette, Image, Served, ServedImage, cid_image_ids, embed_inline_images, escape,
        quoted_text_html, remote_image_url, serve_images, without_sheet,
    };
    pub use catmail_proto::http::RemoteImage;
}
mod ops;
mod pool;
mod prefetch;
mod rebuild;
mod runtime;
mod search;
mod search_slot;
mod secrets;
mod send;
mod settings;
mod sleep;
mod sync;
mod system;
mod task;
#[cfg(feature = "test-support")]
pub mod test_support;
mod version;
mod watch;

pub use accounts::Verify;
pub use addresses::{format_addresses, parse_addresses};
pub use catmail_proto::TlsConfig;
/// Files and directories for the user alone, as the data directory's are: for what
/// frontends keep elsewhere (MCP tokens in the config directory).
pub use catmail_store::{create_private_dir, private_file_options};
pub use catmail_types::*;
pub use client::{DraftEditor, MailClient, MailClientBuilder};
pub use config::{APP_ID, Config, DataDirLock};
pub use drafts::{EditMark, named_as_image};
pub use error::{Error, HttpError, ImapError, Result, SmtpError, StoreError};
pub use filename::safe_filename;
pub use images::RemoteImageStream;
pub use secrecy::{ExposeSecret, SecretString};
pub use secrets::{MemorySecrets, SecretStore};
pub use send::{DraftRevision, SendPreview};
pub use settings::{BodyPrefetch, EngineTuning, SyncSchedule};
/// What [`MailClient::subscribe`]'s receiver returns when it falls behind or the engine
/// is gone.
pub use tokio::sync::broadcast::error::RecvError;
pub use version::VERSION;
