//! SMTP submission client (RFC 5321, RFC 6409): just enough to hand one message after
//! another to the account's server.
//!
//! A transaction is split so the caller can tell the two halves of a failure apart:
//! up to and including [`Session::write_message`] the server has not seen the end of
//! the message and discards it, so sending again is safe; once
//! [`Session::end_message`] starts, the server may accept the message at any moment, and
//! a failure without a reply means the outcome is unknown.

mod error;
mod session;

pub use error::{Error, Result};
pub use session::{Session, is_envelope_address};
