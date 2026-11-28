//! Updates in the Flatpak, through the Flatpak portal (`org.freedesktop.portal.Flatpak`),
//! which every Flatpak app may talk to:
//!
//! - Its update monitor (`CreateUpdateMonitor`) checks the app's remote every half hour (the
//!   portal's `--poll-timeout`; the first check half an hour after the first monitor, none
//!   on a metered network) and says `UpdateAvailable` with the commits running, installed
//!   and in the repository whenever those change. Nothing checks sooner: a new monitor
//!   doesn't. One is kept for as long as the app runs, on a thread of its own, made again
//!   if the portal leaves the bus.
//! - `Update` installs the repository's commit. The first time, the portal asks the user
//!   in a dialog of its own, and keeps the answer; its `Progress` signals say how it goes.
//!   It doesn't install a version that asks for more permissions.
//! - `Spawn` with `LATEST_VERSION` starts the installed version ([`restart`]), from the
//!   portal: it outlives the app, which quits.
//!
//! A system installation's updates need no password either: flatpak allows them to the
//! active session (polkit's `org.freedesktop.Flatpak.app-update`), the commits being
//! signed.

use std::collections::HashMap;
use std::ffi::OsString;
use std::time::Duration;

use futures_channel::mpsc;
use futures_util::{FutureExt as _, StreamExt as _, stream};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::tracker::{Commits, Progress, Tracker};
use super::{Error, Update};
use crate::system::portal_bus;

const PORTAL: &str = "org.freedesktop.portal.Flatpak";
const PORTAL_PATH: &str = "/org/freedesktop/portal/Flatpak";
const MONITOR: &str = "org.freedesktop.portal.Flatpak.UpdateMonitor";
/// Names the monitor's object.
const TOKEN: &str = "catmail_updates";
/// How long after the portal has left (crashed, restarted) the app asks for a monitor
/// again, which starts it again.
const AGAIN: Duration = Duration::from_secs(60);
/// `FLATPAK_SPAWN_FLAGS_LATEST_VERSION`: the installed version rather than the running one.
const LATEST_VERSION: u32 = 2;
/// What the app runs as (the manifest's `command`).
const COMMAND: &str = "catmail-gui";

/// The monitor, running.
pub struct Updates {
    installs: mpsc::UnboundedSender<()>,
}

impl Updates {
    /// Installs the repository's version: the watch says how it goes.
    pub fn install(&self) {
        let _ = self.installs.unbounded_send(());
    }
}

/// Watches for updates in the Flatpak, saying each change of what there is to `said` (on
/// the monitor's thread). `None` outside the Flatpak, or without a thread for it.
pub fn watch(said: impl Fn(Option<Update>) + Send + 'static) -> Option<Updates> {
    if !crate::in_flatpak() {
        return None;
    }
    let (installs, asked) = mpsc::unbounded();
    let started = std::thread::Builder::new()
        .name("catmail-updates".into())
        .spawn(move || {
            pollster::block_on(async {
                match zbus::Connection::session().await {
                    Ok(bus) => follow(&bus, &said, asked, AGAIN).await,
                    Err(e) => tracing::warn!(error = %e, "no session bus; no updates"),
                }
            });
        });
    if let Err(e) = started {
        tracing::warn!(error = %e, "couldn't start watching for updates");
        return None;
    }
    Some(Updates { installs })
}

/// Starts the installed version, as `catmail-gui` with the variables the app reads
/// (`CATMAIL_*`, `RUST_LOG`): the same data directory, settings and logging. Blocks until
/// the portal has started it, which waits for this one to let go of the data directory.
pub fn restart() -> Result<(), Error> {
    if !crate::in_flatpak() {
        return Err(Error(
            "Not in the Flatpak: there's no version installed to start.".into(),
        ));
    }
    let started = zbus::blocking::Connection::session().and_then(|bus| {
        pollster::block_on(spawn_latest(bus.inner(), carried(std::env::vars_os())))
    });
    match started {
        Ok(pid) => {
            tracing::info!(pid, "started the installed version");
            Ok(())
        }
        Err(e) => Err(Error(e.to_string())),
    }
}

/// Keeps a monitor, made again `again` after the portal leaves, telling `said` what it
/// says, and installs when `asked`. Ends when a monitor can't be had.
async fn follow(
    bus: &zbus::Connection,
    said: &impl Fn(Option<Update>),
    mut asked: mpsc::UnboundedReceiver<()>,
    again: Duration,
) {
    let mut tracker = Tracker::default();
    loop {
        if let Err(e) = monitor(bus, said, &mut asked, &mut tracker).await {
            tracing::warn!(error = %e, "couldn't watch for updates; no updates");
            return;
        }
        tracing::info!(
            ?again,
            "the Flatpak portal left; watching for updates again after a while"
        );
        if let Some(failed) = tracker.portal_left() {
            said(Some(failed));
        }
        // Nothing else runs on this thread.
        std::thread::sleep(again);
    }
}

