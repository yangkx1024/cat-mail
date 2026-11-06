//! The tray icon, there with or without a window: the app's icon with the number of
//! unread messages in the INBOXes, a click to bring the main window back, and a menu to
//! write a message, sync or quit. Lives with the app, like the engine it shows. What it
//! shows, and on Linux the item itself, are `catmail_frontend::tray`'s. See
//! docs/architecture/background.md.
#![cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd", target_os = "macos")),
    allow(dead_code, reason = "no tray here yet (#109)")
)]

use std::borrow::Cow;
use std::sync::Arc;

use catmail_core::RecvError;
use catmail_frontend::tray::Chosen;
pub use catmail_frontend::tray::Command;
use gpui_kit::{App, AppContext as _, Global, RenderImage, SvgRenderer, Task, WindowAppearance};

use crate::mailbox::{Coalesce, Refresh};
use crate::resident::{QuitReason, Resident};
use crate::theme::appearance::{self, Themes};

/// Where the desktop may have a tray to register with: the StatusNotifierItem.
#[cfg(all(any(target_os = "linux", target_os = "freebsd"), not(test)))]
mod platform {
    use catmail_frontend::tray::{Pixmap, linux};
    use futures_channel::mpsc::UnboundedSender;

    use super::{Chosen, Pixmaps};

    #[derive(Clone)]
    pub struct Item(linux::Item);

    impl Item {
        pub async fn spawn(chosen: UnboundedSender<Chosen>) -> Option<Self> {
            linux::Item::spawn(crate::APP_NAME, chosen).await.map(Self)
        }

        pub async fn show(&self, icon: Option<Pixmaps>, unread: u32) {
            let icon = icon.map(|pixmaps| {
                let pixmaps = pixmaps.into_iter();
                pixmaps
                    .map(|(size, bgra)| Pixmap::from_bgra(size, &bgra))
                    .collect()
            });
            self.0.show(icon, unread).await;
        }
    }
}

#[cfg(all(target_os = "macos", not(test)))]
#[path = "macos.rs"]
#[allow(unsafe_code, reason = "AppKit's status item, menu and images")]
mod platform;

/// No tray here yet (#109): there is never an item, so nothing is counted.
#[cfg(all(
    not(any(target_os = "linux", target_os = "freebsd", target_os = "macos")),
    not(test)
))]
mod platform {
    use futures_channel::mpsc::UnboundedSender;

    use super::{Chosen, Pixmaps};

    #[derive(Clone)]
    pub struct Item;

    impl Item {
        pub async fn spawn(_: UnboundedSender<Chosen>) -> Option<Self> {
            None
        }

        pub async fn show(&self, _: Option<Pixmaps>, _: u32) {}
    }
}

/// An item that keeps what it was shown, in order: the tool tip, and whether it was
/// given an icon.
#[cfg(test)]
mod platform {
    use std::sync::{Arc, Mutex};

    use futures_channel::mpsc::UnboundedSender;

    use super::{Chosen, Pixmaps};

    #[derive(Clone, Default)]
    pub struct Item(pub Arc<Mutex<Vec<(String, bool)>>>);

    impl Item {
        pub async fn spawn(_: UnboundedSender<Chosen>) -> Option<Self> {
            Some(Self::default())
        }

        pub async fn show(&self, icon: Option<Pixmaps>, unread: u32) {
            let shown = (catmail_frontend::tray::tool_tip(unread), icon.is_some());
            self.0.lock().unwrap().push(shown);
        }
    }
}

use platform::Item;

/// The app's icon, which the count is drawn on.
#[cfg(not(all(target_os = "macos", not(test))))]
const LIGHT: &str = include_str!("../../assets/app-icon/catmail-light.svg");
#[cfg(not(all(target_os = "macos", not(test))))]
const DARK: &str = include_str!("../../assets/app-icon/catmail-dark.svg");

/// What the icon looks like, as an SVG, and the sizes it is drawn at (square).
#[cfg(not(all(target_os = "macos", not(test))))]
fn icon(dark: bool, unread: u32) -> (Cow<'static, str>, &'static [u32]) {
    use catmail_frontend::tray::badged;
    let icon = if dark { DARK } else { LIGHT };
    (badged::svg(icon, unread), &badged::SIZES)
}

/// A template image, which macOS tints to suit the menu bar; the count is text beside it.
#[cfg(all(target_os = "macos", not(test)))]
fn icon(_dark: bool, _unread: u32) -> (Cow<'static, str>, &'static [u32]) {
    (Cow::Borrowed(MENU_BAR_TEMPLATE), &MENU_BAR_SIZES)
}

#[cfg(any(target_os = "macos", test))]
const MENU_BAR_TEMPLATE: &str = include_str!("../../assets/app-icon/catmail-menu-bar.svg");

