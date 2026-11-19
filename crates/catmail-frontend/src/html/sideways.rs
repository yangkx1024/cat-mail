//! A page wider than its web view, scrolled sideways: where it is, and the thumb of the
//! bar the reader shows for it under the pane.
//!
//! The web view is as tall as its page and sits in the pane's scroll view, so the body's
//! own sideways scroller would be at the end of the message, seen only once it is
//! scrolled to. The page hides it ([`super::page`]) and says how it is scrolled
//! ([`FromPage::Sideways`](super::FromPage::Sideways)); the reader shows a bar of its
//! own for it at the bottom of the pane, as its scroll view shows its own at the side.

/// How a page is scrolled sideways, in points: as the body's `scrollLeft`,
/// `clientWidth` and `scrollWidth` say.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sideways {
    /// How far it is scrolled.
    pub offset: f32,
    /// How wide what shows of it is.
    pub view: f32,
    /// How wide it is.
    pub content: f32,
}

/// The thumb's shortest, as the scroll views' own indicators: short enough to see where
/// it is on a very wide page, long enough to see and to grab.
const MIN_THUMB: f32 = 32.0;

impl Sideways {
    /// How far it scrolls.
    pub fn range(self) -> f32 {
        (self.content - self.view).max(0.0)
    }

    /// How far it is scrolled, within its range: past the ends (WebKit's rubber band),
    /// at them.
    pub fn here(self) -> f32 {
        self.offset.clamp(0.0, self.range())
    }

    /// Whether there is anything to scroll to: more than a point, as a page laid out to
    /// a fraction of a pixel may be a pixel wider than the view.
    pub fn scrolls(self) -> bool {
        self.range() > 1.0
    }

    /// The thumb on a rail `rail` points long: where it starts and how long it is; none
    /// when the page fits.
    pub fn thumb(self, rail: f32) -> Option<(f32, f32)> {
        if !self.scrolls() || rail <= 0.0 {
            return None;
        }
        let len = (rail * self.view / self.content).clamp(MIN_THUMB.min(rail), rail);
        Some(((rail - len) * self.here() / self.range(), len))
    }

    /// The offset that moving the thumb `by` points from where it was at offset `from`
    /// gives: the thumb's travel spans the whole range.
    pub fn dragged(self, rail: f32, from: f32, by: f32) -> f32 {
        let Some((_, len)) = self.thumb(rail) else {
            return 0.0;
        };
        let travel = rail - len;
        if travel <= 0.0 {
            return from;
        }
        (from + by * self.range() / travel).clamp(0.0, self.range())
    }

    /// The offset that puts the thumb's middle at `at` on the rail: a press on the rail
    /// beside the thumb.
    pub fn pressed(self, rail: f32, at: f32) -> f32 {
        let Some((start, len)) = self.thumb(rail) else {
            return 0.0;
        };
        self.dragged(rail, self.here(), at - len / 2.0 - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(offset: f32) -> Sideways {
        Sideways {
            offset,
            view: 400.0,
            content: 1600.0,
        }
    }

    #[test]
    fn no_thumb_for_a_page_that_fits() {
        let fits = Sideways {
            offset: 0.0,
            view: 400.0,
            content: 400.5,
        };
        assert!(!fits.scrolls());
        assert_eq!(fits.thumb(400.0), None);
        assert_eq!(Sideways::default().thumb(400.0), None);
        assert_eq!(page(0.0).thumb(0.0), None);
    }

    #[test]
    fn the_thumb_is_as_long_as_what_shows_and_where_it_is() {
        assert_eq!(page(0.0).thumb(400.0), Some((0.0, 100.0)));
        assert_eq!(page(600.0).thumb(400.0), Some((150.0, 100.0)));
        assert_eq!(page(1200.0).thumb(400.0), Some((300.0, 100.0)));
        // Past the ends (WebKit's rubber band), it stays at them.
        assert_eq!(page(-30.0).thumb(400.0), Some((0.0, 100.0)));
        assert_eq!(page(1300.0).thumb(400.0), Some((300.0, 100.0)));
    }

    #[test]
    fn a_very_wide_page_still_has_a_thumb_to_grab() {
        let wide = Sideways {
            offset: 0.0,
            view: 400.0,
            content: 40_000.0,
        };
        assert_eq!(wide.thumb(400.0), Some((0.0, MIN_THUMB)));
        // But never longer than the rail.
        assert_eq!(wide.thumb(20.0), Some((0.0, 20.0)));
    }

    #[test]
    fn dragging_the_thumb_moves_the_page_across_its_whole_range() {
        let p = page(0.0);
        // The thumb travels 300 points for the page's 1200.
        assert_eq!(p.dragged(400.0, 0.0, 75.0), 300.0);
        assert_eq!(p.dragged(400.0, 300.0, -75.0), 0.0);
        assert_eq!(p.dragged(400.0, 0.0, 1000.0), 1200.0);
        assert_eq!(p.dragged(400.0, 600.0, -1000.0), 0.0);
    }

    #[test]
    fn a_press_beside_the_thumb_centers_it_there() {
        let p = page(0.0);
        assert_eq!(p.pressed(400.0, 200.0), 600.0);
        assert_eq!(p.pressed(400.0, 390.0), 1200.0);
        assert_eq!(page(1200.0).pressed(400.0, 10.0), 0.0);
        let fits = Sideways {
            offset: 0.0,
            view: 400.0,
            content: 400.0,
        };
        assert_eq!(fits.pressed(400.0, 200.0), 0.0);
    }
}
