//! The tray icon, as the GUI's (`tray/`): the app's icon with the number of unread
//! messages in the INBOXes, a click to bring the main window back, and a menu to write a
//! message, sync or quit; there with or without a window. The item, its menu and what it
//! says are `catmail_frontend::tray`'s; here it is counted, drawn (resvg, off GTK's
//! thread) and followed, on GTK's thread. See docs/architecture/background.md.

use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::{Arc, LazyLock};

use catmail_core::RecvError;
use catmail_frontend::refresh::{Coalesce, Refresh};
use catmail_frontend::tray::linux::Item;
use catmail_frontend::tray::{Chosen, Pixmap, badged};
use futures_channel::mpsc;
use futures_util::StreamExt as _;
use nsh_platform_linux::gtk4::{gio, glib, prelude::*};
use resvg::{tiny_skia, usvg};

use crate::{engine, shell};

const LIGHT: &str = include_str!("../../catmail-gui/assets/app-icon/catmail-light.svg");
const DARK: &str = include_str!("../../catmail-gui/assets/app-icon/catmail-dark.svg");

#[derive(Default)]
struct Tray {
    item: Option<Item>,
    /// Unread messages in the INBOXes, once counted.
    unread: Option<u32>,
    /// The desktop's appearance (the portal's), which the icon is drawn in: the panel's,
    /// not the app's.
    dark: bool,
    counting: Coalesce,
    /// One drawing at a time, so an icon drawn for an older count can't land last.
    drawing: Coalesce,
    /// The SVG the item's icon was last drawn from: one that looks the same isn't drawn
    /// again.
    drawn: Option<Cow<'static, str>>,
    /// The settings portal, followed for as long as the app runs.
    portal: Option<gio::DBusProxy>,
}

thread_local! {
    /// GTK's thread's.
    static TRAY: RefCell<Tray> = RefCell::default();
}

