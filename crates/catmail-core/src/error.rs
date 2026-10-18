pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] catmail_store::Error),

    /// Any other protocol or network failure.
    #[error(transparent)]
    Imap(catmail_proto::imap::Error),

    /// The server rejected the credentials.
    #[error("login rejected by the server ({0}); check the username and password")]
    AuthFailed(String),

    /// The server lacks something the account's settings require, e.g. STARTTLS.
    #[error("the server does not support {0}; check the security setting and port")]
    Unsupported(&'static str),

    #[error("TLS setup failed: {0}")]
    Tls(String),

    #[error("credential store error: {0}")]
    Secrets(String),

    #[error("account {0} not found")]
    AccountNotFound(String),

    /// The account was removed while the operation ran.
    #[error("cancelled")]
    Cancelled,

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("could not determine a data directory; pass one explicitly")]
    NoDataDir,

    #[error("another cat-mail instance is already using {}", .0.display())]
    AlreadyRunning(std::path::PathBuf),

    #[error("background task failed: {0}")]
    Task(String),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<catmail_proto::imap::Error> for Error {
    /// Lifts the failures a user can act on, so frontends needn't interpret protocol
    /// errors.
    fn from(e: catmail_proto::imap::Error) -> Self {
        use catmail_proto::imap::Error as E;
        match e {
            E::AuthFailed(text) => Self::AuthFailed(text),
            E::Unsupported(what) => Self::Unsupported(what),
            other => Self::Imap(other),
        }
    }
}
