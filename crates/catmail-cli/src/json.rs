//! What `--json` prints. Scripts depend on it, so it is spelled out here rather than
//! derived from `catmail_core`'s types: renaming a field there must not change it.
//! Every document is an object with `schema_version`, which goes up when a field
//! changes meaning or goes away; added fields don't change it. Times are RFC 3339 in
//! UTC, flags and states are lowercase words.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use catmail_core::{
    Account, Address, Attachment, AuthKind, Draft, DraftAttachment, ErrorKind, Event, Flags,
    Folder, FolderRole, FolderSyncReport, GcReport, Message, MessageSummary, OpKind, OpStatus,
    OutboxEntry, OutboxStatus, PendingOp, ReplayReport, ReportedError, Security, ServerConfig,
    SyncReport,
};
use serde::Serialize;

const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
struct Document<'a, T> {
    schema_version: u32,
    #[serde(flatten)]
    body: &'a T,
}

/// Prints `body`, an object, as an indented document.
pub(crate) fn print(body: &impl Serialize) -> Result<()> {
    println!("{}", to_string(body, true)?);
    Ok(())
}

/// Prints a document with `value` under `key`.
pub(crate) fn print_keyed(key: &'static str, value: &impl Serialize) -> Result<()> {
    print(&keyed(key, value))
}

/// Prints a document with each of `items`, as a `J`, in a list under `key`.
pub(crate) fn print_list<'a, J: From<&'a T> + Serialize, T: 'a>(
    key: &'static str,
    items: impl IntoIterator<Item = &'a T>,
) -> Result<()> {
    print_keyed(key, &list::<J, T>(items))
}

fn keyed<T>(key: &'static str, value: T) -> BTreeMap<&'static str, T> {
    BTreeMap::from([(key, value)])
}

/// Prints `body` as a document on one line, for streams of them.
pub(crate) fn print_line(body: &impl Serialize) -> Result<()> {
    println!("{}", to_string(body, false)?);
    Ok(())
}

fn to_string(body: &impl Serialize, pretty: bool) -> Result<String> {
    let document = Document {
        schema_version: SCHEMA_VERSION,
        body,
    };
    Ok(if pretty {
        serde_json::to_string_pretty(&document)?
    } else {
        serde_json::to_string(&document)?
    })
}

fn time(unix: i64) -> Option<String> {
    jiff::Timestamp::from_second(unix)
        .ok()
        .map(|t| t.to_string())
}

fn flags(flags: Flags) -> Vec<&'static str> {
    [
        (Flags::SEEN, "seen"),
        (Flags::FLAGGED, "flagged"),
        (Flags::ANSWERED, "answered"),
        (Flags::DRAFT, "draft"),
        (Flags::DELETED, "deleted"),
    ]
    .into_iter()
    .filter(|&(flag, _)| flags.contains(flag))
    .map(|(_, name)| name)
    .collect()
}

#[derive(Serialize)]
pub(crate) struct JsonAccount {
    id: i64,
    email: String,
    display_name: Option<String>,
    username: String,
    imap: JsonServer,
    smtp: JsonServer,
    auth: &'static str,
    created_at: Option<String>,
}

impl From<&Account> for JsonAccount {
    fn from(a: &Account) -> Self {
        Self {
            id: a.id.0,
            email: a.email.clone(),
            display_name: a.display_name.clone(),
            username: a.username.clone(),
            imap: (&a.imap).into(),
            smtp: (&a.smtp).into(),
            auth: match a.auth {
                AuthKind::Password => "password",
            },
            created_at: time(a.created_at),
        }
    }
}

#[derive(Serialize)]
struct JsonServer {
    host: String,
    port: u16,
    security: &'static str,
}

impl From<&ServerConfig> for JsonServer {
    fn from(s: &ServerConfig) -> Self {
        Self {
            host: s.host.clone(),
            port: s.port,
            security: match s.security {
                Security::Tls => "tls",
                Security::StartTls => "starttls",
                Security::Plain => "plain",
            },
        }
    }
}

