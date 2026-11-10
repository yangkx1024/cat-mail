//! How an agent connects: the commands that add the server to Claude Code and to Codex,
//! for the app's Settings › Agents and `catmail mcp token add` to show.

use std::net::Ipv4Addr;

use crate::http::PATH;

/// The variable Codex reads the token from: it takes a bearer token from the
/// environment only, not on its command line or in its config.
pub const CODEX_TOKEN_VAR: &str = "CATMAIL_MCP_TOKEN";

/// What an agent's examples put in place of a token it doesn't know.
pub const TOKEN_PLACEHOLDER: &str = "<token>";

/// The server's name in the agents' configuration.
const NAME: &str = "catmail";

/// What agents on other devices connect to instead of [`local_url`].
pub const REMOTE_NOTE: &str =
    "On another device, use the HTTPS address tailscale serve or the tunnel gives instead.";

/// The address agents on this machine connect to, with the server on `port`.
pub fn local_url(port: u16) -> String {
    format!("http://{}:{port}{PATH}", Ipv4Addr::LOCALHOST)
}

/// Adding the server to one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Example {
    pub agent: &'static str,
    /// Shell commands (a POSIX shell's), a long one continued over lines with `\`, so
    /// that it wraps between its options.
    pub commands: String,
    /// What else it takes, if anything.
    pub note: Option<&'static str>,
}

/// Adding the server at `url` to Claude Code and to Codex, with `token`.
pub fn examples(url: &str, token: &str) -> [Example; 2] {
    [
        Example {
            agent: "Claude Code",
            commands: format!(
                "claude mcp add --transport http {NAME} {url} \\\n  \
                 --header \"Authorization: Bearer {token}\""
            ),
            note: None,
        },
        Example {
            agent: "Codex",
            commands: format!(
                "codex mcp add {NAME} --url {url} \\\n  \
                 --bearer-token-env-var {CODEX_TOKEN_VAR}\n\
                 export {CODEX_TOKEN_VAR}={token}"
            ),
            note: Some(
                "Codex reads the token from CATMAIL_MCP_TOKEN: set it wherever Codex runs, \
                 such as in your shell's profile.",
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_carry_the_address_and_token() {
        let url = local_url(8642);
        assert_eq!(url, "http://127.0.0.1:8642/mcp");
        let [claude, codex] = examples(&url, "catmail_00");
        assert_eq!(
            claude.commands,
            r#"claude mcp add --transport http catmail http://127.0.0.1:8642/mcp \
  --header "Authorization: Bearer catmail_00""#
        );
        assert_eq!(
            codex.commands,
            r#"codex mcp add catmail --url http://127.0.0.1:8642/mcp \
  --bearer-token-env-var CATMAIL_MCP_TOKEN
export CATMAIL_MCP_TOKEN=catmail_00"#
        );
        assert!(codex.note.unwrap().contains(CODEX_TOKEN_VAR));
    }
}
