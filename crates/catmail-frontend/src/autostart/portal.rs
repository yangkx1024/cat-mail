//! Opening at login in the Flatpak: the sandbox can't write the user's autostart folder,
//! so the Background portal writes the entry (`RequestBackground` with `autostart`),
//! which starts `flatpak run --command=catmail-gui net.yangkx.catmail --background`.
//!
//! The portal tells nobody whether the entry is there, and the sandbox can't see it. What
//! it answered last is kept here instead, as an empty file in the app's configuration
//! folder ([`super::use_config_dir`]), and read as the state. Turned off outside the app (the
//! desktop's autostart settings, its background permission), the switch shows it on
//! until pressed again, which asks the portal: it answers no entry where the app may not
//! run in the background.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures_util::StreamExt as _;
use futures_util::future::{Either, select};
use zbus::Proxy;
use zbus::blocking::Connection;
use zbus::proxy::{Builder, CacheProperties};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::{CONFIG_DIR, Error, State};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
/// Names the request object the portal answers on: one request at a time, each on a
/// connection of its own.
const TOKEN: &str = "catmail_autostart";
/// The portal's responses: answered, and the user dismissed its dialog.
const ANSWERED: u32 = 0;
const DISMISSED: u32 = 1;
/// Why the app asks, which the portal may show the user.
const REASON: &str = "Open catmail at login to sync mail in the background";

/// Once told where to keep the answer.
pub(super) fn available() -> bool {
    record().is_some()
}

pub(super) fn state() -> State {
    record().map_or(State::Off, |record| recorded(&record))
}

pub(super) fn set(on: bool) -> (State, Result<(), Error>) {
    let Some(record) = record() else {
        let why = Error("There's no configuration folder to keep the setting in.".into());
        return (State::Off, Err(why));
    };
    let answer = Connection::session().and_then(|bus| request(&bus, on));
    settle(&record, on, answer)
}

/// Where the portal's last answer is kept, in the app's configuration folder.
fn record() -> Option<PathBuf> {
    CONFIG_DIR.get().map(|dir| dir.join("open-at-login"))
}

fn recorded(record: &Path) -> State {
    if record.exists() {
        State::On
    } else {
        State::Off
    }
}

/// Keeps the portal's `answer` to turning it on or off: whether the entry is there, or
/// `None` if the user dismissed the portal's dialog. The state is the portal's answer,
/// also when it couldn't be kept (which then fails): the entry is there or not whatever
/// the record says.
fn settle(
    record: &Path,
    on: bool,
    answer: zbus::Result<Option<bool>>,
) -> (State, Result<(), Error>) {
    match answer {
        Ok(None) => (recorded(record), Ok(())),
        Ok(Some(entry)) => {
            let state = if entry { State::On } else { State::Off };
            let done = if let Err(e) = keep(record, entry) {
                tracing::warn!(entry, error = %e, "couldn't keep the portal's answer");
                Err(Error(format!("Couldn't remember the setting: {e}")))
            } else if on && !entry {
                Err(Error(
                    "catmail isn't allowed to run in the background. The system's settings \
                     can allow it."
                        .into(),
                ))
            } else {
                Ok(())
            };
            (state, done)
        }
        Err(e) => {
            tracing::warn!(on, error = %e, "couldn't ask the Background portal");
            (recorded(record), Err(Error(e.to_string())))
        }
    }
}

fn keep(record: &Path, entry: bool) -> std::io::Result<()> {
    if entry {
        crate::fs::write_atomically(record, b"")
    } else {
        crate::fs::remove_if_present(record)
    }
}

/// The request object the portal answers the caller `sender` (its unique name) on.
fn request_path(sender: &str, token: &str) -> String {
    let sender = sender.trim_start_matches(':').replace('.', "_");
    format!("{DESKTOP_PATH}/request/{sender}/{token}")
}

/// Asks the portal for the entry (`on`) or for its removal: whether the entry is there
/// afterwards, `None` if the user dismissed its dialog. Blocks until the portal answers,
/// which may wait for the user, or leaves the bus.
fn request(bus: &Connection, on: bool) -> zbus::Result<Option<bool>> {
    pollster::block_on(request_on(bus.inner(), on))
}

