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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountMode {
    /// Every selectable mailbox is a location.
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
    /// As found when its mailboxes were last listed, by a sync; `None` until the first.
    pub mode: Option<AccountMode>,
    /// The server is Gmail's (X-GM-EXT-1), as found when last connected to (logging in
    /// to check the account counts); false until then. Not the same as the Gmail mode,
    /// which also needs All Mail listed.
    pub gmail_server: bool,
    /// Unix timestamp, seconds.
    pub created_at: i64,
}

impl Account {
    /// A Gmail server with All Mail hidden from IMAP in Gmail's settings: the account is
    /// synced as a standard one, each label a folder, until it is shown again. Not known
    /// before the first sync.
    pub fn all_mail_hidden(&self) -> bool {
        self.gmail_server && self.mode == Some(AccountMode::Standard)
    }
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
    /// Whether its SMTP server saves what it sends to the account's Sent itself, as the
    /// user said; `None` until they do, for a guess (see
    /// [`server_saves_sent`](Self::server_saves_sent)).
    pub saves_sent: Option<bool>,
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

    /// Whether its SMTP server saves what it sends to the account's Sent itself, so that
    /// saving a copy would make a second one. Unless the user said, a guess: Gmail does,
    /// for mail sent through its own SMTP server with the account's login (the same login,
    /// on a server in the IMAP server's domain); what goes out elsewhere it never sees.
    /// Other servers are taken not to.
    pub fn server_saves_sent(&self, account: &Account) -> bool {
        // `imap.gmail.com` and `smtp.gmail.com`.
        fn domain(host: &str) -> &str {
            match host.split_once('.') {
                Some((_, domain)) if domain.contains('.') => domain,
                _ => host,
            }
        }
        self.saves_sent.unwrap_or_else(|| {
            account.gmail_server
                && self.username == account.username
                && domain(&self.smtp.host).eq_ignore_ascii_case(domain(&account.imap.host))
        })
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

impl ServerProblem {
    /// Whether the server rejected the login.
    pub fn login_rejected(&self) -> bool {
        self.error.kind == crate::ErrorKind::Auth
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str) -> ServerConfig {
        ServerConfig {
            host: host.into(),
            port: 465,
            security: Security::Tls,
        }
    }

    fn account(gmail_server: bool) -> Account {
        Account {
            id: AccountId(1),
            email: "alice@gmail.com".into(),
            display_name: None,
            username: "alice@gmail.com".into(),
            imap: server("imap.gmail.com"),
            mode: None,
            gmail_server,
            created_at: 0,
        }
    }

    fn identity(username: &str, smtp_host: &str) -> Identity {
        Identity {
            id: IdentityId(1),
            account_id: AccountId(1),
            email: "alice@gmail.com".into(),
            display_name: None,
            username: username.into(),
            smtp: server(smtp_host),
            is_default: true,
            saves_sent: None,
            created_at: 0,
        }
    }

    #[test]
    fn gmail_saves_what_goes_through_its_own_smtp_server_with_the_accounts_login() {
        let gmail = account(true);
        let own = identity("alice@gmail.com", "SMTP.gmail.com");
        assert!(own.server_saves_sent(&gmail));
        // Not known to be Gmail (not connected to yet), or not Gmail.
        assert!(!own.server_saves_sent(&account(false)));
        // Gmail never sees what goes out elsewhere.
        assert!(!identity("resend", "smtp.resend.com").server_saves_sent(&gmail));
        assert!(!identity("bob@gmail.com", "smtp.gmail.com").server_saves_sent(&gmail));
    }

    #[test]
    fn what_the_user_said_wins_over_the_guess() {
        let mut own = identity("alice@gmail.com", "smtp.gmail.com");
        own.saves_sent = Some(false);
        assert!(!own.server_saves_sent(&account(true)));
        let mut relay = identity("alice@gmail.com", "smtp-relay.example.net");
        relay.saves_sent = Some(true);
        assert!(relay.server_saves_sent(&account(false)));
    }

    #[test]
    fn all_mail_is_hidden_only_once_a_sync_found_gmail_without_it() {
        // Known to be Gmail's at login, not synced yet.
        let mut gmail = account(true);
        assert!(!gmail.all_mail_hidden());
        gmail.mode = Some(AccountMode::Gmail);
        assert!(!gmail.all_mail_hidden());
        gmail.mode = Some(AccountMode::Standard);
        assert!(gmail.all_mail_hidden());
        let mut other = account(false);
        other.mode = Some(AccountMode::Standard);
        assert!(!other.all_mail_hidden());
    }
}
