//! `catmail-nsh`: a spike of a catmail frontend on Native Shell (nsh), to see how its
//! GTK 4 backend does on Linux, brought up to `catmail-gui` feature by feature
//! (`ROADMAP.md`). It is the app for the data directory the GUI uses (`CATMAIL_DATA_DIR`
//! for another one), with the GUI's settings (`CATMAIL_CONFIG_DIR`): a launch of either
//! while the other runs brings the running one's window up.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(target_os = "linux")]
mod app;
#[cfg(target_os = "linux")]
mod commands;
#[cfg(target_os = "linux")]
mod composer;
#[cfg(target_os = "linux")]
mod draft;
#[cfg(target_os = "linux")]
mod engine;
#[cfg(target_os = "linux")]
mod form;
#[cfg(target_os = "linux")]
mod icons;
#[cfg(target_os = "linux")]
mod list;
#[cfg(target_os = "linux")]
mod login;
#[cfg(target_os = "linux")]
mod outbox;
mod page;
#[cfg(target_os = "linux")]
mod reader;
#[cfg(target_os = "linux")]
mod settings;
#[cfg(target_os = "linux")]
mod shell;
#[cfg(target_os = "linux")]
mod sidebar;
#[cfg(target_os = "linux")]
mod theme;
#[cfg(target_os = "linux")]
mod toast;

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<std::process::ExitCode> {
    use catmail_core::{BodyPrefetch, Config, DEFAULT_GC_INTERVAL, MailClient, SyncSchedule};
    use catmail_frontend::instance::{self, Claim, Display, Request};
    use catmail_frontend::settings::SettingsFile;

    catmail_core::logging::init("catmail_nsh=info");
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
            catmail_core::logging::write_file(&config, "nsh");
            (MailClient::builder_locked(lock), requests)
        }
        // Opening the engine says why.
        Claim::Unavailable => (MailClient::builder(config), None),
    };
    // What the GUI runs: the engine syncs, prefetches, sends and collects garbage on its
    // own, and its futures don't need tokio, which GTK's loop isn't.
    let builder = builder
        .sync_in_background(Some(SyncSchedule::default()))
        .prefetch_bodies(Some(BodyPrefetch::default()))
        .send_in_background(true)
        .collect_garbage_every(Some(DEFAULT_GC_INTERVAL));
    let code = shell::run(builder, requests, config_dir.map(SettingsFile::new));
    Ok(std::process::ExitCode::from(code.get()))
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("catmail-nsh: the nsh spike runs on Linux only");
    std::process::exit(1);
}
