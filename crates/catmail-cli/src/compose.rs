//! Writing and sending: `compose`, `reply`, `draft`, `send` and `outbox`.

use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use catmail_core::{
    Draft, DraftAttachmentId, DraftContent, DraftId, MailClient, MessageId, OutboxEntry, OutboxId,
    OutboxStatus, parse_addresses,
};
use clap::{Args, Subcommand};

use crate::output::{print_table, printable, printable_text, truncate};
use crate::{addresses, one_account, output};

#[derive(Args)]
pub(crate) struct Compose {
    /// Account id or email address to send from [default: the only account]
    #[arg(short, long)]
    account: Option<String>,
    #[command(flatten)]
    fields: Fields,
    /// Attach a file (repeatable); its content is copied in now
    #[arg(long, value_name = "FILE")]
    attach: Vec<PathBuf>,
    /// Send it right away instead of keeping it as a draft
    #[arg(long)]
    send: bool,
}

/// What `compose` and `draft edit` set. In `draft edit`, only the options given change.
#[derive(Args)]
pub(crate) struct Fields {
    /// Recipients, separated by commas or semicolons: `a@example.com, 张三 <zhang@example.com>`
    #[arg(long)]
    to: Option<String>,
    #[arg(long)]
    cc: Option<String>,
    /// Get the message without being shown to the other recipients
    #[arg(long)]
    bcc: Option<String>,
    #[arg(short, long)]
    subject: Option<String>,
    /// The text of the message
    #[arg(long, conflicts_with = "body_file")]
    body: Option<String>,
    /// Read the text from a file, or from stdin with `-`
    #[arg(long, value_name = "FILE")]
    body_file: Option<PathBuf>,
}

#[derive(Args)]
pub(crate) struct Reply {
    /// Message id, as shown by `catmail ls`
    id: i64,
    /// Reply to everyone the message went to, not just its sender
    #[arg(long)]
    all: bool,
    /// Your text, written above the quoted message
    #[arg(long, conflicts_with = "body_file")]
    body: Option<String>,
    /// Read your text from a file, or from stdin with `-`
    #[arg(long, value_name = "FILE")]
    body_file: Option<PathBuf>,
    /// Attach a file (repeatable)
    #[arg(long, value_name = "FILE")]
    attach: Vec<PathBuf>,
    /// Send it right away instead of keeping it as a draft
    #[arg(long)]
    send: bool,
}

#[derive(Subcommand)]
pub(crate) enum DraftCommand {
    /// List drafts (those being sent are in `catmail outbox`)
    List {
        /// Account id or email address [default: the only account]
        #[arg(short, long)]
        account: Option<String>,
    },
    /// Show a draft
    Show { id: i64 },
    /// Change a draft; only the options given change
    Edit {
        id: i64,
        #[command(flatten)]
        fields: Fields,
    },
    /// Attach files to a draft
    Attach {
        id: i64,
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Remove an attachment, by the id `catmail draft show` lists
    Detach { attachment: i64 },
    /// Delete a draft
    Rm { id: i64 },
}

#[derive(Subcommand)]
pub(crate) enum OutboxCommand {
    /// List messages that are not sent and saved yet
    List,
    /// Send what is queued and save what was sent to Sent, now
    Flush,
    /// Try a failed or unknown send again (an unknown one may arrive twice), or saving
    /// a sent one to Sent
    Retry { id: i64 },
    /// Confirm that a message with an unknown outcome arrived; it is then only saved
    /// to Sent
    MarkSent { id: i64 },
    /// Remove an entry. A message that wasn't sent goes back to the drafts
    Cancel { id: i64 },
}

pub(crate) async fn compose(client: &MailClient, args: Compose, json: bool) -> Result<()> {
    let account = one_account(client, args.account.as_deref()).await?;
    let content = args.fields.apply(DraftContent::default())?;
    let draft = client.create_draft(account.id, content).await?;
    finish_draft(client, draft, &args.attach, args.send, json).await
}

pub(crate) async fn reply(client: &MailClient, args: Reply, json: bool) -> Result<()> {
    // Before the draft exists, so a missing file doesn't leave one behind.
    let text = read_body(args.body, args.body_file.as_ref())?;
    let mut draft = client.reply(MessageId(args.id), args.all).await?;
    if let Some(text) = text {
        let mut content = draft.content.clone();
        content.text = format!("{}{}", text.trim_end(), content.text);
        draft = client.update_draft(draft.id, content).await?;
    }
    finish_draft(client, draft, &args.attach, args.send, json).await
}

/// Attaches the files, then sends the draft or shows it. If a file can't be attached,
/// the command fails as a whole and the new draft goes: the user, who was never told its
/// id, would only run the command again and get another one.
async fn finish_draft(
    client: &MailClient,
    mut draft: Draft,
    attach: &[PathBuf],
    send_now: bool,
    json: bool,
) -> Result<()> {
    for path in attach {
        if let Err(e) = client.attach_file(draft.id, path).await {
            let mut error =
                anyhow::Error::new(e).context(format!("failed to attach {}", path.display()));
            if let Err(e) = client.delete_draft(draft.id).await {
                error = error.context(format!("kept as draft {} ({e})", draft.id));
            }
            return Err(error);
        }
    }
    if send_now {
        return send(client, draft.id, json).await;
    }
    if !attach.is_empty() {
        draft = client
            .get_draft(draft.id)
            .await?
            .context("draft vanished")?;
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&draft)?);
    } else {
        println!(
            "Saved draft {}. Send it with `catmail send {}`.",
            draft.id, draft.id
        );
    }
    Ok(())
}

