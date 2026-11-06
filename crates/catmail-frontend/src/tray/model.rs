//! When the tray counts the unread mail and draws its icon. The frontend counts
//! (`MailClient::count_unread_in_inboxes`) and draws the SVG into pixels off the main
//! thread; this says when, and what.

use std::borrow::Cow;

use crate::refresh::Coalesce;

/// The count and the icon shown for it.
#[derive(Debug, Default)]
pub struct TrayModel {
    /// Unread messages in the INBOXes, once counted.
    unread: Option<u32>,
    /// The desktop's appearance, which the icon is drawn in where it isn't a template:
    /// the panel's, not the app's.
    dark: bool,
    counting: Coalesce,
    /// One drawing at a time, so an icon drawn for an older count can't land last.
    drawing: Coalesce,
    /// The SVG the item's icon was last drawn from: one that looks the same isn't drawn
    /// again (macOS's never changes).
    drawn: Option<Cow<'static, str>>,
}

/// What to show: the count, and the icon to draw for it unless it looks as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drawing {
    pub unread: u32,
    /// The SVG and the sizes to draw it at (square).
    pub icon: Option<(Cow<'static, str>, &'static [u32])>,
}

impl TrayModel {
    /// The unread mail may have changed: whether to count it now. One count at a time;
    /// asked for meanwhile, another follows ([`counted`](Self::counted)).
    pub fn count(&mut self) -> bool {
        self.counting.start()
    }

    /// The count is done: `unread`, or `None` if it failed. Returns whether to draw (the
    /// count changed) and whether to count again.
    pub fn counted(&mut self, unread: Option<u32>) -> (bool, bool) {
        let again = self.counting.finish();
        let changed = unread.is_some_and(|unread| self.unread.replace(unread) != Some(unread));
        (changed, again)
    }

    /// The desktop's appearance is `dark` now: whether to draw again.
    pub fn appearance(&mut self, dark: bool) -> bool {
        std::mem::replace(&mut self.dark, dark) != dark
    }

    /// What to show now: nothing before the first count, or while a drawing is on its
    /// way, which [`shown`](Self::shown) says to follow with this one.
    pub fn draw(&mut self) -> Option<Drawing> {
        let unread = self.unread?;
        if !self.drawing.start() {
            return None;
        }
        let (svg, sizes) = super::icon(self.dark, unread);
        let icon = (self.drawn.as_ref() != Some(&svg)).then_some((svg, sizes));
        Some(Drawing { unread, icon })
    }

    /// `drawing` is shown: whether to draw again, for what changed meanwhile.
    pub fn shown(&mut self, drawing: Drawing) -> bool {
        if let Some((svg, _)) = drawing.icon {
            self.drawn = Some(svg);
        }
        self.drawing.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_drawn_before_the_first_count() {
        let mut tray = TrayModel::default();
        assert_eq!(tray.draw(), None);
        assert!(tray.count());
        // Failed: nothing to show yet.
        assert_eq!(tray.counted(None), (false, false));
        assert_eq!(tray.draw(), None);
    }

    #[test]
    fn a_count_asked_for_meanwhile_follows() {
        let mut tray = TrayModel::default();
        assert!(tray.count());
        assert!(!tray.count());
        assert_eq!(tray.counted(Some(3)), (true, true));
        assert!(tray.count());
        // The same count: nothing to draw.
        assert_eq!(tray.counted(Some(3)), (false, false));
    }

    #[test]
    fn an_icon_for_an_older_count_cant_land_last() {
        let mut tray = TrayModel::default();
        tray.count();
        tray.counted(Some(3));
        let three = tray.draw().unwrap();
        assert_eq!(three.unread, 3);
        assert_eq!(three.icon, Some(super::super::icon(false, 3)));
        // Counted again while the icon for 3 is drawn: it waits for that one.
        tray.count();
        assert_eq!(tray.counted(Some(5)), (true, false));
        assert_eq!(tray.draw(), None);
        assert!(tray.shown(three));
        let five = tray.draw().unwrap();
        assert_eq!(five.unread, 5);
        assert!(!tray.shown(five));
    }

    #[test]
    fn an_icon_that_looks_the_same_isnt_drawn_again() {
        let mut tray = TrayModel::default();
        tray.count();
        tray.counted(Some(0));
        let first = tray.draw().unwrap();
        tray.shown(first);
        assert!(tray.appearance(true));
        assert!(!tray.appearance(true));
        let dark = tray.draw().unwrap();
        // macOS's template doesn't follow the appearance: only the count is shown.
        if cfg!(target_os = "macos") {
            assert_eq!(dark.icon, None);
        } else {
            assert_eq!(dark.icon, Some(super::super::icon(true, 0)));
        }
        tray.shown(dark);
        assert_eq!(tray.draw().unwrap().icon, None);
    }

    #[test]
    fn the_icon_is_the_apps_with_the_count_or_the_menu_bars_template() {
        let (light, sizes) = super::super::icon(false, 7);
        if cfg!(target_os = "macos") {
            assert_eq!(light, super::super::MENU_BAR_ICON);
            assert_eq!(sizes, super::super::MENU_BAR_SIZES);
        } else {
            assert!(light.contains(super::super::LIGHT_ICON) && light.contains(">7</text>"));
            assert!(super::super::icon(true, 0).0 == super::super::DARK_ICON);
            assert_eq!(sizes, super::super::badged::SIZES);
        }
    }
}