/// Shows the tray icon for the open engine's mail, where the desktop has a tray. Counts
/// nothing where it hasn't. On GTK's thread, once the engine is open.
pub fn start() {
    let (choose, mut chosen) = mpsc::unbounded::<Chosen>();
    glib::spawn_future_local(async move {
        while let Some((command, activation_token)) = chosen.next().await {
            shell::chose(command, activation_token);
        }
    });
    glib::spawn_future_local(async move {
        let Some(item) = Item::spawn(shell::APP_NAME, choose).await else {
            return;
        };
        // The app is quitting.
        let Some(client) = engine::open() else {
            return;
        };
        TRAY.with_borrow_mut(|tray| tray.item = Some(item));
        follow_appearance();
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

/// Counts the unread messages again, and shows the count if it changed.
fn count() {
    let Some(client) = engine::open() else {
        return;
    };
    if !TRAY.with_borrow_mut(|tray| tray.counting.start()) {
        return;
    }
    glib::spawn_future_local(async move {
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

/// Shows the count, with the icon for it in the desktop's appearance, drawn off GTK's
/// thread if it changed.
fn draw() {
    let drawing = TRAY.with_borrow_mut(|tray| {
        let (Some(item), Some(unread)) = (tray.item.clone(), tray.unread) else {
            return None;
        };
        if !tray.drawing.start() {
            return None;
        }
        let icon = if tray.dark { DARK } else { LIGHT };
        let svg = badged::svg(icon, unread);
        let unchanged = tray.drawn.as_ref() == Some(&svg);
        Some((item, unread, (!unchanged).then_some(svg)))
    });
    let Some((item, unread, svg)) = drawing else {
        return;
    };
    glib::spawn_future_local(async move {
        let pixmaps = match svg {
            Some(svg) => {
                let drawn = svg.clone();
                let pixmaps = crate::app::off_thread(move || pixmaps(&drawn)).await;
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
/// thread.
static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_system_fonts();
    Arc::new(fonts)
});

/// `svg` at each of the badged icon's sizes.
fn pixmaps(svg: &str) -> Vec<Pixmap> {
    let options = usvg::Options {
        fontdb: FONTS.clone(),
        ..usvg::Options::default()
    };
    let tree = match usvg::Tree::from_str(svg, &options) {
        Ok(tree) => tree,
        Err(e) => {
            tracing::error!(error = %e, "couldn't draw the tray icon");
            return Vec::new();
        }
    };
    badged::SIZES
        .iter()
        .filter_map(|&size| {
            let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
            let scale = size as f32 / tree.size().width();
            resvg::render(
                &tree,
                tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            let rgba: Vec<u8> = pixmap
                .pixels()
                .iter()
                .flat_map(|pixel| {
                    let color = pixel.demultiply();
                    [color.red(), color.green(), color.blue(), color.alpha()]
                })
                .collect();
            Some(Pixmap::from_rgba(size, &rgba))
        })
        .collect()
}

// --- The desktop's appearance -------------------------------------------------------

/// Follows the settings portal's `color-scheme` (1: dark), which the panel follows: the
/// app's own appearance (Settings) can differ. Without a portal the icon stays light.
fn follow_appearance() {
    gio::DBusProxy::for_bus(
        gio::BusType::Session,
        gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES,
        None,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
        gio::Cancellable::NONE,
        |proxy| {
            let proxy = match proxy {
                Ok(proxy) => proxy,
                Err(e) => return tracing::info!(error = %e, "no settings portal"),
            };
            proxy.connect_g_signal(|_, _, signal, params| {
                if signal == "SettingChanged"
                    && let Some((namespace, key, value)) =
                        params.get::<(String, String, glib::Variant)>()
                    && namespace == "org.freedesktop.appearance"
                    && key == "color-scheme"
                {
                    color_scheme(&value);
                }
            });
            proxy.call(
                "ReadOne",
                Some(&("org.freedesktop.appearance", "color-scheme").to_variant()),
                gio::DBusCallFlags::NONE,
                1000,
                gio::Cancellable::NONE,
                |reply| match reply {
                    Ok(reply) => {
                        if let Some((value,)) = reply.get::<(glib::Variant,)>() {
                            color_scheme(&value);
                        }
                    }
                    Err(e) => tracing::info!(error = %e, "the portal has no color scheme"),
                },
            );
            TRAY.with_borrow_mut(|tray| tray.portal = Some(proxy));
        },
    );
}

/// The portal's `color-scheme` (a `u`, maybe boxed in a `v`): the icon is drawn again in
/// it if it changed.
fn color_scheme(value: &glib::Variant) {
    let value = value.as_variant().unwrap_or_else(|| value.clone());
    let Some(scheme) = value.get::<u32>() else {
        return;
    };
    let dark = scheme == 1;
    if TRAY.with_borrow_mut(|tray| std::mem::replace(&mut tray.dark, dark) != dark) {
        draw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digits need the system's fonts, which usvg finds only with its `text` and
    /// `system-fonts` features.
    #[test]
    fn the_icon_is_drawn_at_every_size_with_its_count() {
        for icon in [LIGHT, DARK] {
            let plain = pixmaps(&badged::svg(icon, 0));
            let badged = pixmaps(&badged::svg(icon, 7));
            let sizes: Vec<_> = badged.iter().map(|p| p.size).collect();
            assert_eq!(sizes, badged::SIZES);
            for (plain, badged) in plain.iter().zip(&badged) {
                assert_eq!(plain.argb.len(), (plain.size * plain.size * 4) as usize);
                assert_ne!(plain, badged, "{}", plain.size);
            }
            // White digits on the red badge, in its top right corner.
            let big = badged.last().unwrap();
            let side = big.size as usize;
            let (pixels, _) = big.argb.as_chunks::<4>();
            let white = (0..side / 2)
                .flat_map(|y| (side / 2..side).map(move |x| y * side + x))
                .filter(|&i| matches!(pixels[i], [255, r, g, b] if r > 240 && g > 240 && b > 240))
                .count();
            assert!(white > 20, "{white} white pixels");
        }
    }
}
