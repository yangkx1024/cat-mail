//! catmail over the Model Context Protocol: the tools AI agents call, the tokens they
//! present, and the Streamable HTTP server on a local address that runs them. Hosted by
//! whichever process holds the engine (`catmail serve`, the app): this crate only
//! needs a [`MailClient`](catmail_core::MailClient). See `docs/architecture/mcp.md`.

#![forbid(unsafe_code)]

pub mod connect;
mod http;
mod limits;
mod output;
#[cfg(test)]
mod tests;
mod tokens;
mod tools;
mod uploads;

pub use http::{DEFAULT_PORT, McpServer, ServeOptions};
pub use limits::TokenLimits;
pub use tokens::{Access, NewToken, Tier, Token, TokenError, Tokens};

/// A handle a client is given and gives back (`requestState`, an upload's URL): 16 random
/// bytes in hex, so that knowing one is as good as being given it.
pub(crate) fn random_handle() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("random bytes");
    hex::encode(bytes)
}

/// What a host adds to its log filter for the server's own lines (listening, stopped,
/// tokens turned away).
pub const LOG_FILTER: &str = "catmail_mcp=info";
