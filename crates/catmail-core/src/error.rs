use catmail_proto::{http, imap, smtp};
use catmail_types::{ErrorKind, ReportedError};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A database failure.
    #[error(transparent)]
    Store(StoreError),

    /// Any other IMAP or network failure.
    #[error(transparent)]
    Imap(ImapError),

    /// Any other SMTP failure.
    #[error(transparent)]
    Smtp(SmtpError),

    /// A remote image couldn't be had.
    #[error(transparent)]
    Http(HttpError),

    /// Checking an account's or identity's settings, its SMTP server failed so.
    #[error("outgoing mail server (SMTP): {0}")]
    OutgoingServer(Box<Error>),

    /// Attaching a file to a new draft failed so; no draft was made.
    #[error("failed to attach {}: {error}", path.display())]
    Attach {
        path: std::path::PathBuf,
        error: Box<Error>,
    },

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

    #[error("identity {0} not found")]
    IdentityNotFound(catmail_types::IdentityId),

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

    /// Something with the same identity is stored already, e.g. an account with that
    /// address.
    #[error("{0} already exists")]
    AlreadyExists(String),

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

    /// Asked of background sync, which this client doesn't run
    /// ([`MailClientBuilder::sync_in_background`](crate::MailClientBuilder::sync_in_background)).
    #[error("accounts aren't kept in sync in the background; sync each one instead")]
    NoBackgroundSync,

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

/// A database failure. Its text says what happened; there is nothing to act on but
/// reporting it.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct StoreError(catmail_store::Error);

/// An IMAP failure without a variant of its own. Its text says what happened, and
/// [`Error::kind`] what to make of it.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct ImapError(pub(crate) imap::Error);

/// An SMTP failure without a variant of its own, as for [`ImapError`].
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct SmtpError(pub(crate) smtp::Error);

/// A failure to fetch a remote image, as for [`ImapError`].
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct HttpError(pub(crate) http::Error);

impl Error {
    pub fn kind(&self) -> ErrorKind {
        use ErrorKind as K;
        match self {
            Self::Store(_) => K::Internal,
            Self::Imap(ImapError(e)) => e.kind(),
            Self::Smtp(SmtpError(e)) => e.kind(),
            Self::Http(HttpError(e)) => e.kind(),
            Self::OutgoingServer(e) | Self::Attach { error: e, .. } => e.kind(),
            Self::AuthFailed(_) => K::Auth,
            Self::Unsupported(_)
            | Self::InvalidInput(_)
            | Self::NoDataDir
            | Self::NoBackgroundSync
            | Self::DataDirNotOwned(_) => K::Invalid,
            Self::AccountNotFound(_)
            | Self::IdentityNotFound(_)
            | Self::FolderNotFound(_)
            | Self::MessageNotFound(_)
            | Self::AttachmentNotFound(_)
            | Self::DraftNotFound(_)
            | Self::DraftAttachmentNotFound(_)
            | Self::OutboxNotFound(_)
            | Self::Cancelled => K::NotFound,
            Self::AlreadyExists(_) | Self::Conflict(_) | Self::AlreadyRunning(_) => K::Conflict,
            Self::OutOfSync(_) => K::OutOfSync,
            Self::Tls(_) | Self::Secrets(_) | Self::Task(_) | Self::Io(_) => K::Internal,
        }
    }

    /// Whether trying the same again later may work, with nothing changed meanwhile: the
    /// network failed, or the server refused for now (an SMTP 4xx reply).
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Smtp(SmtpError(e)) => !e.is_permanent(),
            Self::OutgoingServer(e) => e.is_transient(),
            _ => self.kind() == ErrorKind::Network,
        }
    }

    /// Whether a command failed but the connection to the server is still usable, so
    /// whatever else was to be done on it can go on: the server refused the command, or
    /// its answer couldn't be read completely, or the input was wrong for it.
    pub fn is_refusal(&self) -> bool {
        match self {
            Self::Imap(ImapError(e)) => !e.is_fatal(),
            Self::Smtp(SmtpError(e)) => !e.is_fatal(),
            _ => false,
        }
    }
}

impl From<&Error> for ReportedError {
    fn from(e: &Error) -> Self {
        Self {
            kind: e.kind(),
            message: e.to_string(),
            transient: e.is_transient(),
        }
    }
}

impl From<imap::Error> for Error {
    /// Lifts the failures a user can act on, so frontends needn't interpret protocol
    /// errors.
    fn from(e: imap::Error) -> Self {
        use imap::Error as E;
        match e {
            E::AuthFailed(text) => Self::AuthFailed(text),
            E::Unsupported(what) => Self::Unsupported(what),
            other => Self::Imap(ImapError(other)),
        }
    }
}

impl From<smtp::Error> for Error {
    /// Lifts the failures a user can act on, as for IMAP.
    fn from(e: smtp::Error) -> Self {
        use smtp::Error as E;
        match e {
            E::AuthFailed(text) => Self::AuthFailed(text),
            E::Unsupported(what) => Self::Unsupported(what),
            other => Self::Smtp(SmtpError(other)),
        }
    }
}

impl From<http::Error> for Error {
    fn from(e: http::Error) -> Self {
        Self::Http(HttpError(e))
    }
}

impl From<catmail_store::Error> for Error {
    fn from(e: catmail_store::Error) -> Self {
        use catmail_store::Error as E;
        match e {
            E::AlreadyExists(what) => Self::AlreadyExists(what),
            E::Conflict(text) => Self::Conflict(text),
            E::MessageNotFound(id) => Self::MessageNotFound(id),
            E::Invalid(text) => Self::InvalidInput(text),
            other => Self::Store(StoreError(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtp_replies_are_transient_only_below_500() {
        let reply = |code| {
            Error::from(smtp::Error::Reply {
                command: "RCPT",
                code,
                text: String::new(),
            })
        };
        let (busy, refused) = (reply(451), reply(550));
        assert_eq!(
            (busy.kind(), refused.kind()),
            (ErrorKind::Refused, ErrorKind::Refused)
        );
        assert!(busy.is_transient() && !refused.is_transient());
        assert!(busy.is_refusal() && refused.is_refusal());
    }

    #[test]
    fn a_lost_connection_is_transient_but_no_refusal() {
        let closed = Error::from(imap::Error::Closed);
        assert_eq!(closed.kind(), ErrorKind::Network);
        assert!(closed.is_transient() && !closed.is_refusal());

        let refused = Error::from(imap::Error::Command {
            command: "SELECT",
            text: "NO no such mailbox".into(),
        });
        assert_eq!(refused.kind(), ErrorKind::Refused);
        assert!(refused.is_refusal() && !refused.is_transient());
    }

    #[test]
    fn user_facing_failures_are_lifted() {
        let rejected = Error::from(imap::Error::AuthFailed("NO".into()));
        assert!(matches!(rejected, Error::AuthFailed(_)));
        assert_eq!(rejected.kind(), ErrorKind::Auth);

        let dup = Error::from(catmail_store::Error::AlreadyExists("account a".into()));
        assert!(matches!(dup, Error::AlreadyExists(_)));
        assert_eq!(dup.kind(), ErrorKind::Conflict);
    }
}
