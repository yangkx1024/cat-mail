//! The tray on Linux: a StatusNotifierItem over D-Bus (`ksni`), which KDE, the GNOME
//! AppIndicator extension, waybar and others show. Pure D-Bus, so it stays while the app
//! has no display connection.

use futures_channel::mpsc::UnboundedSender;
use ksni::menu::{MenuItem, StandardItem};
use ksni::{Category, Handle, Icon, ToolTip, TrayMethods as _};

use super::{Chosen, Command, MENU, Pixmaps};
use crate::APP_NAME;

struct StatusItem {
    icon: Vec<Icon>,
    tool_tip: String,
    chosen: UnboundedSender<Chosen>,
}

impl StatusItem {
    fn send(&self, command: Command) {
        let activation_token = ksni::current_xdg_activation_token().or_else(|| {
            // KDE may send the token just after the event.
            std::thread::sleep(std::time::Duration::from_millis(1));
            ksni::current_xdg_activation_token()
        });
        self.chosen.unbounded_send((command, activation_token)).ok();
    }
}

impl ksni::Tray for StatusItem {
    fn id(&self) -> String {
        APP_NAME.into()
    }

    fn title(&self) -> String {
        APP_NAME.into()
    }

    fn category(&self) -> Category {
        Category::Communications
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: self.tool_tip.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Command::Open);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        MENU.iter()
            .map(|entry| match *entry {
                Some((label, command)) => StandardItem {
                    label: label.into(),
                    activate: Box::new(move |this: &mut Self| this.send(command)),
                    ..Default::default()
                }
                .into(),
                None => MenuItem::Separator,
            })
            .collect()
    }
}

#[derive(Clone)]
pub struct Item(Handle<StatusItem>);

impl Item {
    /// Registers the item with the desktop's tray, if it has one: GNOME without the
    /// AppIndicator extension doesn't, and the window comes back from the launcher.
    pub async fn spawn(chosen: UnboundedSender<Chosen>) -> Option<Self> {
        let item = StatusItem {
            icon: Vec::new(),
            tool_tip: String::new(),
            chosen,
        };
        match item.spawn().await {
            Ok(handle) => Some(Self(handle)),
            Err(e) => {
                tracing::info!(error = %e, "no tray icon");
                None
            }
        }
    }

    pub async fn show(&self, pixmaps: Pixmaps, tool_tip: String) {
        let icon = pixmaps.into_iter().map(argb).collect();
        self.0
            .update(move |item| {
                item.icon = icon;
                item.tool_tip = tool_tip;
            })
            .await;
    }
}

/// From straight-alpha BGRA to the ARGB, in network byte order, that the specification
/// asks for.
fn argb((size, bgra): (u32, Vec<u8>)) -> Icon {
    let (pixels, _) = bgra.as_chunks::<4>();
    let data = pixels
        .iter()
        .flat_map(|&[b, g, r, a]| [a, r, g, b])
        .collect();
    Icon {
        width: size as i32,
        height: size as i32,
        data,
    }
}
