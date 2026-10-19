//! Protocol test environment for cat-mail.
//!
//! [`FakeImap`] is a small in-process IMAP server with mutable mailboxes and fault
//! injection (dropped connections, duplicate or reordered responses, missing CONDSTORE,
//! UIDVALIDITY changes, plaintext injection after STARTTLS). It implements just enough of
//! IMAP4rev1 for the read-only sync engine and body downloads (BODYSTRUCTURE and body
//! sections are derived from each message's raw form), and runs on every CI platform. Real-server
//! tests against Dovecot live in `testenv/`.

mod imap;
mod mime;
mod tls;

pub use imap::{FakeImap, FakeMailbox, FakeMessage, Faults, LoggedCommand, ServerState};
pub use tls::TestCert;
