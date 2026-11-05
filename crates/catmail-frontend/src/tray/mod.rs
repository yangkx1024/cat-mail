//! The tray icon's model, which every desktop frontend shows the same way: its menu, what
//! its tool tip and badge say, the app's icon with the count drawn on it (as an SVG, for
//! the frontend to draw), and on Linux the StatusNotifierItem itself ([`linux`]), which is
//! D-Bus and no toolkit's. See docs/architecture/background.md.

pub mod badged;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub mod linux;

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
    /// From straight-alpha BGRA.
    pub fn from_bgra(size: u32, bgra: &[u8]) -> Self {
        let (pixels, _) = bgra.as_chunks::<4>();
        let argb = pixels
            .iter()
            .flat_map(|&[b, g, r, a]| [a, r, g, b])
            .collect();
        Self { size, argb }
    }

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
        let argb = [0x80, 1, 2, 3];
        assert_eq!(Pixmap::from_bgra(1, &[3, 2, 1, 0x80]).argb, argb);
        assert_eq!(Pixmap::from_rgba(1, &[1, 2, 3, 0x80]).argb, argb);
    }
}
