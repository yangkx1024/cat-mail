use std::borrow::Cow;
use std::io::{self, BufRead, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use catmail_core::{
    Account, AccountId, AuthKind, Config, Flags, Folder, MailClient, MessageSummary, NewAccount,
    Security, ServerConfig, SyncReport,
};
use clap::{Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
    /// Fetch folders and message headers from the server
    Sync {
        /// Account id or email address [default: every account]
        account: Option<String>,
    },
    /// List folders, or the messages in a folder
    Ls(Ls),
}

#[derive(Args)]
struct Ls {
    /// Folder name (e.g. INBOX); without it, list the folders
    folder: Option<String>,
    /// Account id or email address [default: the only account]
    #[arg(short, long)]
    account: Option<String>,
    /// Maximum number of messages
    #[arg(short = 'n', long, default_value_t = 50)]
    limit: u32,
    /// Skip this many of the newest messages
    #[arg(long, default_value_t = 0)]
    offset: u32,
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
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        // Errors can carry server-supplied text, so they go through `printable` too.
        Err(e) => {
            eprintln!("Error: {}", printable(&format!("{e:#}")));
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
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
        Command::Sync { account } => sync(&client, account.as_deref(), cli.json).await,
        Command::Ls(args) => ls(&client, args, cli.json).await,
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

async fn sync(client: &MailClient, selector: Option<&str>, json: bool) -> Result<()> {
    let accounts = match selector {
        Some(selector) => vec![resolve_account(client, selector).await?],
        None => client.list_accounts().await?,
    };
    if accounts.is_empty() {
        bail!("no accounts; add one with `catmail account add`");
    }
    let mut reports = Vec::new();
    let mut failed = 0;
    for account in &accounts {
        if !json {
            println!("Syncing {}…", account.email);
        }
        match client.sync_account(account.id).await {
            Ok(report) => {
                if !json {
                    print_sync_report(&report);
                }
                reports.push(report);
            }
            Err(e) => {
                failed += 1;
                eprintln!("{}: {}", account.email, printable(&e.to_string()));
            }
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    }
    if failed > 0 {
        bail!("{failed} of {} accounts failed to sync", accounts.len());
    }
    Ok(())
}

fn print_sync_report(report: &SyncReport) {
    let mut quiet = true;
    for folder in &report.folders {
        let mut parts = Vec::new();
        if folder.uidvalidity_changed {
            parts.push("UIDVALIDITY changed, refetched".to_owned());
        }
        for (n, what) in [
            (folder.added, "new"),
            (folder.updated, "updated"),
            (folder.removed, "removed"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        if let Some(error) = &folder.error {
            parts.push(format!("failed: {error}"));
        }
        if !parts.is_empty() {
            quiet = false;
            println!(
                "  {}: {}",
                printable(&folder.name),
                printable(&parts.join(", "))
            );
        }
    }
    if quiet {
        println!("  up to date ({} folders)", report.folders.len());
    }
}

async fn ls(client: &MailClient, args: Ls, json: bool) -> Result<()> {
    let account = match &args.account {
        Some(selector) => resolve_account(client, selector).await?,
        None => {
            let mut accounts = client.list_accounts().await?;
            match accounts.len() {
                0 => bail!("no accounts; add one with `catmail account add`"),
                1 => accounts.remove(0),
                _ => bail!("there are several accounts; choose one with --account"),
            }
        }
    };
    let Some(name) = args.folder else {
        let folders = client.list_folders(account.id).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&folders)?);
        } else if folders.is_empty() {
            println!("No folders yet. Run `catmail sync` first.");
        } else {
            print_folders(&folders);
        }
        return Ok(());
    };
    let folder = client
        .find_folder(account.id, &name)
        .await?
        .with_context(|| format!("no folder `{name}` in {}", account.email))?;
    let messages = client
        .list_messages(folder.id, args.limit, args.offset)
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&messages)?);
    } else if messages.is_empty() {
        println!("No messages in {}.", printable(&folder.name));
    } else {
        print_messages(&messages);
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
    let rows = accounts
        .iter()
        .map(|a| {
            vec![
                a.id.to_string(),
                a.email.clone(),
                format!("{}:{} ({})", a.imap.host, a.imap.port, a.imap.security),
                format!("{}:{} ({})", a.smtp.host, a.smtp.port, a.smtp.security),
            ]
        })
        .collect();
    print_table(&["ID", "EMAIL", "IMAP", "SMTP"], rows);
}

fn print_folders(folders: &[Folder]) {
    let rows = folders
        .iter()
        .map(|f| {
            let [unread, total] = if f.selectable {
                [f.unread_count.to_string(), f.total_count.to_string()]
            } else {
                ["-".into(), "-".into()]
            };
            vec![
                f.name.clone(),
                f.role.map(|r| r.as_str().to_owned()).unwrap_or_default(),
                unread,
                total,
            ]
        })
        .collect();
    print_table(&["FOLDER", "ROLE", "UNREAD", "TOTAL"], rows);
}

fn print_messages(messages: &[MessageSummary]) {
    let tz = jiff::tz::TimeZone::system();
    let rows = messages
        .iter()
        .map(|m| {
            let mut marks = String::new();
            marks.push(if m.flags.contains(Flags::SEEN) {
                ' '
            } else {
                '*'
            });
            marks.push(if m.flags.contains(Flags::FLAGGED) {
                '!'
            } else {
                ' '
            });
            marks.push(if m.has_attachments { '@' } else { ' ' });
            let date = m
                .date
                .and_then(|t| jiff::Timestamp::from_second(t).ok())
                .map(|t| {
                    t.to_zoned(tz.clone())
                        .strftime("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            let from = m
                .from
                .first()
                .map(|a| a.name.clone().unwrap_or_else(|| a.email.clone()))
                .unwrap_or_default();
            vec![
                m.id.to_string(),
                marks,
                date,
                truncate(&from, 24),
                m.subject.clone().unwrap_or_else(|| "(no subject)".into()),
            ]
        })
        .collect();
    print_table(&["ID", "", "DATE", "FROM", "SUBJECT"], rows);
}

/// Shortens to at most `width` terminal columns, marking the cut with `…`.
fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Escapes characters that a terminal would act on rather than show: C0/C1 controls
/// (ESC sequences, line breaks) and bidi overrides that reorder the rest of the line.
/// Subjects, names and server messages come from strangers.
fn printable(s: &str) -> Cow<'_, str> {
    let is_unsafe = |c: char| {
        c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    };
    if !s.chars().any(is_unsafe) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        if is_unsafe(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Left-aligned columns, padded by display width so CJK text lines up. Cells are
/// escaped with [`printable`] before measuring.
fn print_table(header: &[&str], rows: Vec<Vec<String>>) {
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| match printable(&cell) {
                    Cow::Owned(escaped) => escaped,
                    Cow::Borrowed(_) => cell,
                })
                .collect()
        })
        .collect();
    let header: Vec<String> = header.iter().map(|h| h.to_string()).collect();
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            rows.iter()
                .chain([&header])
                .map(|r| r[i].width())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in [&header].into_iter().chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell}{}", " ".repeat(w - cell.width())))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_escapes_terminal_controls() {
        // =?UTF-8?B?G1sySnNwb29m?= decodes to ESC [2J followed by "spoof".
        assert_eq!(printable("\u{1b}[2Jspoof"), "\\u{1b}[2Jspoof");
        assert_eq!(printable("a\nb\tc\u{9b}"), "a\\u{a}b\\u{9}c\\u{9b}");
        assert_eq!(printable("evil\u{202e}fdp.exe"), "evil\\u{202e}fdp.exe");
        assert!(matches!(
            printable("你好，世界 ok"),
            Cow::Borrowed("你好，世界 ok")
        ));
    }

    #[test]
    fn truncate_counts_display_width() {
        assert_eq!(truncate("张三丰", 5), "张三…");
        assert_eq!(truncate("abc", 5), "abc");
    }
}
