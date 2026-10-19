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
mod engine;
mod error;
mod filename;
mod runtime;
mod secrets;
mod sync;

pub use catmail_proto::TlsConfig;
pub use catmail_proto::imap::Error as ImapError;
pub use catmail_types::*;
pub use client::{MailClient, MailClientBuilder};
pub use config::Config;
pub use error::{Error, Result};
pub use filename::safe_filename;
pub use secrets::{KeyringSecrets, MemorySecrets, SecretStore};
