//! IMAP client: our own command encoding and connection driver, with responses parsed
//! by `imap-proto`.

mod body;
mod command;
mod connection;
mod error;
mod framing;
mod session;
mod types;
pub mod utf7;

pub use error::{Error, Result, says_over_limit};
pub use session::{CopyUid, Session};
pub use types::{Capabilities, ChangedFlags, FetchedMessage, Items, ListedMailbox, MailboxStatus};