#[derive(Serialize)]
pub(crate) struct JsonError {
    kind: &'static str,
    message: String,
}

impl JsonError {
    pub(crate) fn new(kind: ErrorKind, message: String) -> Self {
        let kind = match kind {
            ErrorKind::Network => "network",
            ErrorKind::Auth => "auth",
            ErrorKind::Refused => "refused",
            ErrorKind::OutOfSync => "out_of_sync",
            ErrorKind::NotFound => "not_found",
            ErrorKind::Invalid => "invalid",
            ErrorKind::Conflict => "conflict",
            ErrorKind::Internal => "internal",
        };
        Self { kind, message }
    }
}

impl From<&ReportedError> for JsonError {
    fn from(e: &ReportedError) -> Self {
        Self::new(e.kind, e.message.clone())
    }
}

/// One account's part of `catmail sync`: its folders, or why it failed as a whole.
#[derive(Serialize)]
pub(crate) struct JsonAccountSync {
    account: i64,
    email: String,
    folders: Vec<JsonFolderSync>,
    error: Option<JsonError>,
}

impl JsonAccountSync {
    pub(crate) fn new(account: &Account, result: Result<SyncReport, JsonError>) -> Self {
        let (folders, error) = match result {
            Ok(report) => (list(&report.folders), None),
            Err(e) => (Vec::new(), Some(e)),
        };
        Self {
            account: account.id.0,
            email: account.email.clone(),
            folders,
            error,
        }
    }

    pub(crate) fn failed(&self) -> bool {
        self.error.is_some()
    }
}

#[derive(Serialize)]
pub(crate) struct JsonFolderSync {
    folder: i64,
    name: String,
    added: u32,
    updated: u32,
    removed: u32,
    changes_applied: u32,
    changes_refused: u32,
    uidvalidity_changed: bool,
    error: Option<JsonError>,
}

