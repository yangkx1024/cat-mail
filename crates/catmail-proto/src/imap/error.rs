use std::io;

use catmail_types::ErrorKind;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("connection failed: {0}")]
    Io(#[from] io::Error),

    #[error("invalid server name `{0}`")]
    InvalidServerName(String),

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

    /// The server refused a command before all of it was sent (an APPEND, instead of
    /// asking for its message). The client is still in the middle of that command, so
    /// the connection can't be used any more.
    #[error("{command} refused: {text}")]
    Aborted { command: &'static str, text: String },

    /// Part of the answer to a command couldn't be parsed. Results count only when
    /// whole (a missing LIST, SEARCH or EXISTS line would read as something deleted),
    /// so this is the default for every command but the header FETCH. The connection
    /// is still usable.
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
            // A garbled conversation too: a new connection may do better.
            Self::Io(_)
            | Self::Timeout
            | Self::Closed
            | Self::Bye(_)
            | Self::Rejected(_)
            | Self::Protocol(_) => ErrorKind::Network,
            Self::AuthFailed(_) => ErrorKind::Auth,
            Self::Command { .. } | Self::Aborted { .. } | Self::Incomplete(_) => ErrorKind::Refused,
            Self::InvalidServerName(_) | Self::Unsupported(_) | Self::InvalidInput(_) => {
                ErrorKind::Invalid
            }
        }
    }

    /// Whether the connection can no longer be used. Everything except a refused or
    /// incompletely parsed command (and invalid input detected before sending) means it
    /// must be dropped.
    pub fn is_fatal(&self) -> bool {
        !matches!(
            self,
            Self::Command { .. }
                | Self::Incomplete(_)
                | Self::InvalidServerName(_)
                | Self::InvalidInput(_)
        )
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

impl From<crate::net::NetError> for Error {
    fn from(e: crate::net::NetError) -> Self {
        use crate::net::NetError;
        match e {
            NetError::Io(e) => Self::Io(e),
            NetError::Timeout => Self::Timeout,
            NetError::InvalidServerName(name) => Self::InvalidServerName(name),
        }
    }
}
