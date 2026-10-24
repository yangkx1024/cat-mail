use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{AccountId, Address, Folder, IdentityId, OutboxEntry, ReportedError};

/// Transport security for an IMAP or SMTP connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    /// TLS from the first byte (IMAPS 993, SMTPS 465).
    Tls,
    /// Plain connection upgraded with STARTTLS (IMAP 143, submission 587).
    StartTls,
    /// No encryption. Only meant for local test servers.
    Plain,
}

impl Security {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tls => "tls",
            Self::StartTls => "starttls",
            Self::Plain => "plain",
        }
    }

    pub fn default_imap_port(self) -> u16 {
        match self {
            Self::Tls => 993,
            Self::StartTls | Self::Plain => 143,
        }
    }

    pub fn default_smtp_port(self) -> u16 {
        match self {
            Self::Tls => 465,
            Self::StartTls => 587,
            Self::Plain => 25,
        }
    }
}

impl fmt::Display for Security {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Security {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tls" => Ok(Self::Tls),
            "starttls" => Ok(Self::StartTls),
            "plain" => Ok(Self::Plain),
            other => Err(format!(
                "unknown security mode `{other}` (expected tls, starttls or plain)"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

/// How an account's mailboxes map to messages (see `docs/architecture/labels.md`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountMode {
    /// Every selectable mailbox is a location.
    #[default]
    Standard,
    /// Gmail (`X-GM-EXT-1`, with All Mail listed): All Mail, Trash and Spam are the
    /// locations, the other mailboxes labels, and a message is one `X-GM-MSGID`.
    Gmail,
}

impl AccountMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Gmail => "gmail",
        }
    }
}

impl FromStr for AccountMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "standard" => Ok(Self::Standard),
            "gmail" => Ok(Self::Gmail),
            other => Err(format!("unknown account mode `{other}`")),
        }
    }
}

/// A mailbox on an IMAP server. What it sends as, and through which SMTP server, are its
/// [`Identity`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub email: String,
    pub display_name: Option<String>,
    /// Login name for IMAP (and for SMTP, with the identity it was added with); usually
    /// but not always the email address.
    pub username: String,
    pub imap: ServerConfig,
    /// As found when last connected to; standard until then.
    pub mode: AccountMode,
    /// Unix timestamp, seconds.
    pub created_at: i64,
}

/// Input for creating an account. `smtp` makes its first identity: its own address,
/// through that server, logging in as the account does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAccount {
    pub email: String,
    pub display_name: Option<String>,
    pub username: String,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
}

/// An address an account sends as, through an SMTP server and login of its own. Every
/// account has one made with it (its own address and SMTP server, its password), and
/// may add others: a sending service such as Resend, say. One of them is the default,
/// which new messages are from. Replies and copies still go through the account
/// (Drafts, Sent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub id: IdentityId,
    pub account_id: AccountId,
    /// The From address, and MAIL FROM.
    pub email: String,
    pub display_name: Option<String>,
    /// Login name for its SMTP server.
    pub username: String,
    pub smtp: ServerConfig,
    /// New messages are from it. Exactly one of an account's identities is.
    pub is_default: bool,
    /// Unix timestamp, seconds.
    pub created_at: i64,
}

impl Identity {
    /// What messages sent as it are from.
    pub fn address(&self) -> Address {
        Address {
            name: self.display_name.clone(),
            email: self.email.clone(),
        }
    }
}

/// Input for adding an [`Identity`] to an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewIdentity {
    pub email: String,
    pub display_name: Option<String>,
    pub username: String,
    pub smtp: ServerConfig,
}

/// Whether the engine gets through to an account's servers, as its latest attempts
/// found: background sync, syncs and replays asked for, and sending. Kept in memory
/// only; an engine starts out knowing of no problem.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatus {
    /// Syncing with the IMAP server failed.
    pub sync: Option<ServerProblem>,
    /// Reaching identities' SMTP servers to send failed, in identity order. The messages
    /// queued for each wait meanwhile, without any of them counting it as an attempt.
    pub send: Vec<SendProblem>,
}

/// An identity whose SMTP server couldn't be reached to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendProblem {
    pub identity: IdentityId,
    /// The address it sends as.
    pub email: String,
    pub problem: ServerProblem,
}

/// An account with what is shown next to it, read at once: its folders, its drafts and
/// what waits in its outbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountOverview {
    pub account: Account,
    pub folders: Vec<Folder>,
    /// The default first, then the others oldest first.
    pub identities: Vec<Identity>,
    /// Drafts not handed to "send".
    pub drafts: usize,
    /// Not sent yet (sent ones leave the outbox), oldest first.
    pub outbox: Vec<OutboxEntry>,
}

/// Failures in a row reaching a server, or working with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerProblem {
    /// The latest error. If its kind is [`ErrorKind::Auth`](crate::ErrorKind::Auth),
    /// the server rejected the login: retrying is slowed down, since servers lock
    /// accounts after repeated failed logins, and the password needs fixing.
    pub error: ReportedError,
    /// When the first of these failures happened, in unix seconds.
    pub since: i64,
    pub failures: u32,
}