impl From<&FolderSyncReport> for JsonFolderSync {
    fn from(f: &FolderSyncReport) -> Self {
        Self {
            folder: f.folder.0,
            name: f.name.clone(),
            added: f.added,
            updated: f.updated,
            removed: f.removed,
            changes_applied: f.ops_applied,
            changes_refused: f.ops_failed,
            uidvalidity_changed: f.uidvalidity_changed,
            error: f.error.as_ref().map(Into::into),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct JsonFolder {
    id: i64,
    account: i64,
    name: String,
    delimiter: Option<char>,
    role: Option<&'static str>,
    selectable: bool,
    unread: u32,
    total: u32,
    last_synced_at: Option<String>,
}

impl From<&Folder> for JsonFolder {
    fn from(f: &Folder) -> Self {
        Self {
            id: f.id.0,
            account: f.account_id.0,
            name: f.name.clone(),
            delimiter: f.delimiter,
            role: f.role.map(|role| match role {
                FolderRole::Inbox => "inbox",
                FolderRole::Sent => "sent",
                FolderRole::Drafts => "drafts",
                FolderRole::Trash => "trash",
                FolderRole::Junk => "junk",
                FolderRole::Archive => "archive",
                FolderRole::All => "all",
                FolderRole::Flagged => "flagged",
            }),
            selectable: f.selectable,
            unread: f.unread_count,
            total: f.total_count,
            last_synced_at: f.last_synced_at.and_then(time),
        }
    }
}

#[derive(Serialize)]
struct JsonAddress {
    name: Option<String>,
    email: String,
}

fn addresses(list: &[Address]) -> Vec<JsonAddress> {
    list.iter()
        .map(|a| JsonAddress {
            name: a.name.clone(),
            email: a.email.clone(),
        })
        .collect()
}

#[derive(Serialize)]
pub(crate) struct JsonSummary {
    id: i64,
    account: i64,
    folder: i64,
    subject: Option<String>,
    from: Vec<JsonAddress>,
    date: Option<String>,
    snippet: Option<String>,
    flags: Vec<&'static str>,
    has_attachments: bool,
}

impl From<&MessageSummary> for JsonSummary {
    fn from(m: &MessageSummary) -> Self {
        Self {
            id: m.id.0,
            account: m.account_id.0,
            folder: m.folder_id.0,
            subject: m.subject.clone(),
            from: addresses(&m.from),
            date: m.date.and_then(time),
            snippet: m.snippet.clone(),
            flags: flags(m.flags),
            has_attachments: m.has_attachments,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct JsonMessage {
    #[serde(flatten)]
    summary: JsonSummary,
    to: Vec<JsonAddress>,
    cc: Vec<JsonAddress>,
    reply_to: Vec<JsonAddress>,
    message_id: Option<String>,
    references: Vec<String>,
    size: Option<u32>,
    /// `None` until the body is downloaded.
    body: Option<JsonBody>,
}

#[derive(Serialize)]
struct JsonBody {
    text: Option<String>,
    html: Option<String>,
    attachments: Vec<JsonAttachment>,
}

#[derive(Serialize)]
struct JsonAttachment {
    id: i64,
    filename: Option<String>,
    mime_type: String,
    size: Option<u32>,
    content_id: Option<String>,
    inline: bool,
    downloaded: bool,
}

impl From<&Attachment> for JsonAttachment {
    fn from(a: &Attachment) -> Self {
        Self {
            id: a.id.0,
            filename: a.filename.clone(),
            mime_type: a.mime_type.clone(),
            size: a.size,
            content_id: a.content_id.clone(),
            inline: a.inline,
            downloaded: a.downloaded,
        }
    }
}

impl From<&Message> for JsonMessage {
    fn from(m: &Message) -> Self {
        Self {
            summary: (&m.summary).into(),
            to: addresses(&m.to),
            cc: addresses(&m.cc),
            reply_to: addresses(&m.reply_to),
            message_id: m.message_id.clone(),
            references: m.references.clone(),
            size: m.size,
            body: m.body.as_ref().map(|b| JsonBody {
                text: b.text.clone(),
                html: b.html.clone(),
                attachments: b.attachments.iter().map(Into::into).collect(),
            }),
        }
    }
}

/// `catmail read`: the message, and with `--save`, where its attachments went.
#[derive(Serialize)]
pub(crate) struct JsonRead {
    pub(crate) message: JsonMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) saved: Option<Vec<PathBuf>>,
}

#[derive(Serialize)]
pub(crate) struct JsonDraft {
    id: i64,
    account: i64,
    subject: String,
    to: Vec<JsonAddress>,
    cc: Vec<JsonAddress>,
    bcc: Vec<JsonAddress>,
    text: String,
    attachments: Vec<JsonDraftAttachment>,
    reply_to: Option<i64>,
    in_reply_to: Option<String>,
    references: Vec<String>,
    outbox: Option<i64>,
    on_server: bool,
    upload_error: Option<String>,
    updated_at: Option<String>,
}

impl From<&Draft> for JsonDraft {
    fn from(d: &Draft) -> Self {
        let c = &d.content;
        Self {
            id: d.id.0,
            account: d.account_id.0,
            subject: c.subject.clone(),
            to: addresses(&c.to),
            cc: addresses(&c.cc),
            bcc: addresses(&c.bcc),
            text: c.text.clone(),
            attachments: d.attachments.iter().map(Into::into).collect(),
            reply_to: d.reply_to.map(|id| id.0),
            in_reply_to: d.in_reply_to.clone(),
            references: d.references.clone(),
            outbox: d.outbox.map(|id| id.0),
            on_server: d.on_server,
            upload_error: d.upload_error.clone(),
            updated_at: time(d.updated_at),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct JsonDraftAttachment {
    id: i64,
    filename: String,
    mime_type: String,
    size: u64,
}

impl From<&DraftAttachment> for JsonDraftAttachment {
    fn from(a: &DraftAttachment) -> Self {
        Self {
            id: a.id.0,
            filename: a.filename.clone(),
            mime_type: a.mime_type.clone(),
            size: a.size,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct JsonOutboxEntry {
    id: i64,
    account: i64,
    draft: Option<i64>,
    subject: String,
    recipients: Vec<String>,
    message_id: String,
    status: &'static str,
    attempts: u32,
    next_attempt_at: Option<String>,
    last_error: Option<String>,
    created_at: Option<String>,
}

impl From<&OutboxEntry> for JsonOutboxEntry {
    fn from(e: &OutboxEntry) -> Self {
        Self {
            id: e.id.0,
            account: e.account_id.0,
            draft: e.draft_id.map(|id| id.0),
            subject: e.subject.clone(),
            recipients: e.recipients.clone(),
            message_id: e.message_id.clone(),
            status: match e.status {
                OutboxStatus::Queued => "queued",
                OutboxStatus::Sending => "sending",
                OutboxStatus::Accepted => "accepted",
                OutboxStatus::Unknown => "unknown",
                OutboxStatus::Done => "done",
                OutboxStatus::Failed => "failed",
            },
            attempts: e.attempts,
            next_attempt_at: e.next_attempt_at.and_then(time),
            last_error: e.last_error.clone(),
            created_at: time(e.created_at),
        }
    }
}

/// A change to messages waiting for the server, or refused by it.
#[derive(Serialize)]
pub(crate) struct JsonPendingOp {
    id: i64,
    account: i64,
    folder: i64,
    message: Option<i64>,
    subject: Option<String>,
    change: JsonChange,
    status: &'static str,
    attempts: u32,
    last_error: Option<String>,
    created_at: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JsonChange {
    Flags {
        add: Vec<&'static str>,
        remove: Vec<&'static str>,
    },
    Move {
        to: i64,
    },
    Delete,
}

impl From<&PendingOp> for JsonPendingOp {
    fn from(op: &PendingOp) -> Self {
        Self {
            id: op.id.0,
            account: op.account_id.0,
            folder: op.folder_id.0,
            message: op.message_id.map(|id| id.0),
            subject: op.subject.clone(),
            change: match op.kind {
                OpKind::Flags { add, remove } => JsonChange::Flags {
                    add: flags(add),
                    remove: flags(remove),
                },
                OpKind::Move { to } => JsonChange::Move { to: to.0 },
                OpKind::Delete => JsonChange::Delete,
            },
            status: match op.status {
                OpStatus::Pending => "pending",
                OpStatus::Failed => "failed",
                OpStatus::Conflict => "conflict",
            },
            attempts: op.attempts,
            last_error: op.last_error.clone(),
            created_at: time(op.created_at),
        }
    }
}

/// One account's part of making queued changes on the server.
#[derive(Serialize)]
pub(crate) struct JsonReplay {
    account: i64,
    applied: u32,
    refused: u32,
    pending: u32,
    drafts_uploaded: u32,
    drafts_refused: u32,
    error: Option<JsonError>,
}

impl JsonReplay {
    pub(crate) fn new(account: i64, r: &ReplayReport) -> Self {
        Self {
            account,
            applied: r.applied,
            refused: r.failed,
            pending: r.pending,
            drafts_uploaded: r.drafts_uploaded,
            drafts_refused: r.drafts_failed,
            error: r.error.as_ref().map(JsonError::from),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct JsonGc {
    removed: u64,
    bytes: u64,
}

impl From<&GcReport> for JsonGc {
    fn from(r: &GcReport) -> Self {
        Self {
            removed: r.removed,
            bytes: r.bytes,
        }
    }
}

/// A line of `catmail watch`: what changed, to be looked up again.
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum JsonEvent {
    AccountAdded { account: i64 },
    AccountRemoved { account: i64 },
    AccountStatusChanged { account: i64 },
    FoldersChanged { account: i64 },
    MessagesChanged { account: i64, folder: i64 },
    DraftsChanged { account: i64 },
    OutboxChanged { account: i64 },
    PendingOpsChanged { account: i64 },
    MessageSent { account: i64, outbox: i64 },
}

impl From<&Event> for JsonEvent {
    fn from(event: &Event) -> Self {
        match *event {
            Event::AccountAdded { account } => Self::AccountAdded { account: account.0 },
            Event::AccountRemoved { account } => Self::AccountRemoved { account: account.0 },
            Event::AccountStatusChanged { account } => {
                Self::AccountStatusChanged { account: account.0 }
            }
            Event::FoldersChanged { account } => Self::FoldersChanged { account: account.0 },
            Event::MessagesChanged { account, folder } => Self::MessagesChanged {
                account: account.0,
                folder: folder.0,
            },
            Event::DraftsChanged { account } => Self::DraftsChanged { account: account.0 },
            Event::OutboxChanged { account } => Self::OutboxChanged { account: account.0 },
            Event::PendingOpsChanged { account } => Self::PendingOpsChanged { account: account.0 },
            Event::MessageSent { account, outbox } => Self::MessageSent {
                account: account.0,
                outbox: outbox.0,
            },
        }
    }
}

fn list<'a, J: From<&'a T>, T: 'a>(items: impl IntoIterator<Item = &'a T>) -> Vec<J> {
    items.into_iter().map(J::from).collect()
}

#[cfg(test)]
mod tests {
    use catmail_core::{
        AccountId, AttachmentId, DraftAttachmentId, DraftContent, DraftId, FolderId, MessageBody,
        MessageId, OutboxId, PendingOpId,
    };
    use serde_json::json;

    use super::*;

    fn document(body: &impl Serialize) -> serde_json::Value {
        serde_json::from_str(&to_string(body, true).unwrap()).unwrap()
    }

    // The documents scripts read. A failure here means `--json` output changed: if
    // that was meant, a removed or renamed field needs a new SCHEMA_VERSION.
    #[test]
    fn message_snapshot() {
        let summary = MessageSummary {
            id: MessageId(42),
            account_id: AccountId(1),
            folder_id: FolderId(3),
            subject: Some("周报".into()),
            from: vec![Address {
                name: Some("张三".into()),
                email: "zhang@example.com".into(),
            }],
            date: Some(1_790_000_000),
            snippet: Some("本周".into()),
            flags: Flags::SEEN | Flags::FLAGGED,
            has_attachments: true,
        };
        let message = Message {
            summary: summary.clone(),
            to: vec![Address {
                name: None,
                email: "me@example.com".into(),
            }],
            cc: Vec::new(),
            reply_to: Vec::new(),
            message_id: Some("<a@example.com>".into()),
            references: Vec::new(),
            size: Some(2048),
            body: Some(MessageBody {
                text: Some("本周完成".into()),
                html: None,
                attachments: vec![Attachment {
                    id: AttachmentId(7),
                    message_id: MessageId(42),
                    filename: Some("报表.xlsx".into()),
                    mime_type: "application/vnd.ms-excel".into(),
                    size: Some(1024),
                    content_id: None,
                    inline: false,
                    downloaded: true,
                }],
            }),
        };

        assert_eq!(
            document(&keyed("messages", list::<JsonSummary, _>([&summary]))),
            json!({
                "schema_version": 1,
                "messages": [{
                    "id": 42,
                    "account": 1,
                    "folder": 3,
                    "subject": "周报",
                    "from": [{"name": "张三", "email": "zhang@example.com"}],
                    "date": "2026-09-21T14:13:20Z",
                    "snippet": "本周",
                    "flags": ["seen", "flagged"],
                    "has_attachments": true,
                }],
            })
        );
        assert_eq!(
            document(&JsonRead {
                message: (&message).into(),
                saved: Some(vec![PathBuf::from("报表.xlsx")]),
            }),
            json!({
                "schema_version": 1,
                "message": {
                    "id": 42,
                    "account": 1,
                    "folder": 3,
                    "subject": "周报",
                    "from": [{"name": "张三", "email": "zhang@example.com"}],
                    "date": "2026-09-21T14:13:20Z",
                    "snippet": "本周",
                    "flags": ["seen", "flagged"],
                    "has_attachments": true,
                    "to": [{"name": null, "email": "me@example.com"}],
                    "cc": [],
                    "reply_to": [],
                    "message_id": "<a@example.com>",
                    "references": [],
                    "size": 2048,
                    "body": {
                        "text": "本周完成",
                        "html": null,
                        "attachments": [{
                            "id": 7,
                            "filename": "报表.xlsx",
                            "mime_type": "application/vnd.ms-excel",
                            "size": 1024,
                            "content_id": null,
                            "inline": false,
                            "downloaded": true,
                        }],
                    },
                },
                "saved": ["报表.xlsx"],
            })
        );
    }

    #[test]
    fn sending_snapshot() {
        let draft = Draft {
            id: DraftId(5),
            account_id: AccountId(1),
            content: DraftContent {
                subject: "hi".into(),
                to: vec![Address {
                    name: None,
                    email: "b@example.com".into(),
                }],
                cc: Vec::new(),
                bcc: Vec::new(),
                text: "hello".into(),
            },
            attachments: vec![DraftAttachment {
                id: DraftAttachmentId(9),
                filename: "a.txt".into(),
                mime_type: "text/plain".into(),
                size: 3,
            }],
            reply_to: None,
            in_reply_to: None,
            references: Vec::new(),
            outbox: Some(OutboxId(2)),
            on_server: false,
            upload_error: None,
            updated_at: 0,
        };
        let entry = OutboxEntry {
            id: OutboxId(2),
            account_id: AccountId(1),
            draft_id: Some(DraftId(5)),
            subject: "hi".into(),
            recipients: vec!["b@example.com".into()],
            message_id: "<m@example.com>".into(),
            status: OutboxStatus::Unknown,
            attempts: 1,
            next_attempt_at: None,
            last_error: Some("connection lost".into()),
            created_at: 0,
        };
        let op = PendingOp {
            id: PendingOpId(4),
            account_id: AccountId(1),
            folder_id: FolderId(3),
            message_id: Some(MessageId(42)),
            subject: None,
            kind: OpKind::Flags {
                add: Flags::SEEN,
                remove: Flags::FLAGGED,
            },
            status: OpStatus::Pending,
            attempts: 0,
            last_error: None,
            created_at: 0,
        };

        assert_eq!(
            serde_json::to_value(JsonDraft::from(&draft)).unwrap(),
            json!({
                "id": 5,
                "account": 1,
                "subject": "hi",
                "to": [{"name": null, "email": "b@example.com"}],
                "cc": [],
                "bcc": [],
                "text": "hello",
                "attachments": [
                    {"id": 9, "filename": "a.txt", "mime_type": "text/plain", "size": 3},
                ],
                "reply_to": null,
                "in_reply_to": null,
                "references": [],
                "outbox": 2,
                "on_server": false,
                "upload_error": null,
                "updated_at": "1970-01-01T00:00:00Z",
            })
        );
        assert_eq!(
            serde_json::to_value(JsonOutboxEntry::from(&entry)).unwrap(),
            json!({
                "id": 2,
                "account": 1,
                "draft": 5,
                "subject": "hi",
                "recipients": ["b@example.com"],
                "message_id": "<m@example.com>",
                "status": "unknown",
                "attempts": 1,
                "next_attempt_at": null,
                "last_error": "connection lost",
                "created_at": "1970-01-01T00:00:00Z",
            })
        );
        assert_eq!(
            serde_json::to_value(JsonPendingOp::from(&op)).unwrap()["change"],
            json!({"kind": "flags", "add": ["seen"], "remove": ["flagged"]})
        );
        let sent = Event::MessageSent {
            account: AccountId(1),
            outbox: OutboxId(2),
        };
        assert_eq!(
            document(&JsonEvent::from(&sent)),
            json!({"schema_version": 1, "event": "message_sent", "account": 1, "outbox": 2})
        );
    }
}
