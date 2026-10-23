//! IMAP client on top of `imap-next`.

mod body;
mod connection;
mod error;
mod quirks;
mod session;
mod types;
pub mod utf7;

pub use error::{Error, Result};
pub use session::{CopyUid, Session};
pub use types::{Capabilities, ChangedFlags, FetchedMessage, ListedMailbox, MailboxStatus};
