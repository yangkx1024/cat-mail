//! What this build is, for `--version` and the log (`build.rs` finds it out).

/// The version and the commit it was built from, as `--version` shows it.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("CATMAIL_COMMIT"), ")");
/// Short, with `-dirty` if built with changes; `unknown` if the build couldn't tell.
pub(crate) const COMMIT: &str = env!("CATMAIL_COMMIT");
