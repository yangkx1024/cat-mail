//! Writing: `compose`, `reply`, `forward` and `draft`. Sending is `outbox.rs`.
//!
//! Drafts are saved to the server's Drafts folder at once, like changes to messages
//! (`ops.rs`); if that can't be done, the draft is still saved locally, and the next
//! `catmail sync` or `catmail ops flush` uploads it.

use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use catmail_core::{
    AccountId, Address, Draft, DraftAttachmentId, DraftContent, DraftId, MailClient, MessageId,
    display, parse_addresses,
};
use clap::{Args, Subcommand};

use crate::cmd::ops::{self, NEXT_FLUSH};
use crate::cmd::{one_account, outbox};
use crate::json::{self, JsonDraft, JsonDraftAttachment};
use crate::output::{print_draft, print_drafts, printable};

fn print_draft_json(draft: &Draft) -> Result<()> {
    json::print_keyed("draft", &JsonDraft::from(draft))
}

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
    #[command(flatten)]
    recipients: Recipients,
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
pub(crate) struct Recipients {
    /// Recipients, separated by commas or semicolons: `a@example.com, 张三 <zhang@example.com>`
    #[arg(long)]
    to: Option<String>,
    #[arg(long)]
    cc: Option<String>,
    /// Get the message without being shown to the other recipients
    #[arg(long)]
    bcc: Option<String>,
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

#[derive(Args)]
pub(crate) struct Forward {
    /// Message id, as shown by `catmail ls`
    id: i64,
    #[command(flatten)]
    recipients: Recipients,
    /// Your text, written above the forwarded message
    #[arg(long, conflicts_with = "body_file")]
    body: Option<String>,
    /// Read your text from a file, or from stdin with `-`
    #[arg(long, value_name = "FILE")]
    body_file: Option<PathBuf>,
    /// Attach a file (repeatable), besides the message's own attachments
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
        write_above(&text, &mut content);
        draft = client.update_draft(draft.id, content).await?;
    }
    finish_draft(client, draft, &args.attach, args.send, json).await
}

pub(crate) async fn forward(client: &MailClient, args: Forward, json: bool) -> Result<()> {
    // Before the draft exists, so a mistake doesn't leave one behind.
    let text = read_body(args.body, args.body_file.as_ref())?;
    let recipients = args.recipients.parse()?;
    if args.send && recipients.iter().flatten().all(Vec::is_empty) {
        bail!("--send needs a recipient: --to, --cc or --bcc");
    }
    let mut draft = client.forward(MessageId(args.id)).await?;
    if text.is_some() || recipients.iter().any(Option::is_some) {
        let mut content = draft.content.clone();
        if let Some(text) = text {
            write_above(&text, &mut content);
        }
        set_recipients(recipients, &mut content);
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
        return outbox::send(client, draft.id, json).await;
    }
    save_to_server(client, draft.account_id).await;
    draft = client
        .get_draft(draft.id)
        .await?
        .context("draft vanished")?;
    if json {
        print_draft_json(&draft)?;
    } else {
        println!(
            "Saved draft {}. Send it with `catmail send {}`.",
            draft.id, draft.id
        );
    }
    Ok(())
}

