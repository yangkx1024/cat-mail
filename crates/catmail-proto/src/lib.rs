//! Remote protocol adapters for cat-mail.
//!
//! - [`imap`]: a read-only IMAP session (M1); writes and IDLE come in M4
//! - [`mime`]: header parsing, later full MIME parsing and building
//! - SMTP comes in M3

pub mod imap;
pub mod mime;
mod tls;

pub use tls::TlsConfig;
