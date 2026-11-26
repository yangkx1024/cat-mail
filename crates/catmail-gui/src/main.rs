//! `catmail-gui`: catmail's desktop app, on Native Shell (nsh): its GTK 4 backend on
//! Linux, its AppKit one on macOS, message bodies in web views. `CATMAIL_DATA_DIR` opens
//! another data directory, `CATMAIL_CONFIG_DIR` reads other settings; a second launch
//! brings the running one's window up. `--background` opens no window, as at login.

#![cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]

#[cfg(target_os = "linux")]
use nsh_platform_linux as nsh_platform;
#[cfg(target_os = "macos")]
use nsh_platform_macos as nsh_platform;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod accounts;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod buttons;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod clock;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod engine;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod form;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod icons;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod mailbox;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod motion;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod shell;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod system;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod toast;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod web;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod windows;

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main() -> anyhow::Result<std::process::ExitCode> {
    use catmail_core::{Config, MailClient};
    use catmail_frontend::settings::SettingsFile;
    use catmail_frontend::system::instance::{self, Claim, Request};

    catmail_core::logging::init(&format!(
        "catmail_gui=info,catmail_frontend=info,{}",
        catmail_frontend::settings::mcp::LOG_FILTER
    ));
    let config = match std::env::var_os("CATMAIL_DATA_DIR") {
        Some(dir) => Config::with_data_dir(dir),
        None => Config::platform_default()?,
    };
    let config_dir = std::env::var_os("CATMAIL_CONFIG_DIR")
        .map(Into::into)
        .or_else(Config::platform_config_dir);
    // Before any window: a launch that only hands over shows nothing of its own.
    let activate = Request::activate();
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
    let builder = builder.resident();
    let hidden = std::env::args_os().skip(1).any(|arg| arg == "--background");
    let parts = shell::Parts {
        main_window: nsh_platform::Root::of::<mailbox::App>,
        // Before the app has finished launching: a notification clicked to launch it is
        // heard.
        launching: || {
            #[cfg(target_os = "macos")]
            system::new_mail::install();
        },
        // The tray and the notifications follow the engine, and it hears of wakes.
        engine_opened: || {
            system::new_mail::start();
            system::tray::start();
            system::wake::follow();
        },
    };
    let settings = config_dir.map(SettingsFile::new);
    let code = shell::run(builder, requests, settings, hidden, parts);
    Ok(std::process::ExitCode::from(code))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() {
    eprintln!("catmail-gui: runs on Linux and macOS only, as nsh does");
    std::process::exit(1);
}
