//! What the reading pane says of a message apart from its body: the header in short and
//! in full, and the attachments offered as files.

use std::path::PathBuf;

use catmail_core::{Attachment, Message};

use crate::format;

/// A message's header as the pane shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Header {
    pub subject: String,
    /// The sender's name, or address when there is no name.
    pub sender: String,
    /// The address, when the name took the first place.
    pub sender_email: Option<String>,
    /// What the avatar's color is picked by.
    pub sender_key: String,
    /// `To Bob, Carol · Cc Dan`, short names only.
    pub recipients: String,
    pub date: String,
    /// From, To, Cc, Reply-To and Date in full, those that are present, for "Details".
    pub details: Vec<(&'static str, String)>,
}

impl Header {
    pub fn of(message: &Message) -> Self {
        let summary = &message.summary;
        let details = [
            ("From", format::addresses(&summary.from)),
            ("To", format::addresses(&summary.to)),
            ("Cc", format::addresses(&summary.cc)),
            ("Reply-To", format::addresses(&message.reply_to)),
            ("Date", format::date(summary.date)),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .collect();
        let from = summary.from.first();
        let recipients = [("To", &summary.to), ("Cc", &summary.cc)]
            .into_iter()
            .filter(|(_, list)| !list.is_empty())
            .map(|(name, list)| format!("{name} {}", format::names(list)))
            .collect::<Vec<_>>()
            .join(" · ");
        Self {
            subject: format::subject(summary.subject.as_deref()).to_owned(),
            sender: format::sender(&summary.from).to_owned(),
            sender_email: from.filter(|a| a.name.is_some()).map(|a| a.email.clone()),
            sender_key: from.map(|a| a.email.clone()).unwrap_or_default(),
            recipients,
            date: format::date(summary.date),
            details,
        }
    }
}

/// The attachments offered as files: not the inline parts, which the body shows.
pub fn attachments(message: &Message) -> Vec<Attachment> {
    message
        .body
        .as_ref()
        .map(|body| {
            body.attachments
                .iter()
                .filter(|a| !a.inline)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// An attachment's name, and its button's label: the name and the size.
pub fn attachment_label(attachment: &Attachment) -> (String, String) {
    let name = attachment
        .filename
        .clone()
        .unwrap_or_else(|| "Unnamed attachment".into());
    let label = match attachment.size {
        Some(size) => format!("{name} · {}", format::size(size.into())),
        None => name.clone(),
    };
    (name, label)
}

/// Where the save dialog starts: the user's downloads, or else their home.
pub fn download_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| {
            dirs.download_dir()
                .map(|d| d.to_path_buf())
                .or_else(|| Some(dirs.home_dir().to_path_buf()))
        })
        .unwrap_or_else(std::env::temp_dir)
}