/// What a monitor hears.
enum News {
    Available(zbus::Message),
    Progress(zbus::Message),
    Install,
    PortalLeft,
}

/// Makes a monitor and follows it, until the portal leaves (`Ok`).
async fn monitor(
    bus: &zbus::Connection,
    said: &impl Fn(Option<Update>),
    asked: &mut mpsc::UnboundedReceiver<()>,
    tracker: &mut Tracker,
) -> zbus::Result<()> {
    let path = monitor_path(&portal_bus::unique_name(bus)?, TOKEN);
    let monitor = portal_bus::proxy(bus, PORTAL, &path, MONITOR).await?;
    // Listened to before the monitor is made, as its signals may come before the call
    // returns.
    let available = monitor.receive_signal("UpdateAvailable").await?;
    let progress = monitor.receive_signal("Progress").await?;
    let portal = portal_bus::proxy(bus, PORTAL, PORTAL_PATH, PORTAL).await?;
    let options = HashMap::from([("handle_token", Value::from(TOKEN))]);
    let reply = portal
        .call_method("CreateUpdateMonitor", &(options,))
        .await?;
    let handle: OwnedObjectPath = reply.body().deserialize()?;
    if handle.as_str() != path {
        return Err(zbus::Error::Failure(format!(
            "the portal's monitor is at {}, not {path}",
            handle.as_str()
        )));
    }
    let left = portal_bus::left(bus, &reply).await?;
    tracing::info!("watching for updates");
    let mut news = stream::select(
        stream::select(available.map(News::Available), progress.map(News::Progress)),
        stream::select(
            asked.by_ref().map(|()| News::Install),
            left.into_stream().map(|()| News::PortalLeft),
        ),
    );
    while let Some(news) = news.next().await {
        let update = match news {
            News::Available(signal) => match commits(&signal) {
                Ok(commits) => {
                    tracing::info!(?commits, "the Flatpak portal found an update");
                    tracker.available(commits)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "an update the portal found, unread");
                    continue;
                }
            },
            News::Progress(signal) => match progress_of(&signal) {
                Ok(progress) => {
                    if !matches!(progress, Progress::Running { .. }) {
                        tracing::info!(?progress, "installing the update ended");
                    }
                    tracker.progress(progress)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "the update's progress, unread");
                    continue;
                }
            },
            News::Install => {
                said(Some(tracker.install()));
                tracing::info!("installing the update");
                // The portal answers at once, and goes on in the background.
                let none = HashMap::<&str, Value>::new();
                match monitor.call_method("Update", &("", none)).await {
                    Ok(_) => continue,
                    Err(e) => {
                        tracing::warn!(error = %e, "couldn't install the update");
                        Some(tracker.not_installing(e.to_string()))
                    }
                }
            }
            News::PortalLeft => return Ok(()),
        };
        said(update);
    }
    Ok(())
}

/// The monitor the portal makes for the caller `sender` and its `token`.
fn monitor_path(sender: &str, token: &str) -> String {
    portal_bus::handle_path(&format!("{PORTAL_PATH}/update_monitor"), sender, token)
}

/// The commits an `UpdateAvailable` says.
fn commits(signal: &zbus::Message) -> zbus::Result<Commits> {
    let info: HashMap<String, OwnedValue> = signal.body().deserialize()?;
    let commit = |key| text(&info, key);
    Ok(Commits {
        running: commit("running-commit"),
        local: commit("local-commit"),
        remote: commit("remote-commit"),
    })
}

/// The progress a `Progress` says.
fn progress_of(signal: &zbus::Message) -> zbus::Result<Progress> {
    let info: HashMap<String, OwnedValue> = signal.body().deserialize()?;
    let number = |key| {
        info.get(key)
            .and_then(|value| value.downcast_ref::<u32>().ok())
            .unwrap_or(0)
    };
    Ok(match number("status") {
        0 => Progress::Running {
            op: number("op"),
            n_ops: number("n_ops"),
            percent: number("progress"),
        },
        1 => Progress::Empty,
        2 => Progress::Done,
        _ => Progress::Failed {
            error: text(&info, "error"),
            message: text(&info, "error_message"),
        },
    })
}

fn text(info: &HashMap<String, OwnedValue>, key: &str) -> String {
    info.get(key)
        .and_then(|value| value.downcast_ref::<&str>().ok())
        .unwrap_or_default()
        .to_owned()
}

