//! The tray icon, as the GUI's (`tray/`): on Linux the app's icon with the number of
//! unread messages in the INBOXes, a click to bring the main window back; on macOS a
//! template in the menu bar with the number beside it, and on the Dock icon. Either has a
//! menu to write a message, sync or quit, and is there with or without a window. The
//! items, their menu and what they say are `catmail_frontend::tray`'s; here the count is
//! counted, drawn (resvg, off the main thread) and followed, on the main thread. See
//! docs/architecture/background.md.

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod platform;

use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::{Arc, LazyLock};

use catmail_core::RecvError;
use catmail_frontend::refresh::{Coalesce, Refresh};
use catmail_frontend::tray::Chosen;
use futures_channel::mpsc;
use futures_util::StreamExt as _;
use resvg::{tiny_skia, usvg};

use self::platform::Item;
use crate::{engine, shell};

#[derive(Default)]
struct Tray {
    item: Option<Item>,
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

thread_local! {
    /// The main thread's.
    static TRAY: RefCell<Tray> = RefCell::default();
}

/// Shows the tray icon for the open engine's mail, where the desktop has a tray. Counts
/// nothing where it hasn't. On the main thread, once the engine is open.
pub fn start() {
    let (choose, mut chosen) = mpsc::unbounded::<Chosen>();
    shell::spawn(async move {
        while let Some((command, activation_token)) = chosen.next().await {
            shell::chose(command, activation_token);
        }
    });
    shell::spawn(async move {
        let Some(item) = Item::spawn(choose).await else {
            return;
        };
        // The app is quitting.
        let Some(client) = engine::open() else {
            return;
        };
        TRAY.with_borrow_mut(|tray| tray.item = Some(item));
        platform::follow_appearance();
        count();
        let mut events = client.subscribe();
        drop(client);
        loop {
            let event = match events.recv().await {
                Ok(event) => Some(event),
                Err(RecvError::Lagged(_)) => None,
                Err(RecvError::Closed) => break,
            };
            // The unread counts are the sidebar's.
            if Refresh::after(event.as_ref(), None, &[]).sidebar {
                count();
            }
        }
    });
}

/// The desktop's appearance is `dark` now: the icon is drawn again in it if it changed.
#[cfg_attr(
    target_os = "macos",
    expect(dead_code, reason = "a template follows by itself")
)]
fn appearance_changed(dark: bool) {
    if TRAY.with_borrow_mut(|tray| std::mem::replace(&mut tray.dark, dark) != dark) {
        draw();
    }
}

/// Counts the unread messages again, and shows the count if it changed.
fn count() {
    let Some(client) = engine::open() else {
        return;
    };
    if !TRAY.with_borrow_mut(|tray| tray.counting.start()) {
        return;
    }
    shell::spawn(async move {
        let counted = client.count_unread_in_inboxes().await;
        let (changed, again) = TRAY.with_borrow_mut(|tray| {
            let again = tray.counting.finish();
            let changed = match counted {
                Ok(unread) => tray.unread.replace(unread) != Some(unread),
                Err(e) => {
                    tracing::warn!(error = %e, "couldn't count the unread mail");
                    false
                }
            };
            (changed, again)
        });
        if changed {
            draw();
        }
        if again {
            count();
        }
    });
}

/// Shows the count, with the icon for it in the desktop's appearance, drawn off the main
/// thread if it changed.
fn draw() {
    let drawing = TRAY.with_borrow_mut(|tray| {
        let (Some(item), Some(unread)) = (tray.item.clone(), tray.unread) else {
            return None;
        };
        if !tray.drawing.start() {
            return None;
        }
        let (svg, sizes) = platform::icon(tray.dark, unread);
        let unchanged = tray.drawn.as_ref() == Some(&svg);
        Some((item, unread, (!unchanged).then_some((svg, sizes))))
    });
    let Some((item, unread, svg)) = drawing else {
        return;
    };
    shell::spawn(async move {
        let pixmaps = match svg {
            Some((svg, sizes)) => {
                let drawn = svg.clone();
                let pixmaps = crate::app::off_thread(move || {
                    let pixmaps = pixmaps(&drawn, sizes).into_iter();
                    pixmaps.map(platform::icon_from).collect::<Vec<_>>()
                })
                .await;
                TRAY.with_borrow_mut(|tray| tray.drawn = Some(svg));
                pixmaps
            }
            None => None,
        };
        item.show(pixmaps, unread).await;
        // What changed meanwhile is drawn now, from what it is now.
        if TRAY.with_borrow_mut(|tray| tray.drawing.finish()) {
            draw();
        }
    });
}

/// The system's fonts, for the badge's digits: loaded once, on the first drawing's
/// thread that has text to draw.
static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_system_fonts();
    Arc::new(fonts)
});

/// `svg` at each of `sizes`, square: RGBA, premultiplied.
fn pixmaps(svg: &str, sizes: &[u32]) -> Vec<tiny_skia::Pixmap> {
    let mut options = usvg::Options::default();
    // macOS's template has none: the count is the item's title there.
    if svg.contains("<text") {
        options.fontdb = FONTS.clone();
    }
    let tree = match usvg::Tree::from_str(svg, &options) {
        Ok(tree) => tree,
        Err(e) => {
            tracing::error!(error = %e, "couldn't draw the tray icon");
            return Vec::new();
        }
    };
    sizes
        .iter()
        .filter_map(|&size| {
            let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
            let scale = size as f32 / tree.size().width();
            resvg::render(
                &tree,
                tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            Some(pixmap)
        })
        .collect()
}