async fn request_on(bus: &zbus::Connection, on: bool) -> zbus::Result<Option<bool>> {
    let sender = bus
        .unique_name()
        .ok_or_else(|| zbus::Error::Failure("no name on the session bus".into()))?;
    // The answer comes as a signal from a request object named after the caller and the
    // token: listened to before asking, lest it come before the call returns.
    let path = request_path(sender, TOKEN);
    let mut answers = proxy(bus, &path, "org.freedesktop.portal.Request")
        .await?
        .receive_signal("Response")
        .await?;
    let portal = proxy(bus, DESKTOP_PATH, "org.freedesktop.portal.Background").await?;
    let options = HashMap::from([
        ("handle_token", Value::from(TOKEN)),
        ("reason", Value::from(REASON)),
        ("autostart", Value::from(on)),
        (
            "commandline",
            Value::from(vec!["catmail-gui", "--background"]),
        ),
        ("dbus-activatable", Value::from(false)),
    ]);
    // No parent window: the dialog, if the portal shows one, stands on its own.
    let reply = portal
        .call_method("RequestBackground", &("", options))
        .await?;
    let handle: OwnedObjectPath = reply.body().deserialize()?;
    if handle.as_str() != path {
        // A portal older than tokens (before 0.9): it answers somewhere else.
        return Err(zbus::Error::Failure(format!(
            "the portal answers at {}, not {path}",
            handle.as_str()
        )));
    }
    // A portal that leaves the bus (crashed, restarted) knows nothing of the request any
    // more, and the answer would never come. Watched by the unique name of the one that
    // took it: the well-known name also changes hands when the bus starts the portal for
    // the call, which is no leaving.
    let taker = reply
        .header()
        .sender()
        .ok_or_else(|| zbus::Error::Failure("the portal's reply has no sender".into()))?
        .to_owned();
    let names = zbus::fdo::DBusProxy::builder(bus)
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let mut gone = names
        .receive_name_owner_changed_with_args(&[(0, taker.as_str())])
        .await?;
    // It may have left before that listened.
    if !names.name_has_owner(taker.clone().into()).await? {
        return Err(zbus::Error::Failure(
            "the portal left without answering".into(),
        ));
    }
    let answer = match select(answers.next(), gone.next()).await {
        Either::Left((Some(answer), _)) => answer,
        _ => {
            return Err(zbus::Error::Failure(
                "the portal left without answering".into(),
            ));
        }
    };
    let (response, results): (u32, HashMap<String, OwnedValue>) = answer.body().deserialize()?;
    match response {
        ANSWERED => Ok(Some(
            results
                .get("autostart")
                .and_then(|entry| entry.downcast_ref::<bool>().ok())
                .unwrap_or(false),
        )),
        DISMISSED => Ok(None),
        _ => Err(zbus::Error::Failure(
            "the portal couldn't change opening at login".into(),
        )),
    }
}

