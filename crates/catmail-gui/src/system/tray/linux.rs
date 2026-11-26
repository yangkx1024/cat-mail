//! The tray icon on Linux: the StatusNotifierItem, the app's icon with the count on it in
//! the desktop's appearance, which the settings portal says.

use std::cell::RefCell;

use catmail_frontend::tray::{Chosen, Pixmap, linux};
use futures_channel::mpsc::UnboundedSender;
use nsh_platform_linux::gtk4::{gio, glib, prelude::*};
use resvg::tiny_skia;
#[cfg(test)]
use resvg::usvg;

use crate::shell;

#[derive(Clone)]
pub struct Item(linux::Item);

impl Item {
    pub async fn spawn(chosen: UnboundedSender<Chosen>) -> Option<Self> {
        linux::Item::spawn(shell::APP_NAME, chosen).await.map(Self)
    }

    pub async fn show(&self, icon: Option<Vec<Pixmap>>, unread: u32) {
        self.0.show(icon, unread).await;
    }
}

/// The item's straight alpha from resvg's premultiplied; off the main thread.
pub fn icon_from(drawn: tiny_skia::Pixmap) -> Pixmap {
    let rgba: Vec<u8> = drawn
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    Pixmap::from_rgba(drawn.width(), &rgba)
}

thread_local! {
    /// The settings portal, followed for as long as the app runs.
    static PORTAL: RefCell<Option<gio::DBusProxy>> = const { RefCell::new(None) };
}

/// Follows the settings portal's `color-scheme` (1: dark), which the panel follows: the
/// app's own appearance (Settings) can differ. Without a portal the icon stays light.
pub fn follow_appearance() {
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
            PORTAL.with_borrow_mut(|portal| *portal = Some(proxy));
        },
    );
}

/// The portal's `color-scheme` (a `u`, maybe boxed in a `v`).
fn color_scheme(value: &glib::Variant) {
    let value = value.as_variant().unwrap_or_else(|| value.clone());
    if let Some(scheme) = value.get::<u32>() {
        super::appearance_changed(scheme == 1);
    }
}

#[cfg(test)]
mod tests {
    use catmail_frontend::tray::{DARK_ICON, LIGHT_ICON, badged};

    use super::*;

    /// The digits need the system's fonts, which usvg finds only with its `text` and
    /// `system-fonts` features.
    #[test]
    fn the_icon_is_drawn_at_every_size_with_its_count() {
        let drawn = |svg: &str| -> Vec<Pixmap> {
            let pixmaps = super::super::pixmaps(svg, &badged::SIZES).into_iter();
            pixmaps.map(icon_from).collect()
        };
        for icon in [LIGHT_ICON, DARK_ICON] {
            let plain = drawn(&badged::svg(icon, 0));
            let badged = drawn(&badged::svg(icon, 7));
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
            let sans_serif = super::super::FONTS.family_name(&usvg::fontdb::Family::SansSerif);
            assert!(white > 20, "{white} white pixels, in {sans_serif}");
        }
    }
}
