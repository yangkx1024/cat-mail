//! IMAP client on top of `imap-next`.

mod body;
mod connection;
mod error;
mod session;
mod types;
pub mod utf7;

pub use crate::Credentials;
pub use error::{Error, Result};
pub use session::{CopyUid, Session};
pub use types::{Capabilities, FetchedMessage, ListedMailbox, MailboxStatus};