/// Of the process's environment `vars`, what the app reads, for the version it starts.
fn carried(vars: impl Iterator<Item = (OsString, OsString)>) -> HashMap<String, String> {
    vars.filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| name.starts_with("CATMAIL_") || name == "RUST_LOG")
        .collect()
}

/// Asks the portal to start the installed version with `envs`: its pid.
async fn spawn_latest(bus: &zbus::Connection, envs: HashMap<String, String>) -> zbus::Result<u32> {
    // Byte strings, ended with NUL as GLib reads them.
    let nul_ended = |bytes: &[u8]| [bytes, b"\0"].concat();
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let cwd = nul_ended(cwd.as_os_str().as_encoded_bytes());
    let argv = vec![nul_ended(COMMAND.as_bytes())];
    let fds = HashMap::<u32, zbus::zvariant::Fd<'_>>::new();
    let options = HashMap::<&str, Value>::new();
    let portal = portal_bus::proxy(bus, PORTAL, PORTAL_PATH, PORTAL).await?;
    let reply = portal
        .call_method("Spawn", &(cwd, argv, fds, envs, LATEST_VERSION, options))
        .await?;
    reply.body().deserialize()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc as std_mpsc;

    use super::*;
    use crate::system::test_bus::Bus;
    use crate::system::update::Failure;
    use crate::system::update::tracker::ACCESS_DENIED;

    /// How long a test waits for what it expects.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// What the fake portal was asked.
    #[derive(Debug, PartialEq)]
    enum Asked {
        /// A monitor, for the caller, at the path.
        Monitor(String, String),
        Update,
        Spawn {
            argv: Vec<Vec<u8>>,
            envs: HashMap<String, String>,
            flags: u32,
        },
    }

    struct Portal(std_mpsc::Sender<Asked>);

    #[zbus::interface(name = "org.freedesktop.portal.Flatpak")]
    impl Portal {
        async fn create_update_monitor(
            &self,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(object_server)] server: &zbus::ObjectServer,
            options: HashMap<String, OwnedValue>,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            let failed = |e: &dyn std::fmt::Display| zbus::fdo::Error::Failed(e.to_string());
            let caller = header.sender().ok_or_else(|| failed(&"no sender"))?;
            let token: String = options["handle_token"]
                .downcast_ref()
                .map_err(|e| failed(&e))?;
            let path = monitor_path(caller, &token);
            server
                .at(path.as_str(), Monitor(self.0.clone()))
                .await
                .map_err(|e| failed(&e))?;
            let _ = self
                .0
                .send(Asked::Monitor(caller.to_string(), path.clone()));
            OwnedObjectPath::try_from(path).map_err(|e| failed(&e))
        }

        async fn spawn(
            &self,
            _cwd: Vec<u8>,
            argv: Vec<Vec<u8>>,
            _fds: HashMap<u32, zbus::zvariant::OwnedFd>,
            envs: HashMap<String, String>,
            flags: u32,
            _options: HashMap<String, OwnedValue>,
        ) -> u32 {
            let _ = self.0.send(Asked::Spawn { argv, envs, flags });
            42
        }
    }

    struct Monitor(std_mpsc::Sender<Asked>);

    #[zbus::interface(name = "org.freedesktop.portal.Flatpak.UpdateMonitor")]
    impl Monitor {
        async fn update(&self, _parent_window: &str, _options: HashMap<String, OwnedValue>) {
            let _ = self.0.send(Asked::Update);
        }
    }

    /// The fake portal on `bus`, and what it is asked.
    fn portal(bus: &Bus) -> (zbus::blocking::Connection, std_mpsc::Receiver<Asked>) {
        let (asked, asks) = std_mpsc::channel();
        let portal = bus
            .connection()
            .name(PORTAL)
            .unwrap()
            .serve_at(PORTAL_PATH, Portal(asked))
            .unwrap()
            .build()
            .unwrap();
        (portal, asks)
    }

    /// The app watching on `bus`, made again `again` after the portal leaves: how to ask
    /// it to install, and what it says.
    fn watching(
        bus: &Bus,
        again: Duration,
    ) -> (
        mpsc::UnboundedSender<()>,
        std_mpsc::Receiver<Option<Update>>,
    ) {
        let app: zbus::Connection = bus.connection().build().unwrap().into();
        let (installs, asked) = mpsc::unbounded();
        let (says, said) = std_mpsc::channel();
        std::thread::spawn(move || {
            let said = move |update| {
                let _ = says.send(update);
            };
            pollster::block_on(follow(&app, &said, asked, again));
        });
        (installs, said)
    }

    /// The portal's signal `name` at the monitor `(caller, path)`, saying `info`.
    fn signal(
        portal: &zbus::blocking::Connection,
        (caller, path): &(String, String),
        name: &str,
        info: &[(&str, Value<'_>)],
    ) {
        let info: HashMap<&str, &Value<'_>> = info.iter().map(|(k, v)| (*k, v)).collect();
        portal
            .emit_signal(
                Some(caller.as_str()),
                path.as_str(),
                MONITOR,
                name,
                &(info,),
            )
            .unwrap();
    }

    fn monitor_made(asks: &std_mpsc::Receiver<Asked>) -> (String, String) {
        match asks.recv_timeout(PATIENCE).unwrap() {
            Asked::Monitor(caller, path) => (caller, path),
            other => panic!("asked {other:?}, not for a monitor"),
        }
    }

    fn available(running: &str, local: &str, remote: &str) -> [(&'static str, Value<'static>); 3] {
        [
            ("running-commit", Value::from(running.to_owned())),
            ("local-commit", Value::from(local.to_owned())),
            ("remote-commit", Value::from(remote.to_owned())),
        ]
    }

    #[test]
    fn an_update_found_is_installed_when_asked() {
        let bus = Bus::start();
        let (portal, asks) = portal(&bus);
        let (installs, said) = watching(&bus, AGAIN);
        let monitor = monitor_made(&asks);
        let next = || said.recv_timeout(PATIENCE).unwrap();

        signal(
            &portal,
            &monitor,
            "UpdateAvailable",
            &available("a", "a", "b"),
        );
        assert_eq!(next(), Some(Update::Available));

        installs.unbounded_send(()).unwrap();
        assert_eq!(next(), Some(Update::Installing(None)));
        assert_eq!(asks.recv_timeout(PATIENCE).unwrap(), Asked::Update);
        let running = [
            ("status", Value::from(0_u32)),
            ("op", Value::from(0_u32)),
            ("n_ops", Value::from(1_u32)),
            ("progress", Value::from(40_u32)),
        ];
        signal(&portal, &monitor, "Progress", &running);
        assert_eq!(next(), Some(Update::Installing(Some(40))));
        signal(
            &portal,
            &monitor,
            "Progress",
            &[("status", Value::from(2_u32))],
        );
        assert_eq!(next(), Some(Update::Installed));
    }

    #[test]
    fn a_refused_installation_says_why() {
        let bus = Bus::start();
        let (portal, asks) = portal(&bus);
        let (installs, said) = watching(&bus, AGAIN);
        let monitor = monitor_made(&asks);
        let next = || said.recv_timeout(PATIENCE).unwrap();

        signal(
            &portal,
            &monitor,
            "UpdateAvailable",
            &available("a", "a", "b"),
        );
        assert_eq!(next(), Some(Update::Available));
        installs.unbounded_send(()).unwrap();
        assert_eq!(next(), Some(Update::Installing(None)));
        let failed = [
            ("status", Value::from(3_u32)),
            ("error", Value::from(ACCESS_DENIED)),
            (
                "error_message",
                Value::from("Application update not allowed"),
            ),
        ];
        signal(&portal, &monitor, "Progress", &failed);
        assert_eq!(next(), Some(Update::Failed(Failure::NotAllowed)));
    }

    #[test]
    fn a_portal_that_comes_back_is_watched_again() {
        let bus = Bus::start();
        let (first, asks) = portal(&bus);
        let (_installs, said) = watching(&bus, Duration::from_millis(500));
        monitor_made(&asks);
        // The portal crashes, and the bus has another (as it would start one) before the
        // app asks again.
        drop(first);
        let (portal, asks) = portal(&bus);
        let monitor = monitor_made(&asks);
        signal(
            &portal,
            &monitor,
            "UpdateAvailable",
            &available("a", "b", "b"),
        );
        assert_eq!(
            said.recv_timeout(PATIENCE).unwrap(),
            Some(Update::Installed)
        );
    }

    #[test]
    fn restarting_starts_the_latest_version_with_the_apps_variables() {
        let bus = Bus::start();
        let (_portal, asks) = portal(&bus);
        let app: zbus::Connection = bus.connection().build().unwrap().into();
        let envs = carried(
            [
                ("CATMAIL_DATA_DIR", "/data"),
                ("RUST_LOG", "debug"),
                ("HOME", "/home/u"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into())),
        );
        assert_eq!(pollster::block_on(spawn_latest(&app, envs)).unwrap(), 42);
        let Asked::Spawn { argv, envs, flags } = asks.recv_timeout(PATIENCE).unwrap() else {
            panic!("not spawned");
        };
        assert_eq!(argv, [b"catmail-gui\0".to_vec()]);
        assert_eq!(flags, LATEST_VERSION);
        assert_eq!(
            envs,
            HashMap::from([
                ("CATMAIL_DATA_DIR".into(), "/data".into()),
                ("RUST_LOG".into(), "debug".into()),
            ])
        );
    }
}
