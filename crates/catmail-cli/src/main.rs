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

use anyhow::Result;
use catmail_core::{Config, DEFAULT_GC_INTERVAL, DraftId, MailClient, SyncSchedule};
use clap::{Parser, Subcommand};

use crate::cmd::{account, compose, gc, identity, ops, outbox, read, search, sync, watch};
use crate::output::printable_text;

#[derive(Parser)]
#[command(
    name = "catmail",
    version = catmail_core::VERSION,
    about = "cat-mail from the terminal",
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
    /// List folders, or the messages in a folder
    Ls(search::Ls),
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
            "failed to open the local mail store (the cat-mail app keeps running after its \
             window is closed; quit it first)"
        }
        _ => "failed to open the local mail store",
    };
    anyhow::Error::new(e).context(context)
}

async fn run() -> Result<()> {
    catmail_core::logging::init("catmail=info");

    let cli = Cli::parse();
    let config = match cli.data_dir {
        Some(dir) => Config::with_data_dir(dir),
        None => Config::platform_default()?,
    };
    let watching = matches!(cli.command, Command::Watch);
    catmail_core::logging::write_file(&config, "cli");
    let client = MailClient::builder(config)
        .runtime(tokio::runtime::Handle::current())
        .sync_in_background(watching.then(SyncSchedule::default))
        // While this holds the data directory, `catmail gc` can't run.
        .collect_garbage_every(watching.then_some(DEFAULT_GC_INTERVAL))
        .open()
        .map_err(open_failed)?;

    match cli.command {
        Command::Account(cmd) => account::account(&client, cmd, cli.json).await,
        Command::Identity(cmd) => identity::identity(&client, cmd, cli.json).await,
        Command::Sync { account } => sync::sync(&client, account.as_deref(), cli.json).await,
        Command::Watch => {
            let interrupted = async {
                if let Err(e) = tokio::signal::ctrl_c().await {
                    tracing::warn!(error = %e, "can't wait for Ctrl-C");
                    std::future::pending::<()>().await;
                }
            };
            watch::watch(&client, cli.json, interrupted).await
        }
        Command::Ls(args) => search::ls(&client, args, cli.json).await,
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
