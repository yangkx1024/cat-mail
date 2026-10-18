use std::io::{self, BufRead, IsTerminal};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use catmail_core::{
    Account, AccountId, AuthKind, Config, MailClient, NewAccount, Security, ServerConfig,
};
use clap::{Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "catmail", version, about = "cat-mail from the terminal")]
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
    Account(AccountCommand),
}

#[derive(Subcommand)]
enum AccountCommand {
    /// Add an IMAP/SMTP account (no server connection is made yet)
    Add(AddAccount),
    /// List accounts
    List,
    /// Remove an account and all local data for it
    Remove {
        /// Account id or email address
        account: String,
    },
}

#[derive(Args)]
struct AddAccount {
    #[arg(long)]
    email: String,
    /// Display name used in the From header
    #[arg(long)]
    name: Option<String>,
    /// Login name [default: the email address]
    #[arg(long)]
    username: Option<String>,

    #[arg(long)]
    imap_host: String,
    /// [default: 993 for tls, 143 otherwise]
    #[arg(long)]
    imap_port: Option<u16>,
    /// tls | starttls | plain
    #[arg(long, default_value = "tls")]
    imap_security: Security,

    #[arg(long)]
    smtp_host: String,
    /// [default: 465 for tls, 587 for starttls, 25 for plain]
    #[arg(long)]
    smtp_port: Option<u16>,
    /// tls | starttls | plain
    #[arg(long, default_value = "tls")]
    smtp_security: Security,

    /// Read the password from the first line of stdin instead of prompting
    #[arg(long)]
    password_stdin: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();
    let config = match cli.data_dir {
        Some(dir) => Config::with_data_dir(dir),
        None => Config::platform_default()?,
    };
    let client = MailClient::builder(config)
        .runtime(tokio::runtime::Handle::current())
        .open()
        .context("failed to open the local mail store")?;

    match cli.command {
        Command::Account(cmd) => account(&client, cmd, cli.json).await,
    }
}

async fn account(client: &MailClient, cmd: AccountCommand, json: bool) -> Result<()> {
    match cmd {
        AccountCommand::Add(args) => {
            let password = read_password(&args.email, args.password_stdin)?;
            let new = NewAccount {
                username: args.username.unwrap_or_else(|| args.email.clone()),
                email: args.email,
                display_name: args.name,
                imap: ServerConfig {
                    host: args.imap_host,
                    port: args
                        .imap_port
                        .unwrap_or_else(|| args.imap_security.default_imap_port()),
                    security: args.imap_security,
                },
                smtp: ServerConfig {
                    host: args.smtp_host,
                    port: args
                        .smtp_port
                        .unwrap_or_else(|| args.smtp_security.default_smtp_port()),
                    security: args.smtp_security,
                },
                auth: AuthKind::Password,
            };
            let account = client.add_account(new, password).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&account)?);
            } else {
                println!("Added account {} ({})", account.id, account.email);
            }
        }
        AccountCommand::List => {
            let accounts = client.list_accounts().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&accounts)?);
            } else {
                print_accounts(&accounts);
            }
        }
        AccountCommand::Remove { account } => {
            let account = resolve_account(client, &account).await?;
            client.remove_account(account.id).await?;
            if !json {
                println!("Removed account {} ({})", account.id, account.email);
            }
        }
    }
    Ok(())
}

/// Accepts either a numeric id or an email address.
async fn resolve_account(client: &MailClient, selector: &str) -> Result<Account> {
    let found = match selector.parse::<i64>() {
        Ok(id) => client.get_account(AccountId(id)).await?,
        Err(_) => client.find_account_by_email(selector).await?,
    };
    found.with_context(|| format!("no account matches `{selector}`"))
}

fn read_password(email: &str, from_stdin: bool) -> Result<String> {
    let password = if from_stdin {
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        line.trim_end_matches(['\r', '\n']).to_owned()
    } else {
        if !io::stdin().is_terminal() {
            bail!("stdin is not a terminal; use --password-stdin to pipe the password");
        }
        rpassword::prompt_password(format!("Password for {email}: "))?
    };
    if password.is_empty() {
        bail!("password must not be empty");
    }
    Ok(password)
}

fn print_accounts(accounts: &[Account]) {
    if accounts.is_empty() {
        println!("No accounts. Add one with `catmail account add`.");
        return;
    }
    let rows: Vec<[String; 4]> = accounts
        .iter()
        .map(|a| {
            [
                a.id.to_string(),
                a.email.clone(),
                format!("{}:{} ({})", a.imap.host, a.imap.port, a.imap.security),
                format!("{}:{} ({})", a.smtp.host, a.smtp.port, a.smtp.security),
            ]
        })
        .collect();
    let header = ["ID", "EMAIL", "IMAP", "SMTP"].map(String::from);
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            rows.iter()
                .chain([&header])
                .map(|r| r[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in [&header].into_iter().chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}
