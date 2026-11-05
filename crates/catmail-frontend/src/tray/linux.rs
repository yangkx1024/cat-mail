//! The tray on Linux: a StatusNotifierItem over D-Bus (`ksni`), which KDE, the GNOME
//! AppIndicator extension, waybar and others show. Pure D-Bus, so it stays while the app
//! has no display connection, and is no toolkit's: the frontend draws the icon and does
//! what is chosen.

use futures_channel::mpsc::UnboundedSender;
use ksni::menu::{MenuItem, StandardItem};
use ksni::{Category, Handle, Icon, ToolTip, TrayMethods as _};

use super::{Chosen, Command, MENU, Pixmap};

struct StatusItem {
    /// The app's name, which the host shows and tells items apart by.
    name: &'static str,
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
        self.name.into()
    }

    fn title(&self) -> String {
        self.name.into()
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
    /// AppIndicator extension doesn't, and the window comes back from the launcher. What
    /// is chosen in it is sent to `chosen`.
    pub async fn spawn(name: &'static str, chosen: UnboundedSender<Chosen>) -> Option<Self> {
        let item = StatusItem {
            name,
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

    /// Shows the count, and `icon` unless it is `None`: the one there looks the same.
    pub async fn show(&self, icon: Option<Vec<Pixmap>>, unread: u32) {
        let icon = icon.map(|pixmaps| pixmaps.into_iter().map(argb).collect());
        let tool_tip = super::tool_tip(unread);
        self.0
            .update(move |item| {
                if let Some(icon) = icon {
                    item.icon = icon;
                }
                item.tool_tip = tool_tip;
            })
            .await;
    }
}

fn argb(pixmap: Pixmap) -> Icon {
    Icon {
        width: pixmap.size as i32,
        height: pixmap.size as i32,
        data: pixmap.argb,
    }
}
