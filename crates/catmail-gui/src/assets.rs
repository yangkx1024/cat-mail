//! Embedded icons. The app draws [Heroicons](https://heroicons.com) (MIT, vendored under
//! `assets/heroicons/`, only the ones used), and serves them in place of GPUI Kit's own
//! Lucide icons wherever its components draw one we have a counterpart for (check marks,
//! chevrons, close buttons, …), so the window uses one icon family. Component icons
//! without a counterpart (the spinner, panel toggles) still come from GPUI Kit.

use std::borrow::Cow;

use gpui_kit::assets::IconNamed;
use gpui_kit::{AssetSource, SharedString};

macro_rules! heroicons {
    ($($name:ident => $style:literal / $file:literal,)*) => {
        /// The Heroicons this app embeds. `Icon::new(Hero::Inbox)` draws one.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Hero {
            $($name,)*
        }

        impl Hero {
            const ALL: &[Hero] = &[$(Hero::$name,)*];

            fn asset_path(self) -> &'static str {
                match self {
                    $(Hero::$name => concat!("icons/hero/", $style, "/", $file, ".svg"),)*
                }
            }

            fn svg(path: &str) -> Option<&'static [u8]> {
                match path {
                    $(concat!("icons/hero/", $style, "/", $file, ".svg") => Some(include_bytes!(
                        concat!("../assets/heroicons/", $style, "/", $file, ".svg")
                    )),)*
                    _ => None,
                }
            }
        }
    };
}

heroicons! {
    ArchiveBox => "outline" / "archive-box",
    ArrowDown => "outline" / "arrow-down",
    ArrowDownTray => "outline" / "arrow-down-tray",
    ArrowPath => "outline" / "arrow-path",
    ArrowTopRightOnSquare => "outline" / "arrow-top-right-on-square",
    ArrowUp => "outline" / "arrow-up",
    ArrowUpTray => "outline" / "arrow-up-tray",
    ArrowUturnLeft => "outline" / "arrow-uturn-left",
    ArrowUturnRight => "outline" / "arrow-uturn-right",
    ArrowsPointingIn => "outline" / "arrows-pointing-in",
    ArrowsPointingOut => "outline" / "arrows-pointing-out",
    BarsArrowDown => "outline" / "bars-arrow-down",
    BarsArrowUp => "outline" / "bars-arrow-up",
    BookOpen => "outline" / "book-open",
    Calendar => "outline" / "calendar",
    Check => "outline" / "check",
    CheckCircle => "outline" / "check-circle",
    ChevronDown => "outline" / "chevron-down",
    ChevronLeft => "outline" / "chevron-left",
    ChevronRight => "outline" / "chevron-right",
    ChevronUp => "outline" / "chevron-up",
    ChevronUpDown => "outline" / "chevron-up-down",
    Clock => "outline" / "clock",
    Cog6Tooth => "outline" / "cog-6-tooth",
    ComputerDesktop => "outline" / "computer-desktop",
    Document => "outline" / "document",
    DocumentDuplicate => "outline" / "document-duplicate",
    DocumentText => "outline" / "document-text",
    EllipsisHorizontal => "outline" / "ellipsis-horizontal",
    Envelope => "outline" / "envelope",
    EnvelopeOpen => "outline" / "envelope-open",
    ExclamationTriangle => "outline" / "exclamation-triangle",
    Eye => "outline" / "eye",
    EyeSlash => "outline" / "eye-slash",
    Flag => "outline" / "flag",
    FlagSolid => "solid" / "flag",
    Folder => "outline" / "folder",
    FolderArrowDown => "outline" / "folder-arrow-down",
    Inbox => "outline" / "inbox",
    InboxStack => "outline" / "inbox-stack",
    InformationCircle => "outline" / "information-circle",
    Key => "outline" / "key",
    MagnifyingGlass => "outline" / "magnifying-glass",
    Microphone => "outline" / "microphone",
    Minus => "outline" / "minus",
    Moon => "outline" / "moon",
    NoSymbol => "outline" / "no-symbol",
    PaperAirplane => "outline" / "paper-airplane",
    PaperClip => "outline" / "paper-clip",
    PencilSquare => "outline" / "pencil-square",
    Photo => "outline" / "photo",
    Plus => "outline" / "plus",
    ShieldExclamation => "outline" / "shield-exclamation",
    SignalSlash => "outline" / "signal-slash",
    Square2Stack => "outline" / "square-2-stack",
    Star => "outline" / "star",
    StarSolid => "solid" / "star",
    Stop => "outline" / "stop",
    Sun => "outline" / "sun",
    Swatch => "outline" / "swatch",
    Tag => "outline" / "tag",
    Trash => "outline" / "trash",
    User => "outline" / "user",
    UserCircle => "outline" / "user-circle",
    XCircle => "outline" / "x-circle",
    XMark => "outline" / "x-mark",
}

