#![forbid(unsafe_code)]

mod cmd;
mod exit;
mod files;
mod json;
mod output;
#[cfg(test)]
mod test_support;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use catmail_core::{Config, DraftId, MailClient};
use catmail_mcp::Tokens;
use clap::{Parser, Subcommand};

use crate::cmd::{
    account, compose, folder, gc, identity, mcp, ops, outbox, read, search, sync, watch,
};
use crate::output::printable_text;

#[derive(Parser)]
#[command(
    name = "catmail",
    version = catmail_core::VERSION,
    about = "catmail from the terminal",
    after_help = concat!(
        "Exit status:\n",
        "  0  done\n",
        "  1  failed\n",
        "  2  wrong usage\n",
        "  3  not done yet, but kept: `catmail sync`, `ops flush` or `outbox flush` does it\n",
        "  4  a message may or may not have been delivered; see `catmail outbox`",
    )
)]
struct Cli {
    /// Directory for the database and blobs [default: platform data dir]
    #[arg(long, global = true, env = "CATMAIL_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Directory for settings and MCP tokens, the app's too [default: platform config dir]
    #[arg(long, global = true, env = "CATMAIL_CONFIG_DIR")]
    config_dir: Option<PathBuf>,

    /// Print machine-readable JSON instead of tables
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage accounts
    #[command(subcommand)]
    Account(account::AccountCommand),
    /// Manage the other addresses an account sends as
    #[command(subcommand)]
    Identity(identity::IdentityCommand),
    /// Fetch folders and message headers from the server
    Sync {
        /// Account id or email address [default: every account]
        account: Option<String>,
    },
    /// Keep every account in sync until interrupted, printing what changes: new mail
    /// arrives as the server announces it (IDLE), other folders are synced every 15
    /// minutes, and changes to messages reach the server at once
    Watch,
    /// Keep every account in sync as `watch` does (sending too), and let AI agents use
    /// the mail over MCP, until interrupted
    Serve(mcp::Serve),
    /// The tokens AI agents use `catmail serve` with
    #[command(subcommand)]
    Mcp(mcp::McpCommand),
    /// List folders, or the messages in a folder
    Ls(search::Ls),
    /// Create, rename or delete folders (on Gmail, labels), on the server at once
    #[command(subcommand)]
    Folder(folder::FolderCommand),
    /// Show a message, downloading its body first (it stays unread on the server)
    Read(read::Read),
    /// Save an attachment, downloading it first if needed
    Attachment(read::SaveAttachment),
    /// Search subjects, addresses and downloaded bodies
    Search(search::Search),
    /// Mark messages read or unread, flagged or not
    Mark(ops::Mark),
    /// Move messages to another folder
    #[command(name = "mv")]
    Move(ops::Move),
    /// Archive messages: move them to their account's Archive folder, or on Gmail take
    /// them out of INBOX
    Archive(ops::Archive),
    /// Move messages to Trash, or delete them for good
    #[command(name = "rm")]
    Remove(ops::Remove),
    /// Add Gmail labels to messages, or take them off
    Label(ops::Label),
    /// Changes to messages not made on the server yet [default: list]
    Ops {
        #[command(subcommand)]
        command: Option<ops::OpsCommand>,
    },
    /// Write a new message, kept as a draft unless sent with --send
    Compose(compose::Compose),
    /// Reply to a message, quoting it; kept as a draft unless sent with --send
    Reply(compose::Reply),
    /// Forward a message with its attachments; kept as a draft unless sent with --send
    Forward(compose::Forward),
    /// List, show, edit or delete drafts
    #[command(subcommand)]
    Draft(compose::DraftCommand),
    /// Send a draft now, and save it to the Sent folder
    Send {
        /// Draft id, as shown by `catmail draft list`
        draft: i64,
    },
    /// Messages handed to "send" that aren't sent and saved yet [default: list]
    Outbox {
        #[command(subcommand)]
        command: Option<outbox::OutboxCommand>,
    },
    /// Delete stored bodies and attachments that nothing refers to any more
    Gc,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        // Errors can carry server-supplied text, so they are escaped too; line breaks
        // stay, for the hints some errors end with.
        Err(e) => {
            eprintln!("Error: {}", printable_text(&format!("{e:#}")));
            ExitCode::from(exit::outcome(&e).code())
        }
    }
}

