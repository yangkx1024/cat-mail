//! `catmail-gui`, the desktop frontend: a three-pane mailbox (accounts and folders,
//! messages, the message), composer windows, and settings with themes that can come
//! from a terminal's color scheme.

mod actions;
mod assets;
mod composer;
mod fonts;
mod format;
mod html;
mod instance;
mod layout;
mod login_form;
mod mailbox;
mod new_mail;
mod reader;
mod resident;
mod settings;
#[cfg(test)]
mod test_support;
mod theme;

use std::path::PathBuf;

use catmail_core::{Config, DEFAULT_GC_INTERVAL, MailClient, SyncSchedule};
use clap::Parser;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, QuitMode,
    Render, SharedString, Styled as _, Subscription, Window, WindowOptions, div, px, size,
};
use settings::{Preferences, Settings};
use tracing_subscriber::EnvFilter;

use crate::instance::{Claim, Display, Request};
use crate::mailbox::Mailbox;
use crate::resident::{Engine, Resident};

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
    let config = cli.data_dir.map_or_else(Config::platform_default, |dir| {
        Ok(Config::with_data_dir(dir))
    });
    // Before the app starts, so a launch that only hands over shows nothing of its own.
    // The lock goes to the engine: whoever holds it is the app other launches find.
    let (builder, requests) = match config {
        Ok(config) => {
            let activate = Request::Activate {
                display: Display::current(),
            };
            match instance::claim(&config, &activate) {
                Ok(Claim::Handed) => return,
                Ok(Claim::First { lock, requests }) => {
                    (Ok(MailClient::builder_locked(lock)), requests)
                }
                // Opening the engine says why.
                Ok(Claim::Unavailable) => (Ok(MailClient::builder(config)), None),
                Err(e) => {
                    eprintln!("catmail-gui: {e:#}");
                    std::process::exit(1);
                }
            }
        }
        // Opening the engine reports it.
        Err(e) => (Err(e), None),
    };
    // Without `.runtime(..)` the engine runs its own tokio runtime: GPUI's executor is
    // not tokio, and the client's futures don't need it to be. New mail shows up as the
    // server announces it, through the engine's events.
    let opening = builder.map(|builder| {
        builder
            .sync_in_background(Some(SyncSchedule::default()))
            .send_in_background(true)
            .collect_garbage_every(Some(DEFAULT_GC_INTERVAL))
    });
    let opening = async move { opening?.open_async().await };

    let app = gpui_kit::application()
        .with_assets(assets::Assets)
        // Closing the last window may leave the app running; see `Resident`.
        .with_quit_mode(QuitMode::Explicit);
    // macOS: clicking the Dock icon with no window open.
    app.on_reopen(|cx| {
        if cx.has_global::<Resident>() {
            Resident::show(Display::default(), cx);
        }
    });
    app.run(move |cx| {
        // Meanwhile the app and its window are set up.
        let opening = cx.background_executor().spawn(opening);
        init_app(
            match config_dir {
                Some(dir) => Preferences::load(dir),
                None => Preferences::load_nowhere(),
            },
            cx,
        );
        if let Err(e) = Resident::start(opening, requests, cx) {
            eprintln!("catmail-gui: couldn't open a window: {e:#}");
            cx.quit();
        }
    });
}

/// Sets up the app before its first window: components, settings, theme, keys and the
/// app-wide actions.
fn init_app(preferences: Preferences, cx: &mut App) {
    gpui_kit::init(cx);
    // Before any window or notification.
    cx.set_app_identity(new_mail::APP_ID, new_mail::APP_NAME);
    fonts::init(cx);
    cx.set_global(preferences);
    theme::appearance::init(cx);
    actions::bind_keys(cx);
    // Text size applies to every window, so it is the app's to handle.
    cx.on_action(|_: &actions::TextLarger, cx| step_text_size(1, cx));
    cx.on_action(|_: &actions::TextSmaller, cx| step_text_size(-1, cx));
    cx.on_action(|_: &actions::TextReset, cx| {
        Preferences::update(cx, |s| s.text_size = Settings::default().text_size);
    });
}

fn step_text_size(step: isize, cx: &mut App) {
    let size = Preferences::get(cx).stepped_text_size(step);
    Preferences::update(cx, |s| s.text_size = size);
}

pub(crate) fn window_options(cx: &App) -> WindowOptions {
    layout::window_options(
        "cat-mail",
        size(px(1280.), px(800.)),
        size(px(720.), px(480.)),
        cx,
    )
}

/// The main window: empty while the engine opens the data directory (which a migration
/// can make take a while), then the mailbox. Closing it drops the mailbox; the engine
/// stays with the app (see `Resident`).
pub(crate) struct MainWindow {
    mailbox: Option<Entity<Mailbox>>,
    /// Until the mailbox is in it.
    following_engine: Option<Subscription>,
}

impl MainWindow {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let following = cx.observe_global_in::<Resident>(window, Self::follow_engine);
        let mut this = Self {
            mailbox: None,
            following_engine: Some(following),
        };
        this.follow_engine(window, cx);
        this
    }

    fn follow_engine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Engine::Open(client) = Resident::engine(cx) else {
            // Rendered from the engine's state.
            cx.notify();
            return;
        };
        let client = client.clone();
        let mailbox = cx.new(|cx| Mailbox::new(client, window, cx));
        let reopen = Resident::adopt(&mailbox, cx);
        mailbox.update(cx, |mailbox, _| mailbox.reopen_at(reopen));
        self.mailbox = Some(mailbox);
        self.following_engine = None;
        cx.notify();
    }
}

impl Render for MainWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(mailbox) = &self.mailbox {
            return mailbox.clone().into_any_element();
        }
        match Resident::engine(cx) {
            Engine::Failed(message) => open_failed(message.clone(), cx),
            Engine::Opening | Engine::Open(_) => div()
                .size_full()
                .bg(cx.theme().background)
                .into_any_element(),
        }
    }
}

fn open_failed(message: SharedString, cx: &App) -> AnyElement {
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
                .child(message),
        )
        .into_any_element()
}
