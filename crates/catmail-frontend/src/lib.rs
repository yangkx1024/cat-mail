//! What catmail's desktop frontends share and no UI toolkit needs: the settings file, the
//! themes (derived from terminal palettes, imported, kept in a folder), one app per data
//! directory, the sidebar's folders and when the mailbox reads them again, the list's
//! rows, the commands on messages, when a composer saves, sends and closes, what the
//! outbox pane offers, the login forms, what Settings › Accounts asks, how message
//! metadata reads, what the reader says of a message, and which parts of a message's HTML
//! it shows.
//! `catmail-gui` (GPUI) and the nsh spike both use it.

#![forbid(unsafe_code)]

pub mod accounts;
pub mod commands;
pub mod composer;
pub mod folders;
pub mod format;
pub mod html;
pub mod instance;
pub mod list;
pub mod login;
pub mod mailbox;
pub mod outbox;
pub mod reader;
pub mod refresh;
pub mod settings;
pub mod theme;