pub(crate) async fn draft(client: &MailClient, cmd: DraftCommand, json: bool) -> Result<()> {
    // The account whose drafts changed, to save them to the server.
    let changed = match cmd {
        DraftCommand::List { account } => {
            let account = one_account(client, account.as_deref()).await?;
            let drafts = client.list_drafts(account.id).await?;
            if json {
                json::print_list::<JsonDraft, _>("drafts", &drafts)?;
            } else {
                print_drafts(&drafts);
            }
            None
        }
        DraftCommand::Show { id } => {
            let draft = get_draft(client, id).await?;
            if json {
                print_draft_json(&draft)?;
            } else {
                print_draft(&draft);
            }
            None
        }
        DraftCommand::Edit { id, fields } => {
            let draft = get_draft(client, id).await?;
            let content = fields.apply(draft.content)?;
            let draft = client.update_draft(draft.id, content).await?;
            if json {
                print_draft_json(&draft)?;
            } else {
                println!("Saved draft {}.", draft.id);
            }
            Some(draft.account_id)
        }
        DraftCommand::Attach { id, files } => {
            let account = get_draft(client, id).await?.account_id;
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
                        display::size(a.size),
                        a.id
                    );
                }
                attached.push(a);
            }
            if json {
                json::print_list::<JsonDraftAttachment, _>("attachments", &attached)?;
            }
            Some(account)
        }
        DraftCommand::Detach { attachment } => {
            let draft = client
                .remove_draft_attachment(DraftAttachmentId(attachment))
                .await?;
            Some(draft.account_id)
        }
        DraftCommand::Rm { id } => {
            let account = get_draft(client, id).await?.account_id;
            client.delete_draft(DraftId(id)).await?;
            if !json {
                println!("Deleted draft {id}.");
            }
            Some(account)
        }
    };
    if let Some(account) = changed {
        save_to_server(client, account).await;
    }
    Ok(())
}

/// Saves the account's changed drafts to its Drafts folder, deleting what they replace
/// there (and making its other queued changes), warning if that can't be done: the
/// drafts are saved locally all the same.
pub(crate) async fn save_to_server(client: &MailClient, account: AccountId) {
    let error = match client.replay_pending(account).await {
        Ok(report) => {
            if report.drafts_failed > 0 {
                eprintln!("warning: {}", ops::refused_drafts(report.drafts_failed));
            }
            report.error.map(|e| e.to_string())
        }
        Err(e) => Some(e.to_string()),
    };
    if let Some(e) = error {
        eprintln!(
            "warning: the draft is saved here, but not in the server's Drafts folder yet \
             ({e}); {NEXT_FLUSH} puts it there"
        );
    }
}

/// To, Cc and Bcc, each `None` if it wasn't given.
type ParsedRecipients = [Option<Vec<Address>>; 3];

impl Recipients {
    fn parse(self) -> Result<ParsedRecipients> {
        let parse = |field: Option<String>| field.as_deref().map(parse_addresses).transpose();
        Ok([parse(self.to)?, parse(self.cc)?, parse(self.bcc)?])
    }
}

/// Replaces the recipients that were given.
fn set_recipients(recipients: ParsedRecipients, content: &mut DraftContent) {
    for (field, value) in [&mut content.to, &mut content.cc, &mut content.bcc]
        .into_iter()
        .zip(recipients)
    {
        if let Some(value) = value {
            *field = value;
        }
    }
}

/// The user's text in front of what a reply or forward starts with.
fn write_above(text: &str, content: &mut DraftContent) {
    content.text = format!("{}{}", text.trim_end(), content.text);
}

impl Fields {
    /// `content` with the fields that were given replaced.
    fn apply(self, mut content: DraftContent) -> Result<DraftContent> {
        set_recipients(self.recipients.parse()?, &mut content);
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
                recipients: Recipients {
                    to: Some("b@example.com".into()),
                    cc: None,
                    bcc: None,
                },
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
            body_file: Some(missing.clone()),
            attach: Vec::new(),
            send: false,
        };
        let err = reply(&client, args, false).await.unwrap_err();
        assert!(format!("{err:#}").contains("failed to read"), "{err:#}");

        let forwarding = |to: Option<&str>, body_file: Option<PathBuf>, send| Forward {
            id: 1,
            recipients: Recipients {
                to: to.map(Into::into),
                cc: None,
                bcc: None,
            },
            body: None,
            body_file,
            attach: Vec::new(),
            send,
        };
        let err = forward(
            &client,
            forwarding(Some("b@example.com"), Some(missing.clone()), false),
            false,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("failed to read"), "{err:#}");
        let err = forward(
            &client,
            forwarding(Some("not an address"), None, false),
            false,
        )
        .await
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("not an email address"),
            "{err:#}"
        );
        let err = forward(&client, forwarding(None, None, true), false)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("needs a recipient"), "{err:#}");

        let account = client.list_accounts().await.unwrap()[0].id;
        assert!(client.list_drafts(account).await.unwrap().is_empty());
    }
}