pub(crate) async fn draft(client: &MailClient, cmd: DraftCommand, json: bool) -> Result<()> {
    match cmd {
        DraftCommand::List { account } => {
            let account = one_account(client, account.as_deref()).await?;
            let drafts = client.list_drafts(account.id).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&drafts)?);
            } else if drafts.is_empty() {
                println!("No drafts.");
            } else {
                let rows = drafts
                    .iter()
                    .map(|d| {
                        vec![
                            d.id.to_string(),
                            output::date(Some(d.updated_at)),
                            truncate(&recipients(&d.content), 30),
                            if d.attachments.is_empty() { "" } else { "@" }.into(),
                            subject(&d.content.subject).to_owned(),
                        ]
                    })
                    .collect();
                print_table(&["ID", "CHANGED", "TO", "", "SUBJECT"], rows);
            }
        }
        DraftCommand::Show { id } => {
            let draft = get_draft(client, id).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&draft)?);
            } else {
                print_draft(&draft);
            }
        }
        DraftCommand::Edit { id, fields } => {
            let draft = get_draft(client, id).await?;
            let content = fields.apply(draft.content)?;
            let draft = client.update_draft(draft.id, content).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&draft)?);
            } else {
                println!("Saved draft {}.", draft.id);
            }
        }
        DraftCommand::Attach { id, files } => {
            let mut attached = Vec::new();
            for path in files {
                let a = client
                    .attach_file(DraftId(id), &path)
                    .await
                    .with_context(|| format!("failed to attach {}", path.display()))?;
                if !json {
                    println!(
                        "Attached {} ({}) as {}.",
                        printable(&a.filename),
                        output::size(a.size),
                        a.id
                    );
                }
                attached.push(a);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&attached)?);
            }
        }
        DraftCommand::Detach { attachment } => {
            client
                .remove_draft_attachment(DraftAttachmentId(attachment))
                .await?;
        }
        DraftCommand::Rm { id } => {
            client.delete_draft(DraftId(id)).await?;
            if !json {
                println!("Deleted draft {id}.");
            }
        }
    }
    Ok(())
}

/// Queues the draft and works through its account's outbox, reporting how this
/// message fared.
pub(crate) async fn send(client: &MailClient, id: DraftId, json: bool) -> Result<()> {
    let queued = client.send_draft(id).await?;
    flush_and_report(client, queued, json).await
}

/// Works through the entry's outbox and tells the user how the entry fared.
async fn flush_and_report(client: &MailClient, entry: OutboxEntry, json: bool) -> Result<()> {
    let results = client.flush_outbox(entry.account_id).await?;
    let entry = results
        .into_iter()
        .find(|e| e.id == entry.id)
        .unwrap_or(entry);
    report(&entry, json)
}

