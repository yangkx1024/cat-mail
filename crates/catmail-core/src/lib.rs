//! The cat-mail engine and the [`MailClient`] facade.
//!
//! Frontends (CLI, GUI) depend on this crate only. Every public async method is
//! executor-agnostic: work runs on a tokio runtime owned by (or handed to) the client,
//! so the futures can be awaited from GPUI's executor as well as from tokio.

mod auth;
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
/// What renderers of [`MessageBody::html`] need to resolve and block its images, and to
/// lay it out without a browser.
pub mod html {
    pub use catmail_proto::mime::{
        InlineImage, content_id_from_url, embed_inline_images, image_sources, unwrap_layout_tables,
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
pub use client::{MailClient, MailClientBuilder};
pub use config::Config;
pub use drafts::{format_addresses, parse_addresses};
pub use engine::DEFAULT_DRAFT_UPLOAD_DELAY;
pub use error::{Error, ImapError, Result, SmtpError, StoreError};
pub use filename::safe_filename;
pub use gc::DEFAULT_GC_INTERVAL;
pub use secrecy::{ExposeSecret, SecretString};
pub use secrets::{KeyringSecrets, MemorySecrets, SecretStore};
/// What [`MailClient::subscribe`]'s receiver returns when it falls behind or the engine
/// is gone.
pub use tokio::sync::broadcast::error::RecvError;
pub use watch::SyncSchedule;
