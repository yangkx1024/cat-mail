//! What the app does once its last window is gone, and why it quits: the decisions of
//! running without a window ("Keep running after the window is closed"), which every
//! desktop frontend makes the same way. See docs/architecture/background.md.

use std::fmt;

/// Why the app quits. Said in the log, where an app that was killed or crashed just stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitReason {
    /// The Quit action (Ctrl Q).
    Shortcut,
    Tray,
    /// SIGINT, SIGTERM or SIGHUP; on Windows, a console event.
    Signal,
    LastWindowClosed,
    /// The data directory couldn't be opened, and no window is left to say so.
    EngineFailed,
    /// The main window couldn't be opened at launch.
    NoWindow,
    /// Not by the app: the platform's own Quit (macOS's app menu or Dock).
    Platform,
}

impl fmt::Display for QuitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Shortcut => "shortcut",
            Self::Tray => "tray",
            Self::Signal => "signal",
            Self::LastWindowClosed => "last_window_closed",
            Self::EngineFailed => "engine_failed",
            Self::NoWindow => "no_window",
            Self::Platform => "platform",
        })
    }
}

/// What the app does once its last window has closed.
#[derive(Debug, PartialEq, Eq)]
pub enum AfterLastWindow {
    Quit,
    /// Keep running without a window: the engine syncs, sends and tells about new mail,
    /// and the tray or another launch brings the window back.
    KeepRunning,
}

/// With `keep_running` on, the app stays, unless it is `quitting` or the engine has
/// nothing to run: it couldn't be opened, or has been shut down (`engine_usable` false;
/// one still opening is usable).
pub fn after_last_window(
    keep_running: bool,
    engine_usable: bool,
    quitting: bool,
) -> AfterLastWindow {
    if quitting || !keep_running || !engine_usable {
        AfterLastWindow::Quit
    } else {
        AfterLastWindow::KeepRunning
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_last_window_keeps_the_engine_running_if_asked() {
        assert_eq!(
            after_last_window(true, true, false),
            AfterLastWindow::KeepRunning
        );
        assert_eq!(after_last_window(false, true, false), AfterLastWindow::Quit);
    }

    #[test]
    fn quitting_or_a_failed_engine_ends_the_app_with_its_last_window() {
        assert_eq!(after_last_window(true, true, true), AfterLastWindow::Quit);
        assert_eq!(after_last_window(true, false, false), AfterLastWindow::Quit);
    }
}
