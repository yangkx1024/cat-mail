//! Remote protocol adapters for cat-mail.
//!
//! - [`imap`]: an IMAP session: read-only sync and bodies, plus APPEND (saving sent
//!   messages); flag changes and IDLE come in M4
//! - [`smtp`]: an SMTP submission client
//! - [`mime`]: header parsing, message content (text, sanitized HTML, attachments), and
//!   building outgoing messages

mod credentials;
pub mod imap;
pub mod mime;
mod net;
pub mod smtp;
mod tls;

pub use credentials::Credentials;
pub use tls::TlsConfig;

pub(crate) use net::Stream;
