//! Remote protocol adapters for cat-mail.
//!
//! Planned contents (see `docs/architecture.md`):
//! - `imap`: a `RemoteStore` implementation on top of `imap-next` (M1)
//! - `smtp`: a `Transport` implementation (M3)
//! - MIME parsing and building helpers
//!
//! Intentionally empty in M0; the `imap-next` dependency is declared so the chosen
//! feature set is compiled and locked from the start.
