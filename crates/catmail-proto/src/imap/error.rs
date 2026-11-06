use std::io;

use catmail_types::{ErrorKind, Security};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("connection failed: {0}")]
    Io(#[from] io::Error),

    #[error("invalid server name `{0}`")]
    InvalidServerName(String),

    /// The settings say TLS, and the server answered in plaintext.
    #[error(
        "the server on port {port} doesn't start with TLS; choose STARTTLS for it, or port \
         {tls} for TLS",
        tls = Security::Tls.default_imap_port()
    )]
    NotTls { port: u16 },

    #[error("server timed out")]
    Timeout,

    #[error("connection closed by server")]
    Closed,

    #[error("server closed the connection: {0}")]
    Bye(String),

    #[error("server refused the connection: {0}")]
    Rejected(String),

    #[error("server does not support {0}")]
    Unsupported(&'static str),

    #[error("authentication failed: {0}")]
    AuthFailed(String),

    /// The server answered a command with NO or BAD. The connection is still usable.
    #[error("{command} failed: {text}")]
    Command { command: &'static str, text: String },

    /// Part of the answer to a command couldn't be parsed. Results count only when
    /// whole (a missing LIST, SEARCH or EXISTS line would read as something deleted),
    /// so this is the default for every command but the header FETCH and the ones that
    /// change a mailbox (whose answer matters only for its status). The connection is
    /// still usable.
    #[error("{0} response from the server could not be parsed completely")]
    Incomplete(&'static str),

    /// The command can't carry this input (a NUL in a message to APPEND, say). Nothing
    /// was sent; the connection is still usable.
    #[error("{0}")]
    InvalidInput(String),

    #[error("protocol error: {0}")]
    Protocol(String),
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        match self {
            _ if self.is_over_limit() => ErrorKind::OverLimit,
            // A garbled conversation too: a new connection may do better.
            Self::Io(_)
            | Self::Timeout
            | Self::Closed
            | Self::Bye(_)
            | Self::Rejected(_)
            | Self::Protocol(_) => ErrorKind::Network,
            Self::AuthFailed(_) => ErrorKind::Auth,
            Self::Command { .. } | Self::Incomplete(_) => ErrorKind::Refused,
            Self::InvalidServerName(_)
            | Self::NotTls { .. }
            | Self::Unsupported(_)
            | Self::InvalidInput(_) => ErrorKind::Invalid,
        }
    }

    /// Whether the connection can no longer be used. Everything except a refused or
    /// incompletely parsed command (and invalid input detected before sending) means it
    /// must be dropped; so does a refusal saying the account went over its limit
    /// ([`is_over_limit`](Self::is_over_limit)): the next command would fare no better.
    pub fn is_fatal(&self) -> bool {
        self.is_over_limit()
            || !matches!(
                self,
                Self::Command { .. }
                    | Self::Incomplete(_)
                    | Self::InvalidServerName(_)
                    | Self::InvalidInput(_)
            )
    }

    /// Whether the server said the account went over its download limit, on logging in,
    /// with a BYE or refusing a command (by its words, see `OVER_LIMIT`). Then nothing
    /// else on the account works for a while either, and trying again only adds to it.
    pub fn is_over_limit(&self) -> bool {
        self.over_limit().is_some()
    }

    /// What the server said, if it said the account went over its download limit (see
    /// [`is_over_limit`](Self::is_over_limit)).
    pub fn over_limit(&self) -> Option<&str> {
        match self {
            Self::Command { text, .. }
            | Self::Bye(text)
            | Self::Rejected(text)
            | Self::AuthFailed(text) => says_over_limit(text).then_some(text.as_str()),
            _ => None,
        }
    }

    /// Whether the connection was gone, rather than the command refused: the error an
    /// idle connection that the server or a NAT box closed meanwhile shows. Trying again
    /// on a new connection may work.
    pub fn is_disconnected(&self) -> bool {
        matches!(
            self,
            Self::Io(_) | Self::Timeout | Self::Closed | Self::Bye(_)
        )
    }
}

/// What servers answer once an account went over its download limit, in lower case:
/// Gmail's `[ALERT] Account exceeded command or bandwidth limits` (on logging in or on a
/// command), NetEase's `Fetch volume limit exceed` (on FETCH) and its "flow over limit".
const OVER_LIMIT: &[&str] = &["bandwidth limit", "volume limit", "flow over limit"];

/// Whether a server's text says the account went over its download limit. Servers have
/// no response code for it, only words.
fn says_over_limit(text: &str) -> bool {
    let text = text.to_lowercase();
    OVER_LIMIT.iter().any(|t| text.contains(t))
}

impl From<crate::net::NetError> for Error {
    fn from(e: crate::net::NetError) -> Self {
        use crate::net::NetError;
        match e {
            NetError::Io(e) => Self::Io(e),
            NetError::Timeout => Self::Timeout,
            NetError::InvalidServerName(name) => Self::InvalidServerName(name),
            NetError::NotTls { port } => Self::NotTls { port },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_saying_the_account_went_over_its_limit_are_told_apart() {
        let refused = |text: &str| Error::Command {
            command: "UID FETCH",
            text: text.into(),
        };
        assert!(refused("Fetch volume limit exceed").is_over_limit());
        let over = refused("Fetch volume limit exceed");
        assert!(over.kind() == ErrorKind::OverLimit && over.is_fatal());
        assert!(!refused("[SERVERBUG] Message can't be read").is_fatal());
        assert!(
            Error::AuthFailed("[ALERT] Account exceeded command or bandwidth limits".into())
                .is_over_limit()
        );
        assert!(Error::Bye("Account exceeded bandwidth limits".into()).is_over_limit());
        assert!(!refused("[SERVERBUG] Message can't be read").is_over_limit());
        assert!(
            !Error::AuthFailed("[AUTHENTICATIONFAILED] Invalid credentials".into()).is_over_limit()
        );
        assert!(!Error::Protocol("bandwidth limits".into()).is_over_limit());
    }
}
