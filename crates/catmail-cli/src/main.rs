mod compose;
mod files;
mod ops;
mod output;

use std::collections::{BTreeSet, HashMap};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use catmail_core::{
    Account, AccountId, Address, AttachmentId, AuthKind, Config, DraftId, Flags, Folder, FolderId,
    MailClient, Message, MessageId, MessageSummary, NewAccount, SearchQuery, Security,
    ServerConfig, SyncReport, safe_filename,
};
use clap::{Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::output::{print_table, printable, printable_text, truncate};

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
    /// Show a message, downloading its body first (it stays unread on the server)
    Read(Read),
    /// Save an attachment, downloading it first if needed
    Attachment(SaveAttachment),
    /// Search subjects, addresses and downloaded bodies
    Search(Search),
    /// Mark messages read or unread, flagged or not
    Mark(ops::Mark),
    /// Move messages to another folder
    #[command(name = "mv")]
    Move(ops::Move),
    /// Move messages to Trash, or delete them for good
    #[command(name = "rm")]
    Remove(ops::Remove),
    /// Changes to messages not made on the server yet [default: list]
    Ops {
        #[command(subcommand)]
        command: Option<ops::OpsCommand>,
    },
    /// Write a new message, kept as a draft unless sent with --send
    Compose(compose::Compose),
    /// Reply to a message, quoting it; kept as a draft unless sent with --send
    Reply(compose::Reply),
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
        command: Option<compose::OutboxCommand>,
    },
    /// Delete stored bodies and attachments that nothing refers to any more
    Gc,
}

#[derive(Args)]
struct Read {
    /// Message id, as shown by `catmail ls`
    id: i64,
    /// Print the sanitized HTML part instead of the text
    #[arg(long, conflicts_with = "raw")]
    html: bool,
    /// Print the raw message source (the .eml)
    #[arg(long)]
    raw: bool,
    /// Save the attachments into this directory
    #[arg(long, value_name = "DIR")]
    save: Option<PathBuf>,
    /// Don't contact the server; show only what is stored
    #[arg(long, conflicts_with_all = ["raw", "save"])]
    offline: bool,
}

