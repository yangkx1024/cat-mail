//! `catmail-gui`: catmail's desktop app, on Native Shell (nsh): its GTK 4 backend on
//! Linux, its AppKit one on macOS, message bodies in web views. `CATMAIL_DATA_DIR` opens
//! another data directory, `CATMAIL_CONFIG_DIR` reads other settings; a second launch
//! brings the running one's window up.

#![cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]

#[cfg(target_os = "linux")]
use nsh_platform_linux as nsh_platform;
#[cfg(target_os = "macos")]
use nsh_platform_macos as nsh_platform;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod app;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod chrome;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod commands;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod composer;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod draft;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod engine;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod form;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod icons;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod list;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod login;
#[cfg(target_os = "linux")]
mod new_mail;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod outbox;
mod page;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod reader;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod settings;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod shell;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod sidebar;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod theme;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod toast;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod tray;

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main() -> anyhow::Result<std::process::ExitCode> {
    use catmail_core::{BodyPrefetch, Config, DEFAULT_GC_INTERVAL, MailClient, SyncSchedule};
    use catmail_frontend::instance::{self, Claim, Display, Request};
    use catmail_frontend::settings::SettingsFile;

    catmail_core::logging::init("catmail_gui=info,catmail_frontend=info");
    let config = match std::env::var_os("CATMAIL_DATA_DIR") {
        Some(dir) => Config::with_data_dir(dir),
        None => Config::platform_default()?,
    };
    let config_dir = std::env::var_os("CATMAIL_CONFIG_DIR")
        .map(Into::into)
        .or_else(Config::platform_config_dir);
    // Before any window: a launch that only hands over shows nothing of its own.
    let activate = Request::Activate {
        display: Display::current(),
    };
    let (builder, requests) = match instance::claim(&config, &activate)? {
        Claim::Handed => return Ok(std::process::ExitCode::SUCCESS),
        Claim::First { lock, requests } => {
            catmail_core::logging::write_file(&config, "gui");
            (MailClient::builder_locked(lock), requests)
        }
        // Opening the engine says why.
        Claim::Unavailable => (MailClient::builder(config), None),
    };
    // The engine syncs, prefetches, sends and collects garbage on its
    // own, and its futures don't need tokio, which the toolkit's loop isn't.
    let builder = builder
        .sync_in_background(Some(SyncSchedule::default()))
        .prefetch_bodies(Some(BodyPrefetch::default()))
        .send_in_background(true)
        .collect_garbage_every(Some(DEFAULT_GC_INTERVAL));
    let code = shell::run(builder, requests, config_dir.map(SettingsFile::new));
    Ok(std::process::ExitCode::from(code))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() {
    eprintln!("catmail-gui: runs on Linux and macOS only, as nsh does");
    std::process::exit(1);
}
