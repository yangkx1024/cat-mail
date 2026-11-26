//! The tray icon on macOS: the menu bar's status item, a template the system tints to
//! suit the menu bar, with the count beside it.

use catmail_frontend::tray::{Chosen, Template, macos};
use futures_channel::mpsc::UnboundedSender;
use resvg::tiny_skia;

#[derive(Clone)]
pub struct Item(macos::Item);

impl Item {
    pub async fn spawn(chosen: UnboundedSender<Chosen>) -> Option<Self> {
        macos::Item::spawn(chosen).map(Self)
    }

    pub async fn show(&self, icon: Option<Vec<Template>>, unread: u32) {
        self.0.show(icon.as_deref(), unread);
    }
}

/// resvg's pixels are premultiplied, as AppKit's bitmaps hold them.
pub fn icon_from(drawn: tiny_skia::Pixmap) -> Template {
    Template {
        size: drawn.width(),
        rgba: drawn.take(),
    }
}

/// The system tints the template to suit the menu bar.
pub fn follow_appearance() {}

#[cfg(test)]
mod tests {
    use catmail_frontend::tray::{MENU_BAR_ICON, MENU_BAR_SIZES};

    /// macOS tints whatever isn't transparent.
    #[test]
    fn the_menu_bar_template_is_drawn() {
        let drawn = super::super::pixmaps(MENU_BAR_ICON, &MENU_BAR_SIZES);
        assert_eq!(drawn.len(), MENU_BAR_SIZES.len());
        for pixmap in drawn {
            let size = pixmap.width();
            let opaque = pixmap.pixels().iter().filter(|p| p.alpha() > 128).count();
            let all = (size * size) as usize;
            assert!(
                all / 5 < opaque && opaque < all / 2,
                "{size}: {opaque} of {all}"
            );
        }
    }
}
