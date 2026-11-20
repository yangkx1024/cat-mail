//! Opening the app when the user logs in ("Open at login"), and telling whether this
//! launch was that one. On macOS it is a login item (`SMAppService.mainApp`, macOS 13);
//! on Linux an XDG autostart entry, which the Background portal writes for the Flatpak.
//! The system owns it: the user can also turn it off in System Settings › General ›
//! Login Items, or in the desktop's autostart settings, so it is read from there each
//! time rather than kept in the settings file (where it can't be read, in the Flatpak,
//! the portal's last answer is kept: [`use_config_dir`]). Windows comes later. See
//! docs/architecture/background.md.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};

use thiserror::Error;

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "ServiceManagement and Foundation are Objective-C"
)]
#[path = "autostart/macos.rs"]
mod platform;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[path = "autostart/linux.rs"]
mod platform;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[path = "autostart/portal.rs"]
mod portal;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "freebsd")))]
mod platform {
    use super::{Error, State};

    pub fn available() -> bool {
        false
    }

    pub fn state() -> State {
        State::Off
    }

    pub fn set(_on: bool) -> (State, Result<(), Error>) {
        let why = Error("Opening at login isn't available here.".into());
        (State::Off, Err(why))
    }

    pub fn launched_at_login() -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    On,
    Off,
    /// Asked for, but turned off in System Settings: it opens at login once allowed there.
    NeedsApproval,
}

/// Why the system wouldn't change it, in its own words.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct Error(String);

/// The app's configuration folder (the settings file's), as the shell found it.
static CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Where the app keeps its configuration, told once at launch, before [`state`] or
/// [`set`]: what opening at login must remember itself goes there. Without it, the
/// Flatpak can't open at login.
pub fn use_config_dir(dir: PathBuf) {
    if CONFIG_DIR.set(dir).is_err() {
        tracing::warn!("the configuration folder was told twice; the first one stays");
    }
}

/// Whether the app can open at login here: on macOS 13 and later, from its bundle (a
/// binary run on its own, as by `cargo run`, has no login item); on Linux with a
/// configuration folder (in the Flatpak, the app's: [`use_config_dir`]).
pub fn available() -> bool {
    platform::available()
}

/// Whether the app opens at login now; `Off` where it can't. Waits for a change under
/// way.
pub fn state() -> State {
    one_at_a_time(platform::state)
}

/// Turns opening at login on or off: what it is afterwards, and whether the system
/// did as asked. One that [`State::NeedsApproval`] opens the system's list of login
/// items, where it is allowed. A blocking call to the system, which may also tell the
/// user it added a login item.
pub fn set(on: bool) -> (State, Result<(), Error>) {
    one_at_a_time(|| platform::set(on))
}

/// Runs `f` while no other change or reading of the login item runs. A change runs on a
/// thread of its own, which outlives the page that asked for it: one asked for from the
/// page opened again could otherwise run at the same time and finish first, an
/// unregister before the register it should follow.
fn one_at_a_time<R>(f: impl FnOnce() -> R) -> R {
    static SYSTEM: Mutex<()> = Mutex::new(());
    let _turn = SYSTEM.lock().unwrap_or_else(PoisonError::into_inner);
    f()
}

/// Whether this launch was the login's. On macOS, asked once the app has finished
/// launching (`applicationDidFinishLaunching:`), while its launch's Apple Event is the
/// current one. Never on Linux, where the login's launch says `--background`.
pub fn launched_at_login() -> bool {
    platform::launched_at_login()
}
