//! The parts of the app outside its windows, there with or without one, which follow the
//! engine from its opening: the tray icon, the notifications of new mail, and the system
//! saying the computer woke. `main.rs` hands them to the shell ([`crate::shell::Parts`]).

pub(crate) mod new_mail;
pub(crate) mod tray;
pub(crate) mod wake;
