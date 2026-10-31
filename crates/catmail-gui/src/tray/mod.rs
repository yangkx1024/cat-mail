//! The tray icon, there with or without a window: the app's icon with the number of
//! unread messages in the INBOXes, a click to bring the main window back, and a menu to
//! write a message, sync or quit. Lives with the app, like the engine it shows. See
//! docs/architecture/background.md.

use std::sync::Arc;

use catmail_core::{MailClient, RecvError};
use gpui_kit::{App, AppContext as _, Global, RenderImage, SvgRenderer, Task, WindowAppearance};

use crate::mailbox::{Coalesce, Refresh};
use crate::resident::Resident;
use crate::theme::appearance::{self, Themes};

/// Where the desktop may have a tray to register with.
#[cfg(all(any(target_os = "linux", target_os = "freebsd"), not(test)))]
#[path = "linux.rs"]
mod platform;

/// No tray here yet (#108, #109): there is never an item, so nothing is counted.
#[cfg(all(not(any(target_os = "linux", target_os = "freebsd")), not(test)))]
mod platform {
    use futures_channel::mpsc::UnboundedSender;

    use super::{Chosen, Pixmaps};

    #[derive(Clone)]
    pub struct Item;

    impl Item {
        pub async fn spawn(_: UnboundedSender<Chosen>) -> Option<Self> {
            None
        }

        pub async fn show(&self, _: Pixmaps, _: String) {}
    }
}

/// An item that keeps the tool tips it was shown with, in order.
#[cfg(test)]
mod platform {
    use std::sync::{Arc, Mutex};

    use futures_channel::mpsc::UnboundedSender;

    use super::{Chosen, Pixmaps};

    #[derive(Clone, Default)]
    pub struct Item(pub Arc<Mutex<Vec<String>>>);

    impl Item {
        pub async fn spawn(_: UnboundedSender<Chosen>) -> Option<Self> {
            Some(Self::default())
        }

        pub async fn show(&self, _: Pixmaps, tool_tip: String) {
            self.0.lock().unwrap().push(tool_tip);
        }
    }
}

use platform::Item;

/// What the tray asks the app for. Clicking the icon is `Open`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd")),
    allow(dead_code, reason = "no tray to choose them in yet (#108, #109)")
)]
pub enum Command {
    Open,
    Compose,
    Sync,
    Quit,
}

/// The menu, top to bottom; `None` is a separator.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "freebsd")),
    allow(dead_code, reason = "no tray to show it in yet (#108, #109)")
)]
const MENU: [Option<(&str, Command)>; 5] = [
    Some(("Open catmail", Command::Open)),
    Some(("New message", Command::Compose)),
    Some(("Sync now", Command::Sync)),
    None,
    Some(("Quit", Command::Quit)),
];

/// A command, with the token that lets a window take focus on Wayland if the host gave
/// one.
type Chosen = (Command, Option<String>);

/// The sizes the icon is drawn at; the host picks what fits its panel.
const SIZES: [u32; 3] = [22, 32, 64];

/// The icon at each of [`SIZES`], in straight-alpha BGRA.
type Pixmaps = Vec<(u32, Vec<u8>)>;

pub struct Tray {
    client: MailClient,
    /// Unread messages in the INBOXes, once counted.
    unread: Option<u32>,
    counting: Coalesce,
    /// One drawing at a time, so an icon drawn for an older count can't land last.
    drawing: Coalesce,
    item: Option<Item>,
    _events: Task<()>,
    _chosen: Task<()>,
}

impl Global for Tray {}