/// 18 points high, as the menu bar's icons are, for 1x and 2x displays.
#[cfg(any(target_os = "macos", test))]
const MENU_BAR_SIZES: [u32; 2] = [18, 36];

/// The icon at each of its sizes, square, in straight-alpha BGRA.
type Pixmaps = Vec<(u32, Vec<u8>)>;

pub struct Tray {
    /// Unread messages in the INBOXes, once counted.
    unread: Option<u32>,
    counting: Coalesce,
    /// One drawing at a time, so an icon drawn for an older count can't land last.
    drawing: Coalesce,
    /// The SVG the item's icon was last drawn from: one that looks the same isn't drawn
    /// again (macOS's never changes).
    drawn: Option<Cow<'static, str>>,
    item: Option<Item>,
    _events: Task<()>,
    _chosen: Task<()>,
}

impl Global for Tray {}

impl Tray {
    /// Shows the tray icon for the mail of [`Resident`]'s engine, where the desktop has a
    /// tray. Counts nothing where it hasn't.
    pub fn start(cx: &mut App) {
        let (choose, mut chosen) = futures_channel::mpsc::unbounded();
        let chosen = cx.spawn(async move |cx| {
            while let Ok((command, activation_token)) = chosen.recv().await {
                cx.update(|cx| Self::choose(command, activation_token, cx));
            }
        });
        cx.set_global(Self {
            unread: None,
            counting: Coalesce::default(),
            drawing: Coalesce::default(),
            drawn: None,
            item: None,
            _events: Task::ready(()),
            _chosen: chosen,
        });
        cx.spawn(async move |cx| {
            let Some(item) = Item::spawn(choose).await else {
                return;
            };
            cx.update(|cx| Self::shown(item, cx));
        })
        .detach();
    }

    /// The tray has the item: counts from now on, and draws the icon in the desktop's
    /// appearance.
    fn shown(item: Item, cx: &mut App) {
        // The app is quitting.
        let Some(client) = Resident::client(cx) else {
            return;
        };
        let mut events = client.subscribe();
        let events = cx.spawn(async move |cx| {
            loop {
                let event = match events.recv().await {
                    Ok(event) => Some(event),
                    Err(RecvError::Lagged(_)) => None,
                    Err(RecvError::Closed) => break,
                };
                // The unread counts are the sidebar's.
                if Refresh::after(event.as_ref(), None, &[]).sidebar {
                    cx.update(Self::count);
                }
            }
        });
        let this = cx.global_mut::<Self>();
        this.item = Some(item);
        this._events = events;
        cx.observe_global::<Themes>(Self::draw).detach();
        Self::count(cx);
    }

    /// Does what `command`, chosen in the tray, asks for.
    pub fn choose(command: Command, activation_token: Option<String>, cx: &mut App) {
        match command {
            Command::Open => Resident::show_at(None, activation_token, cx),
            Command::Compose => Resident::compose(activation_token, cx),
            Command::Sync => Resident::sync(cx),
            Command::Quit => Resident::quit(QuitReason::Tray, cx),
        }
    }

    /// Counts the unread messages again, and shows the count if it changed.
    fn count(cx: &mut App) {
        let Some(client) = Resident::client(cx) else {
            return;
        };
        if !cx.global_mut::<Self>().counting.start() {
            return;
        }
        cx.spawn(async move |cx| {
            let counted = client.count_unread_in_inboxes().await;
            cx.update(|cx| {
                let this = cx.global_mut::<Self>();
                let again = this.counting.finish();
                match counted {
                    Ok(unread) if this.unread != Some(unread) => {
                        this.unread = Some(unread);
                        Self::draw(cx);
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "couldn't count the unread mail"),
                }
                if again {
                    Self::count(cx);
                }
            });
        })
        .detach();
    }

