//! The catmail engine and the [`MailClient`] facade.
//!
//! Frontends (CLI, GUI) depend on this crate only. Every public async method is
//! executor-agnostic: work runs on a tokio runtime owned by (or handed to) the client,
//! so the futures can be awaited from a UI toolkit's main loop as well as from tokio.

#![forbid(unsafe_code)]

mod accounts;
mod body;
mod client;
pub mod display;
mod drafts;
mod engine;
mod error;
pub mod logging;
/// What renderers of [`MessageBody::html`] need to resolve and block its images.
pub mod html {
    pub use catmail_html::{
        DarkPalette, Image, Served, ServedImage, cid_image_ids, embed_inline_images, escape,
        quoted_text_html, remote_image_url, serve_images, without_sheet,
    };
    pub use catmail_proto::http::RemoteImage;
}
mod search;
mod send;
mod sync;
mod task;
#[cfg(feature = "test-support")]
pub mod test_support;

pub use accounts::Verify;
pub use body::filename::safe_filename;
pub use body::images::RemoteImageStream;
pub use catmail_proto::TlsConfig;
/// Files and directories for the user alone, as the data directory's are: for what
/// frontends keep elsewhere (MCP tokens in the config directory).
pub use catmail_store::{create_private_dir, private_file_options};
pub use catmail_types::*;
pub use client::{DraftEditor, MailClient, MailClientBuilder};
pub use drafts::addresses::{format_addresses, parse_addresses};
pub use drafts::{EditMark, named_as_image};
pub use engine::config::{APP_ID, Config, DataDirLock};
pub use engine::secrets::{MemorySecrets, SecretStore};
pub use engine::settings::{BodyPrefetch, EngineTuning, SyncSchedule};
pub use error::{Error, HttpError, ImapError, Result, SmtpError, StoreError};
pub use logging::version::VERSION;
pub use secrecy::{ExposeSecret, SecretString};
pub use send::{DraftRevision, SendPreview};
/// What [`MailClient::subscribe`]'s receiver returns when it falls behind or the engine
/// is gone.
pub use tokio::sync::broadcast::error::RecvError;
