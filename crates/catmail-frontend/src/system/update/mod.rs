//! Updates of the Flatpak, from catmail's own repository: the Flatpak portal's update
//! monitor (`org.freedesktop.portal.Flatpak`, flatpak 1.5) checks the repository on its
//! own schedule, installs the new version when asked, and the portal starts it once the
//! app has quit. What the app shows of it is an [`Update`]. Only in the Flatpak: on macOS
//! Sparkle updates the app (catmail-gui's AppKit shell), and anywhere else there's nothing
//! here to update. See docs/architecture/background.md.

use std::fmt;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[path = "linux.rs"]
mod platform;

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
mod platform {
    use super::{Error, Update};

    pub struct Updates;

    impl Updates {
        pub fn install(&self) {}
    }

    pub fn watch(_said: impl Fn(Option<Update>) + Send + 'static) -> Option<Updates> {
        None
    }

    pub fn restart() -> Result<(), Error> {
        Err(Error("There's no newer version to start here.".into()))
    }
}

// What the portal's monitor says, made into an `Update`. Only the Flatpak's portal uses
// it; it's built everywhere for its tests, which need no portal.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd")),
    allow(
        dead_code,
        reason = "only the Flatpak's portal reports updates; tested everywhere"
    )
)]
mod tracker;

pub use self::platform::{Updates, restart, watch};

/// A newer version of the app than the one running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    /// In the repository, not installed.
    Available,
    /// Being installed: how far, in percent, once the portal says.
    Installing(Option<u8>),
    /// Installed (by the app, or by the software center in the background): the app runs
    /// it once started again.
    Installed,
    /// Installing it failed.
    Failed(Failure),
}

impl Update {
    /// The button the main window shows for it: none for a failure that trying again
    /// won't mend.
    pub fn button(&self) -> Option<Button> {
        match self {
            Self::Available | Self::Failed(Failure::Other(_)) => Some(Button::Install),
            Self::Installing(percent) => Some(Button::Installing(*percent)),
            Self::Installed => Some(Button::Restart),
            Self::Failed(Failure::Elsewhere | Failure::NotAllowed) => None,
        }
    }

    /// The notification's title and body while the main window isn't open; `None` takes
    /// it back: being installed (from the window, which shows how it goes) or failed.
    pub fn notice(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Available => Some((
                "Update available",
                "A new version of catmail is ready to install.",
            )),
            Self::Installed => Some((
                "Update installed",
                "Restart catmail to use the new version.",
            )),
            Self::Installing(_) | Self::Failed(_) => None,
        }
    }
}

/// What the main window's update button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Install,
    /// Nothing, while it shows how far installing is.
    Installing(Option<u8>),
    Restart,
}

impl Button {
    pub fn label(self) -> String {
        match self {
            Self::Install => "Install update".to_owned(),
            Self::Installing(Some(percent)) => format!("Updating {percent}%"),
            Self::Installing(None) => "Updating".to_owned(),
            Self::Restart => "Restart to update".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The portal can't install it: the new version asks for more permissions than the
    /// one installed (the user has to grant them), or it can't ask the user (the desktop
    /// has no Access portal it finds, or that one fails). The software center or
    /// `flatpak update` can.
    Elsewhere,
    /// The user didn't let the app update itself, in the dialog the portal showed: the
    /// portal keeps that answer.
    NotAllowed,
    /// Anything else, in the portal's words.
    Other(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Elsewhere => f.write_str(
                "catmail can't install this update itself. Update it in your software center \
                 or with flatpak update.",
            ),
            Self::NotAllowed => f.write_str(
                "catmail isn't allowed to update itself. Update it in your software center or \
                 with flatpak update.",
            ),
            Self::Other(why) => write!(f, "Couldn't install the update: {why}"),
        }
    }
}

/// Why the newer version couldn't be started.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_is_there_while_the_app_can_do_something() {
        assert_eq!(Update::Available.button(), Some(Button::Install));
        let retry = Update::Failed(Failure::Other("no network".into()));
        assert_eq!(retry.button(), Some(Button::Install));
        assert_eq!(Update::Failed(Failure::Elsewhere).button(), None);
        assert_eq!(Update::Failed(Failure::NotAllowed).button(), None);
        assert_eq!(Update::Installed.button(), Some(Button::Restart));
        let installing = Update::Installing(Some(40)).button().unwrap();
        assert_eq!(installing.label(), "Updating 40%");
        assert_eq!(Button::Installing(None).label(), "Updating");
    }

    #[test]
    fn only_an_update_to_act_on_is_told_of() {
        assert!(Update::Available.notice().is_some());
        assert!(Update::Installed.notice().is_some());
        assert_eq!(Update::Installing(None).notice(), None);
        assert_eq!(Update::Failed(Failure::Elsewhere).notice(), None);
    }
}
