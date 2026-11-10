pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("database migration failed: {0}")]
    Migration(String),

    /// The database was migrated by a newer catmail, whose schema this one doesn't know.
    #[error("the database is from a newer catmail (schema {found}, this one knows {known})")]
    SchemaTooNew { found: u32, known: u32 },

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0} already exists")]
    AlreadyExists(String),

    #[error("message {0} not found")]
    MessageNotFound(catmail_types::MessageId),

    /// The request makes no sense as given, e.g. moving a message to another account's
    /// folder. The text says why.
    #[error("{0}")]
    Invalid(String),

    /// The row is in a state that doesn't allow the change, e.g. editing a draft that is
    /// being sent. The text says why.
    #[error("{0}")]
    Conflict(String),
}

pub(crate) fn is_unique_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}
