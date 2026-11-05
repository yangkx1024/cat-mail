//! `catmail-nsh`: a spike of a catmail frontend on Native Shell (nsh), to see how its
//! GTK 4 backend does on Linux. Three panes (folders, messages, the message), bodies
//! in a WebKitGTK web view. Read-only: it opens the data directory the GUI uses
//! (`CATMAIL_DATA_DIR` for another one) and syncs it in the background.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(target_os = "linux")]
mod app;
mod page;

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use catmail_core::{Config, MailClient, SyncSchedule};
    use nsh_platform_linux::{AppOptions, AppSource, Root, run_app};

    catmail_core::logging::init("catmail_nsh=info");
    let config = match std::env::var_os("CATMAIL_DATA_DIR") {
        Some(dir) => Config::with_data_dir(dir),
        None => Config::platform_default()?,
    };
    // The engine runs its own tokio runtime; nsh's tasks await the client's futures on
    // the UI thread, which is not tokio.
    let client = MailClient::builder(config)
        .sync_in_background(Some(SyncSchedule::default()))
        .open()?;
    app::set_client(client);
    run_app(
        AppSource::Rust(Root::of::<app::App>()),
        AppOptions {
            title: "catmail (nsh)".to_owned(),
            app_id: "net.yangkx.catmail.nsh".to_owned(),
            size: (1280, 800),
            appearance: match std::env::var("CATMAIL_NSH_APPEARANCE").as_deref() {
                Ok("dark") => nsh_platform_linux::Appearance::Dark,
                Ok("light") => nsh_platform_linux::Appearance::Light,
                _ => nsh_platform_linux::Appearance::System,
            },
            ..Default::default()
        },
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("catmail-nsh: the nsh spike runs on Linux only");
    std::process::exit(1);
}