#[derive(Args)]
struct SaveAttachment {
    /// Attachment id, as shown by `catmail read`
    id: i64,
    /// Where to write it [default: stdout, unless that is a terminal]
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Args)]
struct Search {
    /// Words or "quoted phrases", all of which must match; prefix with from:, to: or
    /// subject: to search one field. Chinese, Japanese and Korean match as substrings
    #[arg(required = true, num_args = 1..)]
    query: Vec<String>,
    /// Account id or email address [default: every account]
    #[arg(short, long)]
    account: Option<String>,
    /// Only this folder (needs --account if there are several accounts)
    #[arg(long)]
    folder: Option<String>,
    /// Maximum number of messages
    #[arg(short = 'n', long, default_value_t = 50)]
    limit: u32,
    /// Skip this many of the newest matches
    #[arg(long, default_value_t = 0)]
    offset: u32,
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
        // Errors can carry server-supplied text, so they are escaped too; line breaks
        // stay, for the hints some errors end with.
        Err(e) => {
            eprintln!("Error: {}", printable_text(&format!("{e:#}")));
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
        Command::Read(args) => read(&client, args, cli.json).await,
        Command::Attachment(args) => save_attachment(&client, args).await,
        Command::Search(args) => search(&client, args, cli.json).await,
        Command::Mark(args) => ops::mark(&client, args, cli.json).await,
        Command::Move(args) => ops::move_messages(&client, args, cli.json).await,
        Command::Remove(args) => ops::remove(&client, args, cli.json).await,
        Command::Ops { command } => {
            let command = command.unwrap_or(ops::OpsCommand::List);
            ops::ops(&client, command, cli.json).await
        }
        Command::Compose(args) => compose::compose(&client, args, cli.json).await,
        Command::Reply(args) => compose::reply(&client, args, cli.json).await,
        Command::Draft(cmd) => compose::draft(&client, cmd, cli.json).await,
        Command::Send { draft } => compose::send(&client, DraftId(draft), cli.json).await,
        Command::Outbox { command } => {
            let command = command.unwrap_or(compose::OutboxCommand::List);
            compose::outbox(&client, command, cli.json).await
        }
        Command::Gc => gc(&client, cli.json).await,
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
            if json {
                println!("{}", serde_json::to_string_pretty(&account)?);
            } else {
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
            (folder.ops_applied, "local changes made"),
            (
                folder.ops_failed,
                "local changes refused and undone (see `catmail ops`)",
            ),
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
    let account = one_account(client, args.account.as_deref()).await?;
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
        print_message_table(&messages, None);
    }
    Ok(())
}

async fn read(client: &MailClient, args: Read, json: bool) -> Result<()> {
    let id = MessageId(args.id);
    if args.raw {
        let raw = client.raw_message(id).await?;
        let mut stdout = io::stdout().lock();
        if stdout.is_terminal() {
            write!(stdout, "{}", printable_text(&String::from_utf8_lossy(&raw)))?;
        } else {
            stdout.write_all(&raw)?;
        }
        return Ok(());
    }
    let message = if args.offline {
        client
            .get_message(id)
            .await?
            .with_context(|| format!("no message {id}"))?
    } else {
        client.download_message(id).await?
    };
    let mut saved = Vec::new();
    if let Some(dir) = &args.save {
        std::fs::create_dir_all(dir)?;
        // The ones listed; inline images are part of the HTML.
        for a in message
            .body
            .iter()
            .flat_map(|b| &b.attachments)
            .filter(|a| !a.inline)
        {
            let data = client.attachment_content(a.id).await?;
            let name = safe_filename(a.filename.as_deref(), &format!("attachment-{}", a.id));
            let path = files::write_new(dir, &name, &data)
                .with_context(|| format!("failed to save into {}", dir.display()))?;
            saved.push(path);
        }
    }
    if json {
        let mut value = serde_json::to_value(&message)?;
        if args.save.is_some() {
            // Where each one went: a name already taken gets a number.
            value["saved"] = serde_json::to_value(&saved)?;
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    print_message(&message, args.html);
    for path in saved {
        println!("Saved {}", printable(&path.display().to_string()));
    }
    Ok(())
}

fn print_message(message: &Message, html: bool) {
    let m = &message.summary;
    let header = |name: &str, value: &str| {
        if !value.is_empty() {
            println!("{name:<9}{}", printable(value));
        }
    };
    header("From:", &addresses(&m.from));
    header("To:", &addresses(&message.to));
    header("Cc:", &addresses(&message.cc));
    header("Date:", &output::date(m.date));
    header("Subject:", m.subject.as_deref().unwrap_or("(no subject)"));
    println!();
    let Some(body) = &message.body else {
        println!("(body not downloaded; run without --offline)");
        return;
    };
    let text = if html {
        body.html.as_deref().or(body.text.as_deref())
    } else {
        body.text.as_deref()
    };
    match text {
        Some(text) => println!("{}", printable_text(text.trim_end())),
        None => println!("(no text)"),
    }
    let attachments: Vec<_> = body.attachments.iter().filter(|a| !a.inline).collect();
    if !attachments.is_empty() {
        println!();
        let rows = attachments
            .iter()
            .map(|a| {
                vec![
                    a.id.to_string(),
                    a.filename.clone().unwrap_or_else(|| "(unnamed)".into()),
                    a.mime_type.clone(),
                    match a.size {
                        Some(size) if a.downloaded => output::size(size.into()),
                        Some(size) => format!("~{}", output::size(size.into())),
                        None => String::new(),
                    },
                ]
            })
            .collect();
        print_table(&["ATTACHMENT", "NAME", "TYPE", "SIZE"], rows);
    }
}

fn addresses(list: &[Address]) -> String {
    list.iter()
        .map(Address::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

async fn save_attachment(client: &MailClient, args: SaveAttachment) -> Result<()> {
    let data = client.attachment_content(AttachmentId(args.id)).await?;
    match args.output {
        Some(path) => std::fs::write(&path, &data)
            .with_context(|| format!("failed to write {}", path.display()))?,
        None => {
            let mut stdout = io::stdout().lock();
            if stdout.is_terminal() {
                bail!("refusing to write an attachment to the terminal; use --output");
            }
            stdout.write_all(&data)?;
        }
    }
    Ok(())
}

async fn search(client: &MailClient, args: Search, json: bool) -> Result<()> {
    let mut query = SearchQuery::new(args.query.join(" "));
    query.limit = args.limit;
    query.offset = args.offset;
    if let Some(name) = &args.folder {
        let account = one_account(client, args.account.as_deref()).await?;
        let folder = client
            .find_folder(account.id, name)
            .await?
            .with_context(|| format!("no folder `{name}` in {}", account.email))?;
        query.account = Some(account.id);
        query.folder = Some(folder.id);
    } else if let Some(selector) = &args.account {
        query.account = Some(resolve_account(client, selector).await?.id);
    }
    let messages = client.search(query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&messages)?);
        return Ok(());
    }
    if messages.is_empty() {
        println!("No matches.");
        return Ok(());
    }
    let accounts: BTreeSet<AccountId> = messages.iter().map(|m| m.account_id).collect();
    let folders = folder_names(client, accounts).await?;
    print_message_table(&messages, Some(&folders));
    Ok(())
}

/// Display names of the accounts' folders, for FOLDER columns.
async fn folder_names(
    client: &MailClient,
    accounts: impl IntoIterator<Item = AccountId>,
) -> Result<HashMap<FolderId, String>> {
    let mut folders = HashMap::new();
    for account in accounts {
        for folder in client.list_folders(account).await? {
            folders.insert(folder.id, folder.name);
        }
    }
    Ok(folders)
}

async fn gc(client: &MailClient, json: bool) -> Result<()> {
    let report = client.collect_garbage().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Removed {} unreferenced files ({}).",
            report.removed,
            output::size(report.bytes)
        );
    }
    Ok(())
}

/// The account `selector` names, or the only account there is.
async fn one_account(client: &MailClient, selector: Option<&str>) -> Result<Account> {
    match selector {
        Some(selector) => resolve_account(client, selector).await,
        None => {
            let mut accounts = client.list_accounts().await?;
            match accounts.len() {
                0 => bail!("no accounts; add one with `catmail account add`"),
                1 => Ok(accounts.remove(0)),
                _ => bail!("there are several accounts; choose one with --account"),
            }
        }
    }
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

/// With `folders`, adds a FOLDER column.
fn print_message_table(messages: &[MessageSummary], folders: Option<&HashMap<FolderId, String>>) {
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
            let from = m
                .from
                .first()
                .map(|a| a.name.clone().unwrap_or_else(|| a.email.clone()))
                .unwrap_or_default();
            let mut row = vec![m.id.to_string(), marks, output::date(m.date)];
            if let Some(folders) = folders {
                let name = folders.get(&m.folder_id).cloned().unwrap_or_default();
                row.push(truncate(&name, 16));
            }
            row.push(truncate(&from, 24));
            row.push(m.subject.clone().unwrap_or_else(|| "(no subject)".into()));
            row
        })
        .collect();
    let header: &[&str] = match folders {
        Some(_) => &["ID", "", "DATE", "FOLDER", "FROM", "SUBJECT"],
        None => &["ID", "", "DATE", "FROM", "SUBJECT"],
    };
    print_table(header, rows);
}
