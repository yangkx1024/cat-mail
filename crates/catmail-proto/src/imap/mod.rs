//! Read-only IMAP client on top of `imap-next`.

mod connection;
mod error;
mod session;
mod types;
pub mod utf7;

pub use error::{Error, Result};
pub use session::Session;
pub use types::{Capabilities, Credentials, FetchedMessage, ListedMailbox, MailboxStatus};
