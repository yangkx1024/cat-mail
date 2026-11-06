//! Accounts, folders and messages for the models' tests.

use catmail_core::{
    Account, AccountId, AccountMode, AccountOverview, AccountStatus, Flags, Folder, FolderId,
    FolderKind, FolderRole, MessageId, MessageSummary, OutboxEntry, OutboxId, OutboxStatus,
    Security, ServerConfig,
};

use crate::mailbox::AccountEntry;

/// A folder of account `account`, nested by `/`; INBOX and the usual names have their role.
pub fn folder(id: i64, account: i64, name: &str) -> Folder {
    let role = match name.rsplit('/').next().unwrap_or(name) {
        "INBOX" => Some(FolderRole::Inbox),
        "Sent" => Some(FolderRole::Sent),
        "Archive" => Some(FolderRole::Archive),
        "All Mail" => Some(FolderRole::All),
        "Drafts" => Some(FolderRole::Drafts),
        "Trash" => Some(FolderRole::Trash),
        "Spam" => Some(FolderRole::Junk),
        _ => None,
    };
    Folder {
        id: FolderId(id),
        account_id: AccountId(account),
        name: name.into(),
        delimiter: Some('/'),
        role,
        kind: FolderKind::Location,
        listed: true,
        selectable: true,
        inferiors: true,
        unread_count: 0,
        total_count: 0,
        last_synced_at: None,
    }
}

/// The same, with `unread` messages unread.
pub fn unread(folder: Folder, unread: u32) -> Folder {
    Folder {
        unread_count: unread,
        ..folder
    }
}

/// On Gmail, every mailbox but All Mail, Trash and Spam is a label.
pub fn gmail_folder(id: i64, account: i64, name: &str) -> Folder {
    let folder = folder(id, account, name);
    let location = matches!(
        folder.role,
        Some(FolderRole::All | FolderRole::Trash | FolderRole::Junk)
    );
    Folder {
        kind: if location {
            FolderKind::Location
        } else {
            FolderKind::Label
        },
        ..folder
    }
}

pub fn overview(id: i64, folders: Vec<Folder>) -> AccountOverview {
    let server = ServerConfig {
        host: "imap.example.com".into(),
        port: 993,
        security: Security::Tls,
    };
    AccountOverview {
        account: Account {
            id: AccountId(id),
            email: format!("user{id}@example.com"),
            username: format!("user{id}"),
            imap: server,
            mode: Some(AccountMode::Standard),
            gmail_server: false,
            created_at: 0,
        },
        folders,
        identities: Vec::new(),
        drafts: 0,
        outbox: Vec::new(),
    }
}

/// Account `id` with these folders.
pub fn account(id: i64, folders: Vec<Folder>) -> AccountEntry {
    AccountEntry::new(overview(id, folders), AccountStatus::default())
}

/// Account `id` on Gmail, with these folders ([`gmail_folder`]).
pub fn gmail(id: i64, folders: Vec<Folder>) -> AccountEntry {
    let mut entry = account(id, folders);
    entry.account.mode = Some(AccountMode::Gmail);
    entry.account.gmail_server = true;
    entry
}

/// The standard account `id`: INBOX `id * 10 + 1`, Sent `+ 2`, Archive `+ 3`, Trash `+ 4`.
pub fn standard_overview(id: i64) -> AccountOverview {
    let folder = |n: i64, name: &str| folder(id * 10 + n, id, name);
    let folders = vec![
        folder(1, "INBOX"),
        folder(2, "Sent"),
        folder(3, "Archive"),
        folder(4, "Trash"),
    ];
    overview(id, folders)
}

/// The same, as the sidebar shows it.
pub fn standard(id: i64) -> AccountEntry {
    AccountEntry::new(standard_overview(id), AccountStatus::default())
}

pub fn message(id: i64, account: i64, folder: i64) -> MessageSummary {
    MessageSummary {
        id: MessageId(id),
        account_id: AccountId(account),
        folder_id: FolderId(folder),
        subject: Some(format!("Message {id}")),
        from: Vec::new(),
        to: Vec::new(),
        cc: Vec::new(),
        date: None,
        snippet: None,
        flags: Flags::empty(),
        has_attachments: false,
        labels: Vec::new(),
    }
}

/// Midday UTC on `day` (`2020-03-02`), in seconds: the same calendar day in nearly every
/// time zone the tests run in.
pub fn midday(day: &str) -> i64 {
    let at = format!("{day}T12:00:00Z").parse::<jiff::Timestamp>();
    at.expect("a day").as_second()
}

pub fn outbox_entry(id: i64, account: i64, status: OutboxStatus) -> OutboxEntry {
    OutboxEntry {
        id: OutboxId(id),
        account_id: AccountId(account),
        identity: catmail_core::IdentityId(1),
        draft_id: None,
        subject: String::new(),
        recipients: Vec::new(),
        message_id: String::new(),
        status,
        attempts: 0,
        next_attempt_at: None,
        last_error: None,
        created_at: 0,
    }
}