pub(crate) async fn outbox(client: &MailClient, cmd: OutboxCommand, json: bool) -> Result<()> {
    match cmd {
        OutboxCommand::List => {
            let entries = client.list_outbox(None).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                print_outbox(&entries);
            }
        }
        OutboxCommand::Flush => {
            // All at once: one unreachable server shouldn't hold up the other accounts.
            let mut flushes = tokio::task::JoinSet::new();
            for account in client.list_accounts().await? {
                let client = client.clone();
                flushes.spawn(async move { client.flush_outbox(account.id).await });
            }
            // One account failing doesn't hide what happened to the others' mail.
            let (mut results, mut errors) = (Vec::new(), Vec::new());
            for flushed in flushes.join_all().await {
                match flushed {
                    Ok(entries) => results.extend(entries),
                    Err(e) => errors.push(e.to_string()),
                }
            }
            results.sort_by_key(|e| e.id);
            if json {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else if !results.is_empty() {
                print_outbox(&results);
            } else if errors.is_empty() {
                println!("Nothing to send.");
            }
            // Like `catmail send`: an error unless everything went out.
            if !results.iter().all(|e| went_out(e.status)) {
                errors.push("not everything was sent; `catmail outbox` shows why".into());
            }
            if !errors.is_empty() {
                bail!("{}", errors.join("\n"));
            }
        }
        OutboxCommand::Retry { id } => {
            let entry = client.retry_outbox(OutboxId(id)).await?;
            flush_and_report(client, entry, json).await?;
        }
        OutboxCommand::MarkSent { id } => {
            let entry = client.mark_outbox_sent(OutboxId(id)).await?;
            flush_and_report(client, entry, json).await?;
        }
        OutboxCommand::Cancel { id } => {
            let entry = client.cancel_outbox(OutboxId(id)).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entry)?);
            } else {
                match entry {
                    OutboxEntry {
                        status: OutboxStatus::Accepted,
                        ..
                    } => println!("Removed {id}; the message was sent but not saved to Sent."),
                    OutboxEntry {
                        draft_id: Some(draft),
                        ..
                    } => println!("Cancelled {id}; it is draft {draft} again."),
                    _ => println!("Cancelled {id}."),
                }
            }
        }
    }
    Ok(())
}

/// The message was sent; at most saving it to Sent is left. Anything else is what
/// [`report`] turns into an error.
fn went_out(status: OutboxStatus) -> bool {
    matches!(status, OutboxStatus::Done | OutboxStatus::Accepted)
}

/// Tells the user how a send went and what they can do; an error unless it went out.
fn report(entry: &OutboxEntry, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(entry)?);
    }
    let id = entry.id;
    let error = entry.last_error.as_deref().unwrap_or("unknown error");
    match entry.status {
        OutboxStatus::Done => {
            if !json {
                println!("Sent.");
            }
            Ok(())
        }
        OutboxStatus::Accepted => {
            eprintln!(
                "Sent, but saving a copy to Sent failed: {}\n\
                 `catmail outbox flush` tries again.",
                printable(error)
            );
            Ok(())
        }
        OutboxStatus::Queued | OutboxStatus::Sending => bail!(
            "not sent yet: {}\n`catmail outbox flush` tries again; \
             `catmail outbox cancel {id}` makes it a draft again.",
            printable(error)
        ),
        OutboxStatus::Failed => bail!(
            "the server refused the message: {}\n\
             `catmail outbox cancel {id}` makes it a draft again to fix it; \
             `catmail outbox retry {id}` sends it as it is.",
            printable(error)
        ),
        OutboxStatus::Unknown => bail!(
            "the connection was lost after the message was handed over, so it may or may \
             not have been delivered: {}\n\
             Check with a recipient, then `catmail outbox mark-sent {id}` if it arrived, \
             `catmail outbox retry {id}` to send it again (it may arrive twice), or \
             `catmail outbox cancel {id}`.",
            printable(error)
        ),
    }
}

impl Fields {
    /// `content` with the fields that were given replaced.
    fn apply(self, mut content: DraftContent) -> Result<DraftContent> {
        for (field, value) in [
            (&mut content.to, self.to),
            (&mut content.cc, self.cc),
            (&mut content.bcc, self.bcc),
        ] {
            if let Some(value) = value {
                *field = parse_addresses(&value)?;
            }
        }
        if let Some(subject) = self.subject {
            content.subject = subject;
        }
        if let Some(text) = read_body(self.body, self.body_file.as_ref())? {
            content.text = text;
        }
        Ok(content)
    }
}

fn read_body(body: Option<String>, file: Option<&PathBuf>) -> Result<Option<String>> {
    let Some(path) = file else {
        return Ok(body);
    };
    let mut bytes = Vec::new();
    if path.as_os_str() == "-" {
        if io::stdin().is_terminal() {
            eprintln!("Type the message, then Ctrl-D:");
        }
        io::stdin().lock().read_to_end(&mut bytes)?;
    } else {
        bytes =
            std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    }
    String::from_utf8(bytes)
        .map(Some)
        .with_context(|| format!("{} is not UTF-8 text", path.display()))
}

