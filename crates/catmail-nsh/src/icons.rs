//! The icons, the GUI's Heroicons (`catmail-gui/assets/heroicons`, MIT), drawn with
//! `currentColor`, so they take the text color of where they are.

use catmail_frontend::folders::FolderIcon;
use nsh_core::element::ImageSrc;
use nsh_core::prelude::*;

macro_rules! hero {
    ($file:literal) => {
        hero!("outline", $file)
    };
    ($style:literal, $file:literal) => {
        include_str!(concat!(
            "../../catmail-gui/assets/heroicons/",
            $style,
            "/",
            $file,
            ".svg"
        ))
    };
}

/// The other icons the app draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Folder(FolderIcon),
    ArchiveBox,
    ArrowLeft,
    ArrowTopRightOnSquare,
    ArrowUturnLeft,
    ArrowUturnLeftDouble,
    ArrowUturnRight,
    Check,
    ChevronDown,
    ChevronRight,
    Document,
    Envelope,
    EnvelopeOpen,
    ExclamationTriangle,
    Flag,
    /// Filled, as a mark on a flagged message.
    FlagSolid,
    FolderArrowRight,
    InformationCircle,
    MagnifyingGlass,
    PaperAirplane,
    PaperClip,
    PencilSquare,
    Photo,
    Tag,
    Trash,
    XCircle,
    XMark,
}

impl From<FolderIcon> for Icon {
    fn from(icon: FolderIcon) -> Self {
        Icon::Folder(icon)
    }
}

impl Icon {
    fn markup(self) -> &'static str {
        match self {
            Icon::Folder(icon) => match icon {
                FolderIcon::Inbox => hero!("inbox"),
                FolderIcon::Sent => hero!("paper-airplane"),
                FolderIcon::Archive => hero!("archive-box"),
                FolderIcon::All => hero!("envelope"),
                FolderIcon::Important => hero!("star"),
                FolderIcon::Flagged => hero!("flag"),
                FolderIcon::Junk => hero!("shield-exclamation"),
                FolderIcon::Trash => hero!("trash"),
                FolderIcon::Drafts => hero!("document-text"),
                FolderIcon::Label => hero!("tag"),
                FolderIcon::Folder => hero!("folder"),
                FolderIcon::Inboxes => hero!("inbox-stack"),
                FolderIcon::Outbox => hero!("arrow-up-tray"),
            },
            Icon::ArchiveBox => hero!("archive-box"),
            Icon::ArrowLeft => hero!("arrow-left"),
            Icon::ArrowTopRightOnSquare => hero!("arrow-top-right-on-square"),
            Icon::ArrowUturnLeft => hero!("arrow-uturn-left"),
            Icon::ArrowUturnLeftDouble => hero!("arrow-uturn-left-double"),
            Icon::ArrowUturnRight => hero!("arrow-uturn-right"),
            Icon::Check => hero!("check"),
            Icon::ChevronDown => hero!("chevron-down"),
            Icon::ChevronRight => hero!("chevron-right"),
            Icon::Document => hero!("document"),
            Icon::Envelope => hero!("envelope"),
            Icon::EnvelopeOpen => hero!("envelope-open"),
            Icon::ExclamationTriangle => hero!("exclamation-triangle"),
            Icon::Flag => hero!("flag"),
            Icon::FlagSolid => hero!("solid", "flag"),
            Icon::FolderArrowRight => hero!("folder-arrow-right"),
            Icon::InformationCircle => hero!("information-circle"),
            Icon::MagnifyingGlass => hero!("magnifying-glass"),
            Icon::PaperAirplane => hero!("paper-airplane"),
            Icon::PaperClip => hero!("paper-clip"),
            Icon::PencilSquare => hero!("pencil-square"),
            Icon::Photo => hero!("photo"),
            Icon::Tag => hero!("tag"),
            Icon::Trash => hero!("trash"),
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
