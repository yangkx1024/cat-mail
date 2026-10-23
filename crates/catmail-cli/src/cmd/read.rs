//! `catmail read` and `catmail attachment`: a message and what it carries.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use catmail_core::{AttachmentId, MailClient, MessageId, safe_filename};
use clap::Args;

use crate::files;
use crate::json::{self, JsonRead};
use crate::output::{print_message, printable, printable_text};

#[derive(Args)]
pub(crate) struct Read {
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
pub(crate) struct SaveAttachment {
    /// Attachment id, as shown by `catmail read`
    id: i64,
    /// A new file to write it to [default: stdout, unless that is a terminal]
    #[arg(short, long)]
    output: Option<PathBuf>,
}

pub(crate) async fn read(client: &MailClient, args: Read, json: bool) -> Result<()> {
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
        return json::print(&JsonRead {
            message: (&message).into(),
            // Where each one went: a name already taken gets a number.
            saved: args.save.is_some().then_some(saved),
        });
    }
    print_message(&message, args.html);
    for path in saved {
        println!("Saved {}", printable(&path.display().to_string()));
    }
    Ok(())
}

pub(crate) async fn save_attachment(client: &MailClient, args: SaveAttachment) -> Result<()> {
    let data = client.attachment_content(AttachmentId(args.id)).await?;
    match args.output {
        Some(path) => {
            if let Err(e) = files::write_new_file(&path, &data) {
                if e.kind() == io::ErrorKind::AlreadyExists {
                    bail!(
                        "{} already exists; choose another name, or write to stdout",
                        path.display()
                    );
                }
                return Err(e).with_context(|| format!("failed to write {}", path.display()));
            }
        }
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