async fn get_draft(client: &MailClient, id: i64) -> Result<Draft> {
    client
        .get_draft(DraftId(id))
        .await?
        .with_context(|| format!("no draft {id}"))
}

fn print_draft(draft: &Draft) {
    let c = &draft.content;
    let header = |name: &str, value: &str| {
        if !value.is_empty() {
            println!("{name:<9}{}", printable(value));
        }
    };
    header("To:", &addresses(&c.to));
    header("Cc:", &addresses(&c.cc));
    header("Bcc:", &addresses(&c.bcc));
    header("Subject:", subject(&c.subject));
    if let Some(outbox) = draft.outbox {
        header("Status:", &format!("being sent (outbox entry {outbox})"));
    }
    println!();
    println!("{}", printable_text(c.text.trim_end()));
    if !draft.attachments.is_empty() {
        println!();
        let rows = draft
            .attachments
            .iter()
            .map(|a| {
                vec![
                    a.id.to_string(),
                    a.filename.clone(),
                    a.mime_type.clone(),
                    output::size(a.size),
                ]
            })
            .collect();
        print_table(&["ATTACHMENT", "NAME", "TYPE", "SIZE"], rows);
    }
}

fn print_outbox(entries: &[OutboxEntry]) {
    if entries.is_empty() {
        println!("The outbox is empty.");
        return;
    }
    let rows = entries
        .iter()
        .map(|e| {
            let state = match (e.status, e.next_attempt_at) {
                (OutboxStatus::Queued | OutboxStatus::Accepted, Some(at)) if e.attempts > 0 => {
                    format!("{}, next try {}", e.status, output::date(Some(at)))
                }
                (status, _) => status.to_string(),
            };
            vec![
                e.id.to_string(),
                state,
                truncate(&e.recipients.join(", "), 30),
                truncate(subject(&e.subject), 30),
                e.last_error.clone().unwrap_or_default(),
            ]
        })
        .collect();
    print_table(&["ID", "STATUS", "TO", "SUBJECT", "LAST ERROR"], rows);
}

fn recipients(content: &DraftContent) -> String {
    content
        .recipients()
        .map(|a| a.name.clone().unwrap_or_else(|| a.email.clone()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn subject(subject: &str) -> &str {
    if subject.is_empty() {
        "(no subject)"
    } else {
        subject
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use catmail_core::{AuthKind, Config, MemorySecrets, NewAccount, Security, ServerConfig};
    use tokio::runtime::Handle;

    use super::*;

    async fn client_with_account(dir: &std::path::Path) -> MailClient {
        let client = MailClient::builder(Config::with_data_dir(dir))
            .runtime(Handle::current())
            .secrets(Arc::new(MemorySecrets::default()))
            .open()
            .unwrap();
        let server = |host: &str| ServerConfig {
            host: host.into(),
            port: 993,
            security: Security::Tls,
        };
        client
            .add_account(
                NewAccount {
                    email: "a@example.com".into(),
                    display_name: None,
                    username: "a".into(),
                    imap: server("imap.example.com"),
                    smtp: server("smtp.example.com"),
                    auth: AuthKind::Password,
                },
                "pw".into(),
            )
            .await
            .unwrap();
        client
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_command_leaves_no_draft() {
        let dir = tempfile::tempdir().unwrap();
        let client = client_with_account(dir.path()).await;
        let missing = dir.path().join("missing.pdf");

        let args = Compose {
            account: None,
            fields: Fields {
                to: Some("b@example.com".into()),
                cc: None,
                bcc: None,
                subject: Some("hi".into()),
                body: None,
                body_file: None,
            },
            attach: vec![missing.clone()],
            send: true,
        };
        let err = compose(&client, args, false).await.unwrap_err();
        assert!(format!("{err:#}").contains("missing.pdf"), "{err:#}");

        let args = Reply {
            id: 1,
            all: false,
            body: None,
            body_file: Some(missing),
            attach: Vec::new(),
            send: false,
        };
        let err = reply(&client, args, false).await.unwrap_err();
        assert!(format!("{err:#}").contains("failed to read"), "{err:#}");

        let account = client.list_accounts().await.unwrap()[0].id;
        assert!(client.list_drafts(account).await.unwrap().is_empty());
    }
}
