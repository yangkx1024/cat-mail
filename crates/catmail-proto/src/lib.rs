//! Remote protocol adapters for cat-mail.
//!
//! - [`imap`]: a read-only IMAP session (M1, bodies in M2); writes and IDLE come in M4
//! - [`mime`]: header parsing and message content (text, sanitized HTML, attachments);
//!   building messages comes in M3
//! - SMTP comes in M3

pub mod imap;
pub mod mime;
mod tls;

pub use tls::TlsConfig;
