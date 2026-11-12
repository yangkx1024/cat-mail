//! The icons, Heroicons (`assets/heroicons`, MIT), drawn with
//! `currentColor`, so they take the text color of where they are.

use catmail_core::FolderRole;
use catmail_frontend::folders::FolderIcon;
use nsh_core::element::ImageSrc;
use nsh_core::prelude::*;

macro_rules! hero {
    ($file:literal) => {
        hero!("outline", $file)
    };
    ($style:literal, $file:literal) => {
        include_str!(concat!("../assets/heroicons/", $style, "/", $file, ".svg"))
    };
}

/// The other icons the app draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Folder(FolderIcon),
    /// Filled, as a selected row's ([`Icon::solid`]).
    FolderSolid(FolderIcon),
    ArchiveBox,
    ArrowDownTray,
    ArrowLeft,
    ArrowPath,
    ArrowTopRightOnSquare,
    ArrowUturnLeft,
    ArrowUturnLeftDouble,
    ArrowUturnRight,
    Bold,
    Check,
    ChevronDown,
    ChevronRight,
    ClipboardDocument,
    Cog6Tooth,
    Cog6ToothSolid,
    ComputerDesktop,
    Document,
    Envelope,
    EnvelopeOpen,
    EnvelopeOpenSolid,
    ExclamationTriangle,
    Flag,
    /// Filled, as a mark on a flagged message.
    FlagSolid,
    FolderArrowRight,
    InformationCircle,
    Italic,
    Key,
    ListBullet,
    MagnifyingGlass,
    Moon,
    NumberedList,
    PaperAirplane,
    PaperClip,
    PencilSquare,
    Photo,
    Plus,
    Sparkles,
    SparklesSolid,
    Star,
    /// Filled, as the default address's mark.
    StarSolid,
    Sun,
    Swatch,
    SwatchSolid,
    Tag,
    Trash,
    Underline,
    UserCircle,
    UserCircleSolid,
    XCircle,
    XMark,
}

/// A folder's icon in `style`: one table for the outlined and the filled.
macro_rules! folder_icon {
    ($icon:expr, $style:literal) => {
        match $icon {
            FolderIcon::Role(role) => match role {
                FolderRole::Inbox => hero!($style, "inbox"),
                FolderRole::Sent => hero!($style, "paper-airplane"),
                FolderRole::Archive => hero!($style, "archive-box"),
                FolderRole::All => hero!($style, "envelope"),
                FolderRole::Important => hero!($style, "star"),
                FolderRole::Flagged => hero!($style, "flag"),
                FolderRole::Junk => hero!($style, "shield-exclamation"),
                FolderRole::Trash => hero!($style, "trash"),
                FolderRole::Drafts => hero!($style, "document-text"),
            },
            FolderIcon::Label => hero!($style, "tag"),
            FolderIcon::Folder => hero!($style, "folder"),
            FolderIcon::Inboxes => hero!($style, "inbox-stack"),
            FolderIcon::Outbox => hero!($style, "arrow-up-tray"),
        }
    };
}

impl From<FolderIcon> for Icon {
    fn from(icon: FolderIcon) -> Self {
        Icon::Folder(icon)
    }
}

impl Icon {
    /// The filled icon, for a selected row;
    /// one with no filled form stays as it is.
    pub fn solid(self) -> Self {
        match self {
            Icon::Folder(icon) => Icon::FolderSolid(icon),
            Icon::Cog6Tooth => Icon::Cog6ToothSolid,
            Icon::EnvelopeOpen => Icon::EnvelopeOpenSolid,
            Icon::Flag => Icon::FlagSolid,
            Icon::Sparkles => Icon::SparklesSolid,
            Icon::Star => Icon::StarSolid,
            Icon::Swatch => Icon::SwatchSolid,
            Icon::UserCircle => Icon::UserCircleSolid,
            other => other,
        }
    }

