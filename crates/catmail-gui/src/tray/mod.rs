//! The tray icon: on Linux the app's icon with the number of unread messages in the
//! INBOXes, a click to bring the main window back; on macOS a template in the menu bar with
//! the number beside it, and on the Dock icon. Either has a menu to write a message, sync
//! or quit, and is there with or without a window. The items, their menu, what they say,
//! the icons and when to count and draw are `catmail_frontend::tray`'s; here the count is
//! counted and the icon drawn (resvg, off the main thread), on the main thread. See
//! docs/architecture/background.md.

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod platform;

use std::cell::RefCell;
use std::sync::{Arc, LazyLock};

use catmail_core::RecvError;
use catmail_frontend::refresh::Refresh;
use catmail_frontend::tray::{Chosen, TrayModel};
use futures_channel::mpsc;
use futures_util::StreamExt as _;
use resvg::{tiny_skia, usvg};
use usvg::fontdb::{Database, Family, Query};

use self::platform::Item;
use crate::{engine, shell};

#[derive(Default)]
struct Tray {
    item: Option<Item>,
    model: TrayModel,
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
    if TRAY.with_borrow_mut(|tray| tray.model.appearance(dark)) {
        draw();
    }
}

/// Counts the unread messages again, and shows the count if it changed.
fn count() {
    let Some(client) = engine::open() else {
        return;
    };
    if !TRAY.with_borrow_mut(|tray| tray.model.count()) {
        return;
    }
    shell::spawn(async move {
        let counted = client.count_unread_in_inboxes().await;
        let counted = counted
            .inspect_err(|e| tracing::warn!(error = %e, "couldn't count the unread mail"))
            .ok();
        let (changed, again) = TRAY.with_borrow_mut(|tray| tray.model.counted(counted));
        if changed {
            draw();
        }
        if again {
            count();
        }
    });
}

/// Shows the count, with the icon for it drawn off the main thread if it changed.
fn draw() {
    let drawing = TRAY.with_borrow_mut(|tray| Some((tray.item.clone()?, tray.model.draw()?)));
    let Some((item, drawing)) = drawing else {
        return;
    };
    shell::spawn(async move {
        let pixmaps = match drawing.icon.clone() {
            Some((svg, sizes)) => {
                crate::tasks::off_thread(move || {
                    let pixmaps = pixmaps(&svg, sizes).into_iter();
                    pixmaps.map(platform::icon_from).collect::<Vec<_>>()
                })
                .await
            }
            None => None,
        };
        item.show(pixmaps, drawing.unread).await;
        // What changed meanwhile is drawn now, from what it is now.
        if TRAY.with_borrow_mut(|tray| tray.model.shown(drawing)) {
            draw();
        }
    });
}

/// The system's fonts, for the badge's digits: loaded once, on the first drawing's
/// thread that has text to draw.
static FONTS: LazyLock<Arc<Database>> = LazyLock::new(|| {
    let mut fonts = Database::new();
    fonts.load_system_fonts();
    installed_sans_serif(&mut fonts);
    Arc::new(fonts)
});

/// Sans-serif fonts one of which most systems have, tried in order when the one
/// `sans-serif` names isn't installed.
const SANS_SERIF: &[&str] = &[
    "DejaVu Sans",
    "Noto Sans",
    "Liberation Sans",
    "Cantarell",
    "Ubuntu",
    "Arial",
    "Helvetica",
];

/// Makes `sans-serif`, which the badge's digits are in, a font that is installed.
/// fontdb takes it from fontconfig's aliases, the first font each prefers, installed or
/// not, the last alias winning: on Ubuntu one that isn't there, and the digits weren't
/// drawn. Then the first of [`SANS_SERIF`] there is, else any font at all.
fn installed_sans_serif(fonts: &mut Database) {
    let installed = |fonts: &Database, family: Family| {
        let families = [family];
        let query = Query {
            families: &families,
            ..Query::default()
        };
        fonts.query(&query).is_some()
    };
    if installed(fonts, Family::SansSerif) {
        return;
    }
    let name = SANS_SERIF
        .iter()
        .find(|&&name| installed(fonts, Family::Name(name)))
        .map(|&name| name.to_owned())
        .or_else(|| {
            let face = fonts.faces().next()?;
            face.families.first().map(|(name, _)| name.clone())
        });
    if let Some(name) = name {
        tracing::info!(
            font = name,
            "sans-serif isn't installed: the tray's badge uses another"
        );
        fonts.set_sans_serif_family(name);
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever fontconfig's aliases name: Ubuntu's named one that isn't installed.
    #[test]
    fn sans_serif_is_a_font_that_is_installed() {
        let mut fonts = Database::new();
        fonts.load_system_fonts();
        fonts.set_sans_serif_family("No Such Font");
        installed_sans_serif(&mut fonts);
        let query = Query {
            families: &[Family::SansSerif],
            ..Query::default()
        };
        let name = fonts.family_name(&Family::SansSerif).to_owned();
        assert!(fonts.query(&query).is_some(), "sans-serif is {name}");
    }
}
