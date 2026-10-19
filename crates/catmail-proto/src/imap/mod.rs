//! IMAP client on top of `imap-next`: read-only, except for APPEND.

mod body;
mod connection;
mod error;
mod session;
mod types;
pub mod utf7;

pub use crate::Credentials;
pub use error::{Error, Result};
pub use session::Session;
pub use types::{Capabilities, FetchedMessage, ListedMailbox, MailboxStatus};