    fn markup(self) -> &'static str {
        match self {
            Icon::Folder(icon) => folder_icon!(icon, "outline"),
            Icon::FolderSolid(icon) => folder_icon!(icon, "solid"),
            Icon::ArchiveBox => hero!("archive-box"),
            Icon::ArrowDownTray => hero!("arrow-down-tray"),
            Icon::ArrowLeft => hero!("arrow-left"),
            Icon::ArrowPath => hero!("arrow-path"),
            Icon::ArrowTopRightOnSquare => hero!("arrow-top-right-on-square"),
            Icon::ArrowUturnLeft => hero!("arrow-uturn-left"),
            Icon::ArrowUturnLeftDouble => hero!("arrow-uturn-left-double"),
            Icon::ArrowUturnRight => hero!("arrow-uturn-right"),
            Icon::Bold => hero!("bold"),
            Icon::Check => hero!("check"),
            Icon::ChevronDown => hero!("chevron-down"),
            Icon::ChevronRight => hero!("chevron-right"),
            Icon::ClipboardDocument => hero!("clipboard-document"),
            Icon::Cog6Tooth => hero!("cog-6-tooth"),
            Icon::Cog6ToothSolid => hero!("solid", "cog-6-tooth"),
            Icon::ComputerDesktop => hero!("computer-desktop"),
            Icon::Document => hero!("document"),
            Icon::Envelope => hero!("envelope"),
            Icon::EnvelopeOpen => hero!("envelope-open"),
            Icon::EnvelopeOpenSolid => hero!("solid", "envelope-open"),
            Icon::ExclamationTriangle => hero!("exclamation-triangle"),
            Icon::Flag => hero!("flag"),
            Icon::FlagSolid => hero!("solid", "flag"),
            Icon::FolderArrowRight => hero!("folder-arrow-right"),
            Icon::InformationCircle => hero!("information-circle"),
            Icon::Italic => hero!("italic"),
            Icon::Key => hero!("key"),
            Icon::ListBullet => hero!("list-bullet"),
            Icon::MagnifyingGlass => hero!("magnifying-glass"),
            Icon::Moon => hero!("moon"),
            Icon::NumberedList => hero!("numbered-list"),
            Icon::PaperAirplane => hero!("paper-airplane"),
            Icon::PaperClip => hero!("paper-clip"),
            Icon::PencilSquare => hero!("pencil-square"),
            Icon::Photo => hero!("photo"),
            Icon::Plus => hero!("plus"),
            Icon::Sparkles => hero!("sparkles"),
            Icon::SparklesSolid => hero!("solid", "sparkles"),
            Icon::Star => hero!("star"),
            Icon::StarSolid => hero!("solid", "star"),
            Icon::Sun => hero!("sun"),
            Icon::Swatch => hero!("swatch"),
            Icon::SwatchSolid => hero!("solid", "swatch"),
            Icon::Tag => hero!("tag"),
            Icon::Trash => hero!("trash"),
            Icon::Underline => hero!("underline"),
            Icon::UserCircle => hero!("user-circle"),
            Icon::UserCircleSolid => hero!("solid", "user-circle"),
            Icon::XCircle => hero!("x-circle"),
            Icon::XMark => hero!("x-mark"),
        }
    }

    pub fn src(self) -> ImageSrc {
        ImageSrc::svg_markup(self.markup())
    }
}

/// The icon `icon` gives, `size` points square.
pub fn icon(icon: impl Fn() -> Icon + 'static, size: f32) -> Svg {
    svg(move || icon().src()).w(px(size)).h(px(size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selected_row_has_its_icon_filled() {
        use FolderRole::*;
        let roles = [
            Inbox, Sent, Archive, All, Important, Flagged, Junk, Trash, Drafts,
        ];
        let others = [
            FolderIcon::Label,
            FolderIcon::Folder,
            FolderIcon::Inboxes,
            FolderIcon::Outbox,
        ];
        let folders = roles.map(FolderIcon::Role).into_iter().chain(others);
        let settings = [
            Icon::Cog6Tooth,
            Icon::Swatch,
            Icon::UserCircle,
            Icon::EnvelopeOpen,
        ];
        for icon in folders.map(Icon::from).chain(settings) {
            assert_ne!(icon.solid().markup(), icon.markup(), "{icon:?}");
        }
        assert_eq!(Icon::XMark.solid(), Icon::XMark);
    }
}