impl IconNamed for Hero {
    fn path(self) -> SharedString {
        self.asset_path().into()
    }
}

/// GPUI Kit's component icons, by asset path, and the Heroicon drawn instead.
const COMPONENT_ICONS: &[(&str, Hero)] = &[
    ("icons/arrow-down.svg", Hero::ArrowDown),
    ("icons/arrow-up.svg", Hero::ArrowUp),
    ("icons/ban.svg", Hero::NoSymbol),
    ("icons/book-open.svg", Hero::BookOpen),
    ("icons/calendar.svg", Hero::Calendar),
    ("icons/check.svg", Hero::Check),
    ("icons/chevron-down.svg", Hero::ChevronDown),
    ("icons/chevron-left.svg", Hero::ChevronLeft),
    ("icons/chevron-right.svg", Hero::ChevronRight),
    ("icons/chevron-up.svg", Hero::ChevronUp),
    ("icons/chevrons-up-down.svg", Hero::ChevronUpDown),
    ("icons/circle-check.svg", Hero::CheckCircle),
    ("icons/circle-x.svg", Hero::XCircle),
    ("icons/close.svg", Hero::XMark),
    ("icons/copy.svg", Hero::DocumentDuplicate),
    ("icons/ellipsis.svg", Hero::EllipsisHorizontal),
    ("icons/external-link.svg", Hero::ArrowTopRightOnSquare),
    ("icons/eye-off.svg", Hero::EyeSlash),
    ("icons/eye.svg", Hero::Eye),
    ("icons/inbox.svg", Hero::Inbox),
    ("icons/info.svg", Hero::InformationCircle),
    ("icons/maximize.svg", Hero::ArrowsPointingOut),
    ("icons/mic.svg", Hero::Microphone),
    ("icons/minimize.svg", Hero::ArrowsPointingIn),
    ("icons/minus.svg", Hero::Minus),
    ("icons/plus.svg", Hero::Plus),
    ("icons/refresh-cw.svg", Hero::ArrowPath),
    ("icons/search.svg", Hero::MagnifyingGlass),
    ("icons/sort-ascending.svg", Hero::BarsArrowUp),
    ("icons/sort-descending.svg", Hero::BarsArrowDown),
    ("icons/square.svg", Hero::Stop),
    ("icons/star-fill.svg", Hero::StarSolid),
    ("icons/star.svg", Hero::Star),
    ("icons/triangle-alert.svg", Hero::ExclamationTriangle),
    ("icons/user.svg", Hero::User),
    ("icons/window-close.svg", Hero::XMark),
    ("icons/window-maximize.svg", Hero::Stop),
    ("icons/window-minimize.svg", Hero::Minus),
    ("icons/window-restore.svg", Hero::Square2Stack),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        let hero = COMPONENT_ICONS
            .iter()
            .find(|(component, _)| *component == path)
            .map_or(path, |(_, hero)| hero.asset_path());
        match Hero::svg(hero) {
            Some(bytes) => Ok(Some(Cow::Borrowed(bytes))),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(
            Hero::ALL
                .iter()
                .map(|icon| icon.asset_path())
                .filter(|p| p.starts_with(path))
                .map(SharedString::from),
        );
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::assets::IconName as Lucide;

    use super::*;

    #[test]
    fn every_heroicon_is_embedded() {
        for icon in Hero::ALL {
            let path = icon.path();
            assert!(Assets.load(&path).unwrap().is_some(), "{path} is missing");
        }
    }

    /// The components draw Heroicons where there is one, and their own icon otherwise.
    #[test]
    fn component_icons_are_replaced() {
        let check = Assets.load(&Lucide::Check.path()).unwrap().unwrap();
        assert_eq!(check, Hero::svg(Hero::Check.asset_path()).unwrap());
        for (component, _) in COMPONENT_ICONS {
            assert!(
                gpui_kit::assets::Assets.load(component).unwrap().is_some(),
                "{component} is not a GPUI Kit icon"
            );
        }
        let spinner = Lucide::LoaderCircle.path();
        assert_eq!(
            Assets.load(&spinner).unwrap(),
            gpui_kit::assets::Assets.load(&spinner).unwrap()
        );
    }
}
