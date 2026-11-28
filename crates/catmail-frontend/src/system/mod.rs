//! The app on the system, around its windows: one app per data directory ([`instance`]),
//! whether it opens a window and what it does once the last one is gone ([`resident`]),
//! the tray icon ([`tray`]), opening at login ([`autostart`]), giving the memory the
//! windows used back ([`memory`]), and updating the Flatpak ([`update`]).

pub mod autostart;
pub mod instance;
pub mod memory;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod portal_bus;
pub mod resident;
#[cfg(all(test, any(target_os = "linux", target_os = "freebsd")))]
mod test_bus;
pub mod tray;
pub mod update;
