//! What catmail's desktop app does that no UI toolkit needs: the settings file, the
//! themes (derived from terminal palettes, imported, kept in a folder), one app per data
//! directory, the sidebar's folders and when the mailbox reads them again, the list's
//! rows, the commands on messages, when a composer saves, sends and closes, what the
//! outbox pane offers, the login forms, what Settings › Accounts asks, how message
//! metadata reads, what the reader says of a message, which parts of a message's HTML it
//! shows and the page it shows a body as, the tray icon, the new-mail notifications, and running with no window.
//! `catmail-gui` draws it; kept apart from it so that it is tested without GTK, WebKit
//! or nsh, and holds no type of theirs.
//!
//! `unsafe_code` is denied (the workspace's lint), not forbidden: [`memory`] calls glibc's
//! `malloc_trim`.

pub mod accounts;
pub mod commands;
pub mod composer;
#[cfg(test)]
mod fixtures;
pub mod folders;
pub mod format;
pub mod html;
pub mod instance;
pub mod list;
pub mod login;
pub mod mailbox;
pub mod memory;
pub mod new_mail;
pub mod outbox;
pub mod reader;
pub mod refresh;
pub mod resident;
pub mod settings;
pub mod theme;
pub mod tray;

/// FNV-1a: stable across Rust versions, runs and platforms, unlike `DefaultHasher`.
fn fnv1a(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}