/// The app holding the data directory may have no window to show it is there.
fn open_failed(e: catmail_core::Error) -> anyhow::Error {
    let context = match e {
        catmail_core::Error::AlreadyRunning(_) => {
            "failed to open the local mail store (the catmail app keeps running after its \
             window is closed; quit it first)"
        }
        _ => "failed to open the local mail store",
    };
    anyhow::Error::new(e).context(context)
}

async fn run() -> Result<()> {
    catmail_core::logging::init(&format!("catmail=info,{}", catmail_mcp::LOG_FILTER));

    let cli = Cli::parse();
    let tokens = || -> Result<Tokens> {
        let dir = match &cli.config_dir {
            Some(dir) => dir.clone(),
            None => {
                Config::platform_config_dir().context("no config directory; use --config-dir")?
            }
        };
        Ok(Tokens::new(dir))
    };
    // Tokens are a file of their own: managing them works while `serve` runs.
    if let Command::Mcp(command) = cli.command {
        return mcp::mcp(&tokens()?, command, cli.json);
    }
    let config = match cli.data_dir {
        Some(dir) => Config::with_data_dir(dir),
        None => Config::platform_default()?,
    };
    catmail_core::logging::write_file(&config, "cli");
    let builder = MailClient::builder(config).runtime(tokio::runtime::Handle::current());
    // While a long-running one holds the data directory, `catmail gc` can't run: it
    // collects garbage itself.
    let builder = match &cli.command {
        Command::Serve(_) => builder.resident(),
        // Sends nothing behind the user's back: `send` and `outbox flush` do.
        Command::Watch => builder.resident().send_in_background(false),
        _ => builder,
    };
    let client = builder.open().map_err(open_failed)?;

    match cli.command {
        Command::Account(cmd) => account::account(&client, cmd, cli.json).await,
        Command::Identity(cmd) => identity::identity(&client, cmd, cli.json).await,
        Command::Sync { account } => sync::sync(&client, account.as_deref(), cli.json).await,
        Command::Watch => watch::watch(&client, cli.json, interrupted()).await,
        Command::Serve(args) => mcp::serve(&client, tokens()?, args, interrupted()).await,
        Command::Mcp(_) => unreachable!("handled before opening the engine"),
        Command::Ls(args) => search::ls(&client, args, cli.json).await,
        Command::Folder(cmd) => folder::folder(&client, cmd, cli.json).await,
        Command::Read(args) => read::read(&client, args, cli.json).await,
        Command::Attachment(args) => read::save_attachment(&client, args).await,
        Command::Search(args) => search::search(&client, args, cli.json).await,
        Command::Mark(args) => ops::mark(&client, args, cli.json).await,
        Command::Move(args) => ops::move_messages(&client, args, cli.json).await,
        Command::Archive(args) => ops::archive(&client, args, cli.json).await,
        Command::Remove(args) => ops::remove(&client, args, cli.json).await,
        Command::Label(args) => ops::label(&client, args, cli.json).await,
        Command::Ops { command } => {
            let command = command.unwrap_or(ops::OpsCommand::List);
            ops::ops(&client, command, cli.json).await
        }
        Command::Compose(args) => compose::compose(&client, args, cli.json).await,
        Command::Reply(args) => compose::reply(&client, args, cli.json).await,
        Command::Forward(args) => compose::forward(&client, args, cli.json).await,
        Command::Draft(cmd) => compose::draft(&client, cmd, cli.json).await,
        Command::Send { draft } => outbox::send(&client, DraftId(draft), cli.json).await,
        Command::Outbox { command } => {
            let command = command.unwrap_or(outbox::OutboxCommand::List);
            outbox::outbox(&client, command, cli.json).await
        }
        Command::Gc => gc::gc(&client, cli.json).await,
    }
}

/// Ctrl-C, or (on Unix) SIGTERM, as a service manager stops a service.
async fn interrupted() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "can't wait for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                terminate.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "can't wait for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("interrupted; stopping");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_store_names_the_app_that_may_have_no_window() {
        let e = open_failed(catmail_core::Error::AlreadyRunning("/data".into()));
        assert!(format!("{e:#}").contains("keeps running after its window is closed"));
        // Still the engine's error underneath, for `--json` and the exit code.
        assert!(e.downcast_ref::<catmail_core::Error>().is_some());
        let e = open_failed(catmail_core::Error::NoDataDir);
        assert!(!format!("{e:#}").contains("window"));
    }
}
