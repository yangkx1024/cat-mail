//! Protocol test environment for cat-mail.
//!
//! [`FakeImap`] is a small in-process IMAP server with mutable mailboxes and fault
//! injection (dropped connections, duplicate or reordered responses, missing CONDSTORE,
//! UIDVALIDITY changes, plaintext injection after STARTTLS, refused or unanswered
//! APPENDs). It implements just enough of IMAP4rev1 for the sync engine, body downloads
//! (BODYSTRUCTURE and body sections are derived from each message's raw form) and
//! saving sent messages. [`FakeSmtp`] does the same for submission: it records what it
//! delivers and can refuse, stall or lose the confirmation of a message. [`FakeHttp`]
//! serves remote images, answering or holding each request as the test says. All run on
//! every CI platform. Real-server tests against Dovecot live in `testenv/`.

#![forbid(unsafe_code)]

mod http;
mod imap;
mod mime;
mod smtp;
mod tls;

pub use http::{FakeHttp, Reply, response};
pub use imap::{FakeImap, FakeMailbox, FakeMessage, Faults, LoggedCommand, ServerState};
pub use smtp::{FakeSmtp, ReceivedMessage, SmtpFaults, SmtpState};
pub use tls::TestCert;