impl Tray {
    /// Shows the tray icon for `client`'s mail, where the desktop has a tray. Counts
    /// nothing where it hasn't.
    pub fn start(client: MailClient, cx: &mut App) {
        let (choose, mut chosen) = futures_channel::mpsc::unbounded();
        let chosen = cx.spawn(async move |cx| {
            while let Ok((command, activation_token)) = chosen.recv().await {
                cx.update(|cx| Self::choose(command, activation_token, cx));
            }
        });
        cx.set_global(Self {
            client,
            unread: None,
            counting: Coalesce::default(),
            drawing: Coalesce::default(),
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
        let mut events = cx.global::<Self>().client.subscribe();
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
            Command::Quit => Resident::quit(cx),
        }
    }

    /// Counts the unread messages again, and shows the count if it changed.
    fn count(cx: &mut App) {
        let this = cx.global_mut::<Self>();
        if !this.counting.start() {
            return;
        }
        let client = this.client.clone();
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

    /// Draws the icon for the count, in the desktop's appearance, off the UI thread, and
    /// hands it to the tray.
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
        let renderer = cx.svg_renderer();
        let drawn = cx.background_spawn(async move { pixmaps(&renderer, dark, unread) });
        cx.spawn(async move |cx| {
            item.show(drawn.await, tool_tip(unread)).await;
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

    /// The tool tips the icon was shown with, in order.
    #[cfg(test)]
    fn tool_tips(cx: &App) -> Vec<String> {
        let item = cx.global::<Self>().item.as_ref();
        item.map(|item| item.0.lock().unwrap().clone())
            .unwrap_or_default()
    }
}

fn tool_tip(unread: u32) -> String {
    match unread {
        0 => "No unread mail".into(),
        1 => "1 unread message".into(),
        n => format!("{n} unread messages"),
    }
}

/// What the badge says, if there is one.
fn badge(unread: u32) -> Option<String> {
    match unread {
        0 => None,
        1..=99 => Some(unread.to_string()),
        _ => Some("99+".into()),
    }
}

const LIGHT: &str = include_str!("../../assets/app-icon/catmail-light.svg");
const DARK: &str = include_str!("../../assets/app-icon/catmail-dark.svg");

/// The app's icon with the badge on top, as one SVG; the icon keeps its own canvas
/// (512 × 512) as a nested `<svg>`.
fn icon_svg(dark: bool, unread: u32) -> String {
    let icon = if dark { DARK } else { LIGHT };
    let Some(text) = badge(unread) else {
        return icon.to_owned();
    };
    // Big enough to read at a panel's 22 pixels; wider for each digit past the first.
    let (height, size) = (260, 210);
    let width = height + (text.chars().count() as u32 - 1) * 110;
    let (x, radius) = (512 - width, height / 2);
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512">
{icon}
<rect x="{x}" y="0" width="{width}" height="{height}" rx="{radius}" fill="#d93025"/>
<text x="{cx}" y="{radius}" font-family="sans-serif" font-weight="bold" font-size="{size}" fill="#ffffff" text-anchor="middle" dominant-baseline="central">{text}</text>
</svg>"##,
        cx = x + width / 2,
    )
}

/// The icon at each of [`SIZES`].
fn pixmaps(renderer: &SvgRenderer, dark: bool, unread: u32) -> Pixmaps {
    let svg = match renderer.parse_svg(icon_svg(dark, unread).as_bytes()) {
        Ok(svg) => svg,
        Err(e) => {
            tracing::error!(error = %e, "couldn't draw the tray icon");
            return Vec::new();
        }
    };
    SIZES
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

    #[gpui_kit::test(iterations = 20)]
    fn the_icon_shows_the_latest_count_however_its_drawings_finish(cx: &mut TestAppContext) {
        let mail = TestMail::start();
        cx.executor().allow_parking();
        let client = mail.client.clone();
        cx.update(|cx| {
            crate::init_app(Preferences::load_nowhere(), cx);
            Tray::start(client, cx);
        });
        wait_until(|| {
            cx.run_until_parked();
            !cx.read(Tray::tool_tips).is_empty()
        });

        // Two counts in a row, the second while the first is being drawn.
        cx.update(|cx| {
            for unread in [7, 8] {
                cx.global_mut::<Tray>().unread = Some(unread);
                Tray::draw(cx);
            }
        });
        cx.run_until_parked();
        let shown = cx.read(Tray::tool_tips);
        assert_eq!(shown.last().map(String::as_str), Some("8 unread messages"));
    }

    #[test]
    fn the_menu_has_each_command_once() {
        let commands: Vec<_> = MENU.iter().flatten().map(|(_, command)| *command).collect();
        let all = [
            Command::Open,
            Command::Compose,
            Command::Sync,
            Command::Quit,
        ];
        assert_eq!(commands, all);
    }

    #[test]
    fn the_badge_counts_up_to_99() {
        assert_eq!(badge(0), None);
        assert_eq!(badge(7).as_deref(), Some("7"));
        assert_eq!(badge(99).as_deref(), Some("99"));
        assert_eq!(badge(100).as_deref(), Some("99+"));
    }

    #[test]
    fn the_icon_is_drawn_at_every_size_with_its_badge() {
        let renderer = SvgRenderer::new(Arc::new(crate::assets::Assets));
        for dark in [false, true] {
            let plain = pixmaps(&renderer, dark, 0);
            let badged = pixmaps(&renderer, dark, 12);
            let sizes: Vec<_> = badged.iter().map(|(size, _)| *size).collect();
            assert_eq!(sizes, SIZES);
            for ((size, plain), (_, badged)) in plain.iter().zip(&badged) {
                assert_eq!(plain.len(), (size * size * 4) as usize);
                assert_ne!(plain, badged, "{size}");
            }
        }
    }
}
