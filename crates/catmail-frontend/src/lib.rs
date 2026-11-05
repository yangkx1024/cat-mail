//! What catmail's desktop frontends share and no UI toolkit needs: the settings file, one
//! app per data directory, the sidebar's folders and when the mailbox reads them again, the
//! list's rows, the commands on messages, how message metadata reads, what the reader says
//! of a message, and which parts of a message's HTML it shows.
//! `catmail-gui` (GPUI) and the nsh spike both use it.

#![forbid(unsafe_code)]

pub mod commands;
pub mod folders;
pub mod format;
pub mod html;
pub mod instance;
pub mod list;
pub mod mailbox;
pub mod reader;
pub mod refresh;
pub mod settings;
pub mod theme;
