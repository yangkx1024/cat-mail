//! The icons, the GUI's Heroicons (`catmail-gui/assets/heroicons`, MIT): outline ones,
//! drawn with `currentColor`, so they take the text color of where they are.

use catmail_frontend::folders::FolderIcon;
use nsh_core::element::ImageSrc;
use nsh_core::prelude::*;

macro_rules! hero {
    ($file:literal) => {
        include_str!(concat!(
            "../../catmail-gui/assets/heroicons/outline/",
            $file,
            ".svg"
        ))
    };
}

/// The other icons the app draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Folder(FolderIcon),
    ChevronDown,
    ChevronRight,
    ExclamationTriangle,
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
            Icon::ChevronDown => hero!("chevron-down"),
            Icon::ChevronRight => hero!("chevron-right"),
            Icon::ExclamationTriangle => hero!("exclamation-triangle"),
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
