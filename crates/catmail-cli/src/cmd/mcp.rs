//! `catmail serve`: the engine kept running, as the app keeps it, with the MCP server
//! on a local address for AI agents; and `catmail mcp token`, the tokens they present,
//! which don't need the engine (see `docs/architecture/mcp.md`).

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};

use anyhow::{Context, Result, bail};
use catmail_core::{MailClient, display};
use catmail_mcp::{DEFAULT_PORT, McpServer, ServeOptions, Tokens, connect};
use clap::{Args, Subcommand};

use crate::json::{self, JsonToken};
use crate::output::{print_table, printable};

#[derive(Args)]
pub(crate) struct Serve {
    /// Port on 127.0.0.1 to listen on; other devices reach it through `tailscale serve`
    /// or a tunnel
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
}

#[derive(Subcommand)]
pub(crate) enum McpCommand {
    /// The tokens agents connect with
    #[command(subcommand)]
    Token(TokenCommand),
}

#[derive(Subcommand)]
pub(crate) enum TokenCommand {
    /// Make a token for an agent; it is shown this once
    Add {
        /// What it is for, e.g. the device the agent runs on
        name: String,
    },
    /// List the tokens
    List,
    /// Take a token away; agents using it are turned away at once
    Rm { name: String },
}

/// Serves until `stop` resolves. The client must be [`resident`](
/// catmail_core::MailClientBuilder::resident).
pub(crate) async fn serve(
    client: &MailClient,
    tokens: Tokens,
    args: Serve,
    stop: impl Future<Output = ()>,
) -> Result<()> {
    if tokens.list()?.is_empty() {
        eprintln!(
            "No MCP tokens yet, so every agent is turned away; add one with \
             `catmail mcp token add <name>` (no restart needed)"
        );
    }
    let options = ServeOptions {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, args.port)),
        ..ServeOptions::default()
    };
    let server = McpServer::start(client.clone(), tokens, options)
        .await
        .with_context(|| format!("failed to listen on 127.0.0.1:{}", args.port))?;
    eprintln!(
        "Serving MCP on {}; press Ctrl-C to stop",
        connect::local_url(server.local_addr().port())
    );
    stop.await;
    server.stop().await;
    Ok(())
}

pub(crate) fn mcp(tokens: &Tokens, command: McpCommand, json: bool) -> Result<()> {
    let McpCommand::Token(command) = command;
    match command {
        TokenCommand::Add { name } => {
            let new = tokens.add(&name)?;
            if json {
                json::print_keyed("token", &JsonToken::new(&new.token, Some(new.secret)))?;
            } else {
                eprintln!(
                    "Token `{}` made; the agent sends it as `Authorization: Bearer <token>`. \
                     It isn't shown again:",
                    printable(&new.token.name)
                );
                println!("{}", new.secret);
                eprintln!("\n{}", connecting(&new.secret));
            }
        }
        TokenCommand::List => {
            let list = tokens.list()?;
            if json {
                let list: Vec<_> = list.iter().map(|t| JsonToken::new(t, None)).collect();
                json::print_keyed("tokens", &list)?;
            } else if list.is_empty() {
                eprintln!("No tokens; add one with `catmail mcp token add <name>`");
            } else {
                let rows = list
                    .iter()
                    .map(|t| {
                        vec![
                            printable(&t.name).into_owned(),
                            display::date(Some(t.created_at)),
                        ]
                    })
                    .collect();
                print_table(&["NAME", "CREATED"], rows);
            }
        }
        TokenCommand::Rm { name } => {
            if !tokens.remove(&name)? {
                bail!("no token named `{name}`");
            }
            if !json {
                eprintln!("Token `{}` taken away", printable(&name));
            }
        }
    }
    Ok(())
}

/// After a token is made: adding the server to the agents, with it.
fn connecting(secret: &str) -> String {
    let url = connect::local_url(DEFAULT_PORT);
    let mut text = String::from(
        "To add catmail to an agent on this machine (`catmail serve` on its default port):\n",
    );
    for example in connect::examples(&url, secret) {
        text.push_str(&format!("\n  {}:\n", example.agent));
        for line in example.commands.lines() {
            text.push_str(&format!("    {line}\n"));
        }
        if let Some(note) = example.note {
            text.push_str(&format!("    {note}\n"));
        }
    }
    text.push('\n');
    text.push_str(connect::REMOTE_NOTE);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_added_listed_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::new(dir.path());
        let add = |name: &str| TokenCommand::Add { name: name.into() };
        mcp(&tokens, McpCommand::Token(add("laptop")), true).unwrap();
        assert!(mcp(&tokens, McpCommand::Token(add("laptop")), true).is_err());
        mcp(&tokens, McpCommand::Token(TokenCommand::List), true).unwrap();
        let rm = || TokenCommand::Rm {
            name: "laptop".into(),
        };
        mcp(&tokens, McpCommand::Token(rm()), true).unwrap();
        let e = mcp(&tokens, McpCommand::Token(rm()), true).unwrap_err();
        assert!(e.to_string().contains("no token named"));
        assert!(tokens.list().unwrap().is_empty());
    }

    #[test]
    fn a_new_token_comes_with_the_commands_that_use_it() {
        let text = connecting("catmail_00");
        assert!(text.contains(
            "    claude mcp add --transport http catmail http://127.0.0.1:8642/mcp \\\n      \
             --header \"Authorization: Bearer catmail_00\"\n"
        ));
        assert!(text.contains("    export CATMAIL_MCP_TOKEN=catmail_00\n"));
    }
}
