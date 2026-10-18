use std::io;

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

    /// Part of the answer to a command couldn't be parsed. Results count only when
    /// whole (a missing LIST, SEARCH or EXISTS line would read as something deleted),
    /// so this is the default for every command but the header FETCH. The connection
    /// is still usable.
    #[error("{0} response from the server could not be parsed completely")]
    Incomplete(&'static str),

    #[error("protocol error: {0}")]
    Protocol(String),
}

impl Error {
    /// Whether the connection can no longer be used. Everything except a refused or
    /// incompletely parsed command (and invalid input detected before sending) means it
    /// must be dropped.
    pub fn is_fatal(&self) -> bool {
        !matches!(
            self,
            Self::Command { .. } | Self::Incomplete(_) | Self::InvalidServerName(_)
        )
    }
}