async fn proxy<'a>(
    bus: &zbus::Connection,
    path: &'a str,
    interface: &'a str,
) -> zbus::Result<Proxy<'a>> {
    Builder::new(bus)
        .destination(DESKTOP)?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;

    /// A session bus of the test's own, which starts nothing but `portal`: this test
    /// binary running that test (ignored, so only the bus runs it), as the bus starts
    /// xdg-desktop-portal when nobody runs it yet.
    struct Bus {
        daemon: Child,
        address: String,
        _dir: tempfile::TempDir,
    }

    impl Bus {
        fn start() -> Self {
            Self::starting(None)
        }

        fn starting(portal: Option<&str>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let services = dir.path().join("services");
            std::fs::create_dir(&services).unwrap();
            if let Some(test) = portal {
                let exe = std::env::current_exe().unwrap();
                let service = format!(
                    "[D-BUS Service]\nName={DESKTOP}\nExec={} --exact \
                     autostart::portal::tests::{test} --ignored --test-threads=1\n",
                    exe.display()
                );
                std::fs::write(services.join(format!("{DESKTOP}.service")), service).unwrap();
            }
            let config = dir.path().join("bus.conf");
            std::fs::write(
                &config,
                format!(
                    "<busconfig><type>session</type><listen>unix:dir={}</listen>\
                     <auth>EXTERNAL</auth><servicedir>{}</servicedir>\
                     <policy context=\"default\">\
                     <allow send_destination=\"*\" eavesdrop=\"true\"/>\
                     <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                    dir.path().display(),
                    services.display()
                ),
            )
            .unwrap();
            let mut daemon = Command::new("dbus-daemon")
                .arg(format!("--config-file={}", config.display()))
                .args(["--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("dbus-daemon");
            let mut output = BufReader::new(daemon.stdout.take().unwrap());
            let mut address = String::new();
            output.read_line(&mut address).unwrap();
            let address = address.trim().to_owned();
            // The portal it starts prints there too, also after the test: read to its end.
            std::thread::spawn(move || std::io::copy(&mut output, &mut std::io::sink()));
            Self {
                daemon,
                address,
                _dir: dir,
            }
        }

        fn connection(&self) -> zbus::blocking::connection::Builder<'_> {
            zbus::blocking::connection::Builder::address(self.address.as_str()).unwrap()
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    /// What the portal does with every request.
    #[derive(Clone, Copy)]
    enum Says {
        /// Whether the entry is there.
        Entry(bool),
        Dismissed,
        /// Whether the entry is there, a moment after the call returns.
        Late(bool),
        /// Exits a moment after the call returns, without answering, as a portal that
        /// crashed.
        Leaves,
    }

    struct Portal {
        says: Says,
        asked: Arc<Mutex<Vec<HashMap<String, OwnedValue>>>>,
    }

    #[zbus::interface(name = "org.freedesktop.portal.Background")]
    impl Portal {
        async fn request_background(
            &self,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] bus: &zbus::Connection,
            _parent_window: &str,
            options: HashMap<String, OwnedValue>,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            let failed = |e: &dyn std::fmt::Display| zbus::fdo::Error::Failed(e.to_string());
            let caller = header.sender().ok_or_else(|| failed(&"no sender"))?;
            let token: String = options["handle_token"]
                .downcast_ref()
                .map_err(|e| failed(&e))?;
            let path = request_path(caller, &token);
            self.asked.lock().unwrap().push(options);
            let (response, entry) = match self.says {
                Says::Entry(entry) => (ANSWERED, entry),
                Says::Dismissed => (DISMISSED, false),
                Says::Late(entry) => (ANSWERED, entry),
                Says::Leaves => {
                    std::thread::spawn(|| {
                        std::thread::sleep(Duration::from_millis(100));
                        std::process::exit(0);
                    });
                    return OwnedObjectPath::try_from(path).map_err(|e| failed(&e));
                }
            };
            let results = HashMap::from([
                ("background", Value::from(entry)),
                ("autostart", Value::from(entry)),
            ]);
            let answer = {
                let (bus, caller, path) = (bus.clone(), caller.to_owned(), path.clone());
                async move {
                    let body = (response, results);
                    let interface = "org.freedesktop.portal.Request";
                    bus.emit_signal(Some(caller), path.as_str(), interface, "Response", &body)
                        .await
                }
            };
            if let Says::Late(_) = self.says {
                // Off zbus's executor, which runs this call.
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(200));
                    pollster::block_on(answer).unwrap();
                });
            } else {
                // Answered before the call returns, as a quick portal may: the caller
                // listens already.
                answer.await?;
            }
            OwnedObjectPath::try_from(path).map_err(|e| failed(&e))
        }
    }

    /// Turns it `on` or off against a portal that `says`, kept in `record`: the state
    /// after, whether it was done, and what the portal was asked.
    fn ask(
        says: Says,
        record: &Path,
        on: bool,
    ) -> (State, Result<(), Error>, HashMap<String, OwnedValue>) {
        let bus = Bus::start();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let portal = Portal {
            says,
            asked: asked.clone(),
        };
        let _portal = bus
            .connection()
            .name(DESKTOP)
            .unwrap()
            .serve_at(DESKTOP_PATH, portal)
            .unwrap()
            .build()
            .unwrap();
        let (state, done) = settle(record, on, request(&bus.connection().build().unwrap(), on));
        let options = asked.lock().unwrap().pop().expect("the portal was asked");
        (state, done, options)
    }

    #[test]
    fn turning_it_on_asks_for_an_entry_that_opens_no_window() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("open-at-login");
        let (state, done, options) = ask(Says::Entry(true), &record, true);
        assert_eq!(state, State::On);
        done.unwrap();
        assert!(record.exists());
        assert!(options["autostart"].downcast_ref::<bool>().unwrap());
        let commandline: Vec<String> = options["commandline"]
            .try_clone()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(commandline, ["catmail-gui", "--background"]);

        // And off again.
        let (state, done, options) = ask(Says::Entry(false), &record, false);
        assert_eq!(state, State::Off);
        done.unwrap();
        assert!(!record.exists());
        assert!(!options["autostart"].downcast_ref::<bool>().unwrap());
    }

    #[test]
    fn an_app_not_allowed_in_the_background_has_no_entry() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("open-at-login");
        std::fs::write(&record, b"").unwrap();
        let (state, done, _) = ask(Says::Entry(false), &record, true);
        assert_eq!(state, State::Off);
        assert!(done.is_err());
        assert!(!record.exists());
    }

    #[test]
    fn a_dismissed_dialog_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("open-at-login");
        std::fs::write(&record, b"").unwrap();
        let (state, done, _) = ask(Says::Dismissed, &record, false);
        assert_eq!(state, State::On);
        done.unwrap();
    }

    /// The portal the bus starts for a request, until the bus goes.
    fn serve(says: Says) {
        let address = std::env::var("DBUS_STARTER_ADDRESS").unwrap();
        let portal = Portal {
            says,
            asked: Arc::default(),
        };
        let bus = zbus::blocking::connection::Builder::address(address.as_str())
            .unwrap()
            .name(DESKTOP)
            .unwrap()
            .serve_at(DESKTOP_PATH, portal)
            .unwrap()
            .build()
            .unwrap();
        for message in zbus::blocking::MessageIterator::from(&bus) {
            if message.is_err() {
                break;
            }
        }
    }

    #[test]
    #[ignore = "started by the bus in a_portal_started_for_the_request_is_waited_for"]
    fn portal_answering_late() {
        serve(Says::Late(true));
    }

    #[test]
    #[ignore = "started by the bus in a_portal_that_leaves_without_answering_fails_the_change"]
    fn portal_leaving() {
        serve(Says::Leaves);
    }

    /// Turns it on against the portal the bus starts for it, `portal`.
    fn ask_started(portal: &str, record: &Path) -> (State, Result<(), Error>) {
        let bus = Bus::starting(Some(portal));
        settle(
            record,
            true,
            request(&bus.connection().build().unwrap(), true),
        )
    }

    #[test]
    fn a_portal_started_for_the_request_is_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("open-at-login");
        let (state, done) = ask_started("portal_answering_late", &record);
        done.unwrap();
        assert_eq!(state, State::On);
    }

    #[test]
    fn a_portal_that_leaves_without_answering_fails_the_change() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("open-at-login");
        let (state, done) = ask_started("portal_leaving", &record);
        assert!(done.is_err());
        assert_eq!(state, State::Off);
    }

    #[test]
    fn an_answer_that_cant_be_kept_is_still_the_state() {
        let dir = tempfile::tempdir().unwrap();
        // Nowhere to write: its folder is a file.
        let folder = dir.path().join("config");
        std::fs::write(&folder, b"").unwrap();
        let record = folder.join("open-at-login");
        let (state, done, _) = ask(Says::Entry(true), &record, true);
        assert_eq!(state, State::On);
        assert!(done.is_err());
    }
}
