//! Embedded icons: GPUI Kit's default set, which its components use, plus the Lucide
//! icons this app adds. Only the listed icons are embedded, not the whole catalog.

use std::borrow::Cow;

use gpui_kit::{AssetSource, SharedString};

gpui_kit::assets::icon_assets!(
    MailIcons,
    [
        Archive,
        FilePen,
        Flag,
        ImageOff,
        Mails,
        Paperclip,
        Send,
        ShieldAlert,
        Trash
    ]
);

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        match MailIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(MailIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::assets::IconName;

    use super::*;

    /// Every icon the views name must be embedded; a missing one renders as nothing and
    /// only logs an error.
    #[test]
    fn icons_used_by_the_views_are_embedded() {
        let used = [
            IconName::Archive,
            IconName::ExternalLink,
            IconName::FilePen,
            IconName::Flag,
            IconName::Folder,
            IconName::ImageOff,
            IconName::Inbox,
            IconName::Mails,
            IconName::Paperclip,
            IconName::Plus,
            IconName::RefreshCw,
            IconName::Send,
            IconName::ShieldAlert,
            IconName::Trash,
        ];
        for icon in used {
            let path = icon.path();
            assert!(
                Assets.load(&path).unwrap().is_some(),
                "{path} is not embedded"
            );
        }
    }
}
