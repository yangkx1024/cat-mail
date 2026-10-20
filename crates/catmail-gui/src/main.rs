//! `catmail-gui`, the desktop frontend: a three-pane mailbox (accounts and folders,
//! messages, the message), composer windows, and settings with themes that can come
//! from a terminal's color scheme.

mod actions;
mod assets;
mod composer;
mod format;
mod html;
mod layout;
mod mailbox;
mod reader;
mod settings;
#[cfg(test)]
mod test_support;
mod theme;

use std::path::PathBuf;

use catmail_core::{Config, DEFAULT_GC_INTERVAL, MailClient, SyncSchedule};
use clap::Parser;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Window, WindowOptions, div, px, size,
};
use settings::{Preferences, Settings};
use tracing_subscriber::EnvFilter;

use crate::mailbox::Mailbox;

#[derive(Parser)]
#[command(name = "catmail-gui", version, about = "cat-mail in a window")]
struct Cli {
    /// Directory for the database and blobs [default: platform data dir]
    #[arg(long, env = "CATMAIL_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Directory for settings and imported themes [default: platform config dir]
    #[arg(long, env = "CATMAIL_CONFIG_DIR")]
    config_dir: Option<PathBuf>,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let config_dir = cli
        .config_dir
        .clone()
        .or_else(catmail_core::Config::platform_config_dir);
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
                .send_in_background(true)
                .collect_garbage_every(Some(DEFAULT_GC_INTERVAL))
                .open()
        });

    gpui_kit::application()
        .with_assets(assets::Assets)
        .run(move |cx| {
            init_app(
                match config_dir {
                    Some(dir) => Preferences::load(dir),
                    None => Preferences::load_nowhere(),
                },
                cx,
            );
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

/// Sets up the app before its first window: components, settings, theme, keys and the
/// app-wide actions.
fn init_app(preferences: Preferences, cx: &mut App) {
    gpui_kit::init(cx);
    cx.set_global(preferences);
    theme::appearance::init(cx);
    actions::bind_keys(cx);
    cx.on_action(|_: &actions::Quit, cx| composer::quit(cx));
    // Text size applies to every window, so it is the app's to handle.
    cx.on_action(|_: &actions::TextLarger, cx| step_text_size(1, cx));
    cx.on_action(|_: &actions::TextSmaller, cx| step_text_size(-1, cx));
    cx.on_action(|_: &actions::TextReset, cx| {
        Preferences::update(cx, |s| s.text_size = Settings::default().text_size);
    });
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}

fn step_text_size(step: isize, cx: &mut App) {
    let size = Preferences::get(cx).stepped_text_size(step);
    Preferences::update(cx, |s| s.text_size = size);
}

fn window_options(cx: &App) -> WindowOptions {
    layout::window_options(
        "cat-mail",
        size(px(1280.), px(800.)),
        size(px(720.), px(480.)),
        cx,
    )
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