    /// Shows the count, with the icon for it in the desktop's appearance, drawn off the UI
    /// thread if it changed.
    fn draw(cx: &mut App) {
        let this = cx.global_mut::<Self>();
        let (Some(item), Some(unread)) = (this.item.clone(), this.unread) else {
            return;
        };
        if !this.drawing.start() {
            return;
        }
        let dark = matches!(
            appearance::system(cx),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let (svg, sizes) = icon(dark, unread);
        let unchanged = cx.global::<Self>().drawn.as_ref() == Some(&svg);
        let drawing = (!unchanged).then(|| {
            let renderer = cx.svg_renderer();
            cx.background_spawn(async move {
                let pixmaps = pixmaps(&renderer, &svg, sizes);
                (svg, pixmaps)
            })
        });
        cx.spawn(async move |cx| {
            let pixmaps = match drawing {
                Some(drawing) => {
                    let (svg, pixmaps) = drawing.await;
                    cx.update(|cx| cx.global_mut::<Self>().drawn = Some(svg));
                    Some(pixmaps)
                }
                None => None,
            };
            item.show(pixmaps, unread).await;
            // What changed meanwhile is drawn now, from what it is now.
            if cx.update(|cx| cx.global_mut::<Self>().drawing.finish()) {
                cx.update(Self::draw);
            }
        })
        .detach();
    }

    /// The unread messages counted last.
    #[cfg(test)]
    pub fn unread(cx: &App) -> Option<u32> {
        cx.global::<Self>().unread
    }

    /// What the item was shown, in order: the tool tip, and whether with a new icon.
    #[cfg(test)]
    fn showings(cx: &App) -> Vec<(String, bool)> {
        let item = cx.global::<Self>().item.as_ref();
        item.map(|item| item.0.lock().unwrap().clone())
            .unwrap_or_default()
    }
}

/// `svg` at each of `sizes`.
fn pixmaps(renderer: &SvgRenderer, svg: &str, sizes: &[u32]) -> Pixmaps {
    let svg = match renderer.parse_svg(svg.as_bytes()) {
        Ok(svg) => svg,
        Err(e) => {
            tracing::error!(error = %e, "couldn't draw the tray icon");
            return Vec::new();
        }
    };
    sizes
        .iter()
        .filter_map(|&size| {
            let side = gpui_kit::DevicePixels(size as i32);
            let image: Arc<RenderImage> = renderer
                .render_parsed(
                    &svg,
                    gpui_kit::SvgSize::ExactSize(gpui_kit::size(side, side)),
                )
                .ok()?;
            Some((size, image.as_bytes(0)?.to_vec()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;

    use super::*;
    use crate::settings::Preferences;
    use crate::test_support::{TestMail, wait_until};

    /// A tray showing its first count, with its engine.
    fn start(cx: &mut TestAppContext) -> TestMail {
        let mail = TestMail::start();
        cx.executor().allow_parking();
        let client = mail.client.clone();
        cx.update(|cx| {
            crate::init_app(Preferences::load_nowhere(), cx);
            Resident::hold(client, cx);
            Tray::start(cx);
        });
        wait_until(|| {
            cx.run_until_parked();
            !cx.read(Tray::showings).is_empty()
        });
        mail
    }

    #[gpui_kit::test(iterations = 20)]
    fn the_icon_shows_the_latest_count_however_its_drawings_finish(cx: &mut TestAppContext) {
        let _mail = start(cx);

        // Two counts in a row, the second while the first is being drawn.
        cx.update(|cx| {
            for unread in [7, 8] {
                cx.global_mut::<Tray>().unread = Some(unread);
                Tray::draw(cx);
            }
        });
        cx.run_until_parked();
        let shown = cx.read(Tray::showings);
        let last = shown.last().map(|(tool_tip, _)| tool_tip.as_str());
        assert_eq!(last, Some("8 unread messages"));
    }

    #[gpui_kit::test]
    fn an_icon_that_looks_the_same_isnt_drawn_again(cx: &mut TestAppContext) {
        let _mail = start(cx);

        // The appearance changed, say, but not the count.
        cx.update(Tray::draw);
        cx.run_until_parked();
        let shown = cx.read(Tray::showings);
        assert_eq!(shown.len(), 2);
        assert!(shown[0].1, "the first time, the icon is drawn");
        assert!(!shown[1].1, "drawn again: {shown:?}");
    }

    #[test]
    fn the_icon_is_drawn_at_every_size_with_its_badge() {
        let renderer = SvgRenderer::new(Arc::new(crate::assets::Assets));
        for dark in [false, true] {
            let (plain, sizes) = icon(dark, 0);
            let plain = pixmaps(&renderer, &plain, sizes);
            let badged = pixmaps(&renderer, &icon(dark, 12).0, sizes);
            let drawn: Vec<_> = badged.iter().map(|(size, _)| *size).collect();
            assert_eq!(drawn, sizes);
            for ((size, plain), (_, badged)) in plain.iter().zip(&badged) {
                assert_eq!(plain.len(), (size * size * 4) as usize);
                assert_ne!(plain, badged, "{size}");
            }
        }
    }

    /// macOS tints whatever isn't transparent.
    #[test]
    fn the_menu_bar_template_is_drawn() {
        let renderer = SvgRenderer::new(Arc::new(crate::assets::Assets));
        let drawn = pixmaps(&renderer, MENU_BAR_TEMPLATE, &MENU_BAR_SIZES);
        assert_eq!(drawn.len(), MENU_BAR_SIZES.len());
        for (size, bgra) in drawn {
            let (pixels, _) = bgra.as_chunks::<4>();
            let opaque = pixels.iter().filter(|[.., a]| *a > 128).count();
            let all = (size * size) as usize;
            assert!(
                all / 5 < opaque && opaque < all / 2,
                "{size}: {opaque} of {all}"
            );
        }
    }
}
