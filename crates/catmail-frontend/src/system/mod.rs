//! The app on the system, around its windows: one app per data directory ([`instance`]),
//! whether it opens a window and what it does once the last one is gone ([`resident`]),
//! the tray icon ([`tray`]), opening at login ([`autostart`]), and giving the memory the
//! windows used back ([`memory`]).

pub mod autostart;
pub mod instance;
pub mod memory;
pub mod resident;
pub mod tray;
