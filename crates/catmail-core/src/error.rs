pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(catmail_store::Error),

    /// Any other IMAP or network failure.
    #[error(transparent)]
    Imap(catmail_proto::imap::Error),

    /// Any other SMTP failure.
    #[error(transparent)]
    Smtp(catmail_proto::smtp::Error),

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

    #[error("folder {0} not found")]
    FolderNotFound(catmail_types::FolderId),

    #[error("message {0} not found")]
    MessageNotFound(catmail_types::MessageId),

    #[error("attachment {0} not found")]
    AttachmentNotFound(catmail_types::AttachmentId),

    #[error("draft {0} not found")]
    DraftNotFound(catmail_types::DraftId),

    #[error("draft attachment {0} not found")]
    DraftAttachmentNotFound(catmail_types::DraftAttachmentId),

    #[error("outbox entry {0} not found")]
    OutboxNotFound(catmail_types::OutboxId),

    /// The draft or outbox entry is in a state that doesn't allow this, e.g. editing a
    /// draft that is being sent. The text says why.
    #[error("{0}")]
    Conflict(String),

    /// The local copy no longer matches the server: the message was expunged, or its
    /// folder got a new UIDVALIDITY. The next sync brings it up to date.
    #[error("{0}; sync the account and try again")]
    OutOfSync(&'static str),

    /// The account was removed while the operation ran.
    #[error("cancelled")]
    Cancelled,

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("could not determine a data directory; pass one explicitly")]
    NoDataDir,

    #[error("another cat-mail instance is already using {}", .0.display())]
    AlreadyRunning(std::path::PathBuf),

    /// Whoever owns the data directory could read or replace what is stored in it.
    #[error("{} belongs to another user; choose a data directory of your own", .0.display())]
    DataDirNotOwned(std::path::PathBuf),

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

impl From<catmail_proto::smtp::Error> for Error {
    /// Lifts the failures a user can act on, as for IMAP.
    fn from(e: catmail_proto::smtp::Error) -> Self {
        use catmail_proto::smtp::Error as E;
        match e {
            E::AuthFailed(text) => Self::AuthFailed(text),
            E::Unsupported(what) => Self::Unsupported(what),
            other => Self::Smtp(other),
        }
    }
}

impl From<catmail_store::Error> for Error {
    fn from(e: catmail_store::Error) -> Self {
        match e {
            catmail_store::Error::Conflict(text) => Self::Conflict(text),
            catmail_store::Error::MessageNotFound(id) => Self::MessageNotFound(id),
            catmail_store::Error::Invalid(text) => Self::InvalidInput(text),
            other => Self::Store(other),
        }
    }
}
