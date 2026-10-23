//! The cat-mail engine and the [`MailClient`] facade.
//!
//! Frontends (CLI, GUI) depend on this crate only. Every public async method is
//! executor-agnostic: work runs on a tokio runtime owned by (or handed to) the client,
//! so the futures can be awaited from GPUI's executor as well as from tokio.

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
mod gc;
mod image_cache;
mod images;
/// What renderers of [`MessageBody::html`] need to resolve and block its images.
pub mod html {
    pub use catmail_proto::http::RemoteImage;
    pub use catmail_proto::mime::{
        ColorUsage, InlineImage, color_usage, content_id_from_url, embed_inline_images,
        image_sources, remote_image_url, remote_image_urls,
    };
}
mod ops;
mod runtime;
mod secrets;
mod send;
mod sync;
mod watch;

pub use catmail_proto::TlsConfig;
pub use catmail_types::*;
pub use client::{EVENT_CAPACITY, MailClient, MailClientBuilder};
pub use config::Config;
pub use drafts::{format_addresses, parse_addresses};
pub use engine::{DEFAULT_DRAFT_UPLOAD_DELAY, EngineTuning};
pub use error::{Error, HttpError, ImapError, Result, SmtpError, StoreError};
pub use filename::safe_filename;
pub use gc::DEFAULT_GC_INTERVAL;
pub use images::RemoteImageStream;
pub use secrecy::{ExposeSecret, SecretString};
pub use secrets::{KeyringSecrets, MemorySecrets, SecretStore};
/// What [`MailClient::subscribe`]'s receiver returns when it falls behind or the engine
/// is gone.
pub use tokio::sync::broadcast::error::RecvError;
pub use watch::SyncSchedule;
