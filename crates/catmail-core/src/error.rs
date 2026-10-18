pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] catmail_store::Error),

    #[error("credential store error: {0}")]
    Secrets(String),

    #[error("account {0} not found")]
    AccountNotFound(String),

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
