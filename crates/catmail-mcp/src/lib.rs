//! catmail over the Model Context Protocol: the tools AI agents call, the tokens they
//! present, and the Streamable HTTP server on a local address that runs them. Hosted by
//! whichever process holds the engine (`catmail serve`, the app): this crate only
//! needs a [`MailClient`](catmail_core::MailClient). See `docs/architecture/mcp.md`.

#![forbid(unsafe_code)]

pub mod connect;
mod http;
mod output;
#[cfg(test)]
mod tests;
mod tokens;
mod tools;

pub use http::{DEFAULT_PORT, McpServer, ServeOptions};
pub use tokens::{Access, NewToken, Tier, Token, TokenError, Tokens};

/// What a host adds to its log filter for the server's own lines (listening, stopped,
/// tokens turned away).
pub const LOG_FILTER: &str = "catmail_mcp=info";
