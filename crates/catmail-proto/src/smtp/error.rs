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

    #[error("server does not support {0}")]
    Unsupported(&'static str),

    #[error("authentication failed: {0}")]
    AuthFailed(String),

    /// The server answered with an error reply. The connection is still usable.
    #[error("{command} refused: {code} {text}")]
    Reply {
        command: &'static str,
        code: u16,
        text: String,
    },

    #[error("message is {size} bytes, more than the server accepts ({limit} bytes)")]
    TooLarge { size: usize, limit: u64 },

    /// Detected before anything was sent.
    #[error("invalid address `{0}`")]
    InvalidAddress(String),

    #[error("protocol error: {0}")]
    Protocol(String),
}

impl Error {
    /// Whether trying again later can't help: the server refused for good (5xx), or the
    /// message or settings are wrong. Network trouble and 4xx replies are temporary.
    pub fn is_permanent(&self) -> bool {
        match self {
            Self::Reply { code, .. } => *code >= 500,
            Self::Unsupported(_)
            | Self::AuthFailed(_)
            | Self::InvalidServerName(_)
            | Self::TooLarge { .. }
            | Self::InvalidAddress(_) => true,
            Self::Io(_) | Self::Timeout | Self::Closed | Self::Protocol(_) => false,
        }
    }

    /// Whether the connection can no longer be used.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Io(_) | Self::Timeout | Self::Closed | Self::Protocol(_)
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
