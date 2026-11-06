//! The tray icon's model: its menu, what its tool tip and badge say, when the unread mail
//! is counted and the icon drawn ([`TrayModel`]), the app's icon with the count drawn on
//! it (as an SVG, for the frontend to draw), and the item itself: on Linux the
//! StatusNotifierItem ([`linux`]), which is D-Bus and no toolkit's, on macOS the status
//! item ([`macos`]), which is AppKit's. See docs/architecture/background.md.

pub mod badged;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub mod linux;
#[cfg(target_os = "macos")]
#[allow(unsafe_code, reason = "AppKit's status item, menu and images")]
pub mod macos;
mod model;

use std::borrow::Cow;

pub use model::{Drawing, TrayModel};

/// The app's icon, light and dark (`assets/app-icon/`, 512 × 512): Linux's tray icon, in
/// the desktop's appearance.
pub const LIGHT_ICON: &str = include_str!("../../assets/app-icon/catmail-light.svg");
pub const DARK_ICON: &str = include_str!("../../assets/app-icon/catmail-dark.svg");

/// The menu bar's icon on macOS: a template, which the system tints to suit the menu bar.
pub const MENU_BAR_ICON: &str = include_str!("../../assets/app-icon/catmail-menu-bar.svg");

/// What the icon looks like, as an SVG, and the sizes it is drawn at (square): on macOS
/// the template, whatever the count and the appearance (the count is beside it); elsewhere
/// the app's icon in the desktop's appearance, with the count on it.
pub fn icon(dark: bool, unread: u32) -> (Cow<'static, str>, &'static [u32]) {
    if cfg!(target_os = "macos") {
        (Cow::Borrowed(MENU_BAR_ICON), &MENU_BAR_SIZES)
    } else {
        let icon = if dark { DARK_ICON } else { LIGHT_ICON };
        (badged::svg(icon, unread), &badged::SIZES)
    }
}

/// What the tray asks the app for. Clicking the icon is `Open` where that doesn't open
/// the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    Compose,
    Sync,
    Quit,
}

/// The menu, top to bottom; `None` is a separator.
pub const MENU: [Option<(&str, Command)>; 5] = [
    Some(("Open catmail", Command::Open)),
    Some(("New message", Command::Compose)),
    Some(("Sync now", Command::Sync)),
    None,
    Some(("Quit", Command::Quit)),
];

/// A command, with the token that lets a window take focus on Wayland if the host gave
/// one.
pub type Chosen = (Command, Option<String>);

/// The icon at one of its sizes, square: ARGB in network byte order, straight alpha, as
/// the StatusNotifierItem specification asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixmap {
    pub size: u32,
    pub argb: Vec<u8>,
}

impl Pixmap {
    /// From straight-alpha RGBA.
    pub fn from_rgba(size: u32, rgba: &[u8]) -> Self {
        let (pixels, _) = rgba.as_chunks::<4>();
        let argb = pixels
            .iter()
            .flat_map(|&[r, g, b, a]| [a, r, g, b])
            .collect();
        Self { size, argb }
    }
}

/// The menu bar's icon on macOS, 18 points high as the menu bar's icons are, for 1x and
/// 2x displays: a template, which the system tints, with the count as text beside it.
pub const MENU_BAR_SIZES: [u32; 2] = [18, 36];

/// The menu bar's icon at one of its sizes, square: RGBA with premultiplied alpha, as
/// AppKit's bitmaps hold it. Only its alpha counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub size: u32,
    pub rgba: Vec<u8>,
}

pub fn tool_tip(unread: u32) -> String {
    match unread {
        0 => "No unread mail".into(),
        1 => "1 unread message".into(),
        n => format!("{n} unread messages"),
    }
}

/// What the badge says, if there is one.
pub fn badge(unread: u32) -> Option<String> {
    match unread {
        0 => None,
        1..=99 => Some(unread.to_string()),
        _ => Some("99+".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_has_each_command_once() {
        let commands: Vec<_> = MENU.iter().flatten().map(|(_, command)| *command).collect();
        let all = [
            Command::Open,
            Command::Compose,
            Command::Sync,
            Command::Quit,
        ];
        assert_eq!(commands, all);
    }

    #[test]
    fn the_badge_counts_up_to_99() {
        assert_eq!(badge(0), None);
        assert_eq!(badge(7).as_deref(), Some("7"));
        assert_eq!(badge(99).as_deref(), Some("99"));
        assert_eq!(badge(100).as_deref(), Some("99+"));
    }

    #[test]
    fn pixels_come_out_as_argb() {
        assert_eq!(Pixmap::from_rgba(1, &[1, 2, 3, 0x80]).argb, [0x80, 1, 2, 3]);
    }
}
