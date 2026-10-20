//! `catmail-gui`, the desktop frontend. So far it is the minimal reading window planned
//! for M2: browse folders, read messages as text or HTML, save attachments. It changes
//! nothing on the server; writing and sending come in M5.

mod add_account;
mod assets;
mod folders;
mod format;
mod html;
mod mailbox;
mod message_list;
mod reader;
#[cfg(test)]
mod test_support;

use std::path::PathBuf;

use catmail_core::{Config, MailClient, SyncSchedule};
use clap::Parser;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, IntoElement, KeyBinding, ParentElement as _, Render,
    SharedString, Styled as _, TitlebarOptions, Window, WindowBounds, WindowOptions, div, px, size,
};
use tracing_subscriber::EnvFilter;

use crate::mailbox::Mailbox;

#[derive(Parser)]
#[command(name = "catmail-gui", version, about = "cat-mail in a window")]
struct Cli {
    /// Directory for the database and blobs [default: platform data dir]
    #[arg(long, env = "CATMAIL_DATA_DIR")]
    data_dir: Option<PathBuf>,
}

gpui_kit::actions!(catmail, [Quit]);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    // Without `.runtime(..)` the engine runs its own tokio runtime: GPUI's executor is
    // not tokio, and the client's futures don't need it to be. New mail shows up as the
    // server announces it, through the engine's events.
    let client = cli
        .data_dir
        .map_or_else(Config::platform_default, |dir| {
            Ok(Config::with_data_dir(dir))
        })
        .and_then(|config| {
            MailClient::builder(config)
                .sync_in_background(Some(SyncSchedule::default()))
                .open()
        });

    gpui_kit::application()
        .with_assets(assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let opened = match client {
                Ok(client) => gpui_kit::open_window(window_options(cx), cx, |window, cx| {
                    cx.new(|cx| Mailbox::new(client, window, cx))
                })
                .map(drop),
                Err(e) => {
                    tracing::error!(error = %e, "failed to open the data directory");
                    let message = SharedString::from(e.to_string());
                    gpui_kit::open_window(window_options(cx), cx, |_, cx| {
                        cx.new(|_| OpenFailed { message })
                    })
                    .map(drop)
                }
            };
            if let Err(e) = opened {
                eprintln!("catmail-gui: couldn't open a window: {e:#}");
                cx.quit();
            }
        });
}

fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("cat-mail".into()),
            ..Default::default()
        }),
        // Window geometry is in device-independent pixels, not the rem scale.
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1280.), px(800.)),
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(480.))),
        ..Default::default()
    }
}

/// Shown instead of the mailbox when the data directory can't be opened, typically because
/// the CLI or another window is using it.
struct OpenFailed {
    message: SharedString,
}

impl Render for OpenFailed {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(div().font_semibold().child("Couldn't open the mail data"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.message.clone()),
            )
    }
}
