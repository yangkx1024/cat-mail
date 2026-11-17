//! Remote protocol adapters for catmail.
//!
//! - [`imap`]: an IMAP session: read-only sync and bodies, the changes the operation
//!   queue replays (flags, moves, deletions, labels, APPEND) and IDLE
//! - [`smtp`]: an SMTP submission client
//! - [`mime`]: header parsing, message content (text, sanitized HTML, attachments), and
//!   building outgoing messages
//! - [`http`]: a message's remote images, fetched when the user asks to see them

#![forbid(unsafe_code)]

mod credentials;
pub mod http;
pub mod imap;
pub mod mime;
mod net;
pub mod smtp;
mod tls;

pub use credentials::Credentials;
pub use tls::TlsConfig;

pub(crate) use net::Stream;
