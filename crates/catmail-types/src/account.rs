use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{AccountId, Folder, OutboxEntry, ReportedError};

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

/// How the account authenticates. The secret itself never lives in the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    Password,
}

impl AuthKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
        }
    }
}

impl FromStr for AuthKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "password" => Ok(Self::Password),
            other => Err(format!("unknown auth kind `{other}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub email: String,
    pub display_name: Option<String>,
    /// Login name for IMAP/SMTP; usually but not always the email address.
    pub username: String,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    pub auth: AuthKind,
    /// Unix timestamp, seconds.
    pub created_at: i64,
}

/// Input for creating an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAccount {
    pub email: String,
    pub display_name: Option<String>,
    pub username: String,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    pub auth: AuthKind,
}

/// Whether the engine gets through to an account's servers, as its latest attempts
/// found: background sync, syncs and replays asked for, and sending. Kept in memory
/// only; an engine starts out knowing of no problem.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatus {
    /// Syncing with the IMAP server failed.
    pub sync: Option<ServerProblem>,
    /// Reaching the SMTP server to send failed. Queued messages wait meanwhile, without
    /// any of them counting it as an attempt.
    pub send: Option<ServerProblem>,
}

/// An account with what is shown next to it, read at once: its folders, its drafts and
/// what waits in its outbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountOverview {
    pub account: Account,
    pub folders: Vec<Folder>,
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
