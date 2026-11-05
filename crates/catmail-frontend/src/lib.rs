//! What catmail's desktop frontends share and no UI toolkit needs: the settings file, one
//! app per data directory, how message metadata reads, and which parts of a message's HTML
//! the reader shows. `catmail-gui` (GPUI) and the nsh spike both use it.

#![forbid(unsafe_code)]

pub mod format;
pub mod html;
pub mod instance;
pub mod settings;
pub mod theme;
