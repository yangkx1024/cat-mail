//! One app per data directory. The launch that locks the data directory for its engine is
//! the app; a later launch hands its request (show the window) to it over a socket in the
//! data directory and exits, instead of failing to open the engine there. The data
//! directory is the user's alone, so only they can connect. See
//! docs/architecture/background.md.
//!
//! A socket's path is limited to about a hundred bytes. A longer data directory's socket
//! goes in the user's runtime directory instead, named after the data directory.

use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use catmail_core::{Config, DataDirLock};
use futures_channel::mpsc;
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
use uds_windows::{UnixListener, UnixStream};

const SOCKET: &str = "gui.sock";
/// `sun_path` holds 108 bytes on Linux and Windows and 104 on macOS, the final NUL
/// included.
const MAX_SOCKET_PATH: usize = 100;
/// Bumped when a request or the reply changes in a way the other side can't read.
const VERSION: u32 = 2;
/// How long a launch waits for an answer.
const PATIENCE: Duration = Duration::from_secs(5);
/// How long a launch waits for the one holding the data directory to start answering: the
/// moment between its locking and its listening. Longer, and it's not an app (the CLI).
const STARTING: Duration = Duration::from_secs(1);
/// How long a launch waits for an app that is quitting to let go of the data directory:
/// its MCP server's requests end (at most five seconds) and its engine closes.
const QUITTING: Duration = Duration::from_secs(30);
/// A request is one short line; anything longer isn't one.
const MAX_REQUEST: u64 = 64 * 1024;

/// What a launch asks the running app for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Show the main window. On Wayland the asking launch's activation token lets it take
    /// the focus; the app stays on the display it is connected to.
    Activate { activation_token: Option<String> },
}

impl Request {
    /// Show the main window, with this process's activation token (`XDG_ACTIVATION_TOKEN`,
    /// only set on Linux).
    pub fn activate() -> Self {
        let activation_token = cfg!(any(target_os = "linux", target_os = "freebsd"))
            .then(|| std::env::var("XDG_ACTIVATION_TOKEN").ok())
            .flatten()
            .filter(|v| !v.is_empty());
        Self::Activate { activation_token }
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    #[serde(flatten)]
    request: Request,
}

/// Read before the rest, so a launch of another version gets told rather than a parse error.
#[derive(Deserialize)]
struct Version {
    version: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Reply {
    error: Option<String>,
    /// The app is quitting and takes no more requests: the launch waits for it to let go
    /// of the data directory, and is the app then. `error` says so too, for launches that
    /// don't know this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    quitting: bool,
}

impl Reply {
    fn error(error: String) -> Self {
        Self {
            error: Some(error),
            quitting: false,
        }
    }

    fn quitting() -> Self {
        Self {
            error: Some("it is quitting".to_owned()),
            quitting: true,
        }
    }
}

/// Why a launch couldn't hand its request to the running app.
#[derive(Debug, thiserror::Error)]
pub enum HandError {
    #[error("the running catmail didn't answer")]
    NoAnswer(#[source] io::Error),
    #[error("the running catmail refused: {0}")]
    Refused(String),
}

/// Requests from later launches, in the order they came.
pub type Requests = mpsc::UnboundedReceiver<Request>;

pub enum Claim {
    /// The running app took the request; this launch has nothing more to do.
    Handed,
    /// This launch is the app for the data directory, holding it for its engine. Later
    /// launches' requests arrive on `requests`; `None` if they can't reach it (logged), and
    /// they will find the data directory taken.
    First {
        lock: DataDirLock,
        requests: Option<Requests>,
    },
    /// The data directory can't be had: something that doesn't answer holds it (the CLI),
    /// or it can't be opened at all. Opening the engine says which.
    Unavailable,
}

/// Makes this launch the app for the data directory, or hands `request` to the one that
/// is. Fails if that one doesn't take the request.
pub fn claim(config: &Config, request: &Request) -> Result<Claim, HandError> {
    claim_in(config, runtime_dir().as_deref(), request)
}

/// [`claim`], with `runtime` for sockets too long for the data directory.
fn claim_in(
    config: &Config,
    runtime: Option<&Path>,
    request: &Request,
) -> Result<Claim, HandError> {
    // Once: the lock is retried below. Opening the engine says why it can't be had.
    if config.prepare_data_dir().is_err() {
        return Ok(Claim::Unavailable);
    }
    let start = Instant::now();
    let mut quitting = false;
    loop {
        match config.lock() {
            Ok(lock) => {
                // The lock says nobody else serves the socket.
                let requests = socket_path(config.data_dir(), runtime)
                    .and_then(|socket| serve(&socket))
                    .inspect_err(
                        |e| tracing::warn!(error = %e, "other launches won't find this one"),
                    )
                    .ok();
                return Ok(Claim::First { lock, requests });
            }
            Err(catmail_core::Error::AlreadyRunning(_)) => {}
            Err(_) => return Ok(Claim::Unavailable),
        }
        // Held, so it exists and has a path. Asked again even once the app has said it is
        // quitting: another launch waiting for it may take over and answer.
        if let Ok(socket) = socket_path(config.data_dir(), runtime) {
            match hand(&socket, request)? {
                Handing::Taken => return Ok(Claim::Handed),
                Handing::Quitting => quitting = true,
                Handing::Nobody => {}
            }
        }
        if start.elapsed() >= if quitting { QUITTING } else { STARTING } {
            return Ok(Claim::Unavailable);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Where the app for the data directory `dir` (which must exist) listens: in it, or if
/// that path is too long, in `runtime` under a name made from it. Decided on the canonical
/// path, so a link to the directory or a relative path to it finds the same socket. Both
/// launches derive the same path, whichever version each is: the name doesn't depend on
/// Rust's hasher.
fn socket_path(dir: &Path, runtime: Option<&Path>) -> io::Result<PathBuf> {
    let dir = dunce::canonicalize(dir)?;
    let inside = dir.join(SOCKET);
    if inside.as_os_str().len() < MAX_SOCKET_PATH {
        return Ok(inside);
    }
    let runtime = runtime.ok_or_else(|| {
        io::Error::other(
            "the data directory's path is too long for a socket, and there is no runtime directory",
        )
    })?;
    let name = format!(
        "catmail-{:016x}.sock",
        crate::fnv1a(dir.as_os_str().as_encoded_bytes().iter().copied())
    );
    Ok(runtime.join(name))
}

/// The user's own directory for sockets: `$XDG_RUNTIME_DIR` on Linux (never `/tmp`, where
/// others can write), the per-user temporary directory elsewhere.
fn runtime_dir() -> Option<PathBuf> {
    if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        directories::BaseDirs::new()?
            .runtime_dir()
            .map(Path::to_path_buf)
    } else {
        Some(std::env::temp_dir())
    }
}

/// How handing a request to the app went, when it didn't fail.
enum Handing {
    Taken,
    /// Nobody listens.
    Nobody,
    /// The app is quitting: it will let go of the data directory.
    Quitting,
}

/// Sends `request` to the app listening on `socket`.
fn hand(socket: &Path, request: &Request) -> Result<Handing, HandError> {
    // Missing, or left by a run that ended: nobody to hand it to.
    let Ok(stream) = UnixStream::connect(socket) else {
        return Ok(Handing::Nobody);
    };
    let ask = || -> io::Result<Reply> {
        be_patient(&stream)?;
        write_line(
            &stream,
            &Envelope {
                version: VERSION,
                request: request.clone(),
            },
        )?;
        Ok(serde_json::from_str(&read_line(&stream)?)?)
    };
    match ask().map_err(HandError::NoAnswer)? {
        Reply { quitting: true, .. } => Ok(Handing::Quitting),
        Reply {
            error: Some(error), ..
        } => Err(HandError::Refused(error)),
        Reply { error: None, .. } => Ok(Handing::Taken),
    }
}

/// Listens on `socket` for the rest of the process.
fn serve(socket: &Path) -> io::Result<Requests> {
    // Left by a run that ended without removing it.
    crate::fs::remove_if_present(socket)?;
    let listener = UnixListener::bind(socket)?;
    // The directory already keeps others out unless the user widened it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    }
    let (sender, requests) = mpsc::unbounded();
    thread::Builder::new()
        .name("catmail-instance".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        if let Err(e) = answer(&stream, &sender) {
                            tracing::debug!(error = %e, "bad request from another launch");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "couldn't accept another launch"),
                }
            }
        })?;
    Ok(requests)
}

fn answer(stream: &UnixStream, sender: &mpsc::UnboundedSender<Request>) -> io::Result<()> {
    be_patient(stream)?;
    let line = read_line(stream)?;
    let reply = match serde_json::from_str::<Version>(&line).map(|v| v.version) {
        Ok(VERSION) => match serde_json::from_str::<Envelope>(&line) {
            Ok(Envelope { request, .. }) => match sender.unbounded_send(request) {
                Ok(()) => Reply::default(),
                // The app let go of the receiving end: it is quitting.
                Err(_) => Reply::quitting(),
            },
            Err(e) => Reply::error(format!("unknown request: {e}")),
        },
        Ok(version) => Reply::error(format!(
            "it speaks version {VERSION}, not {version}; quit it and start this one again"
        )),
        Err(e) => Reply::error(format!("unknown request: {e}")),
    };
    write_line(stream, &reply)
}

fn be_patient(stream: &UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))
}

fn write_line(mut stream: &UnixStream, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    stream.write_all(&line)
}

fn read_line(stream: &UnixStream) -> io::Result<String> {
    let mut line = String::new();
    BufReader::new(stream)
        .take(MAX_REQUEST)
        .read_line(&mut line)?;
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activate(token: &str) -> Request {
        Request::Activate {
            activation_token: Some(token.into()),
        }
    }

    /// Claims the data directory, as nobody holds it.
    fn first(config: &Config, runtime: Option<&Path>) -> (DataDirLock, Requests) {
        match claim_in(config, runtime, &activate("first")).unwrap() {
            Claim::First { lock, requests } => (lock, requests.expect("not serving")),
            Claim::Handed | Claim::Unavailable => panic!("nobody was running"),
        }
    }

    fn hands_over(config: &Config, runtime: Option<&Path>, mut requests: Requests) {
        assert!(matches!(
            claim_in(config, runtime, &activate("second")).unwrap(),
            Claim::Handed
        ));
        assert_eq!(requests.try_recv().ok(), Some(activate("second")));
    }

    #[test]
    fn a_second_launch_hands_its_request_to_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(dir.path().join("data"));
        let (_lock, requests) = first(&config, None);
        hands_over(&config, None, requests);
    }

    #[test]
    fn a_socket_left_by_an_ended_run_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(dir.path());
        drop(UnixListener::bind(dir.path().join(SOCKET)).unwrap());
        assert!(dir.path().join(SOCKET).exists());
        let (_lock, requests) = first(&config, None);
        hands_over(&config, None, requests);
    }

    #[test]
    fn a_data_directory_too_long_for_a_socket_still_has_one_app() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let data = dir
            .path()
            .join("a-data-directory-whose-path-is-longer".repeat(3));
        let config = Config::with_data_dir(&data);
        let (_lock, requests) = first(&config, Some(runtime.path()));
        let socket = socket_path(&data, Some(runtime.path())).unwrap();
        assert_eq!(socket.parent(), Some(runtime.path()));
        hands_over(&config, Some(runtime.path()), requests);
    }

    #[test]
    fn a_short_data_directory_keeps_its_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_path(dir.path(), None).unwrap();
        assert_eq!(
            socket,
            dunce::canonicalize(dir.path()).unwrap().join(SOCKET)
        );
        // The name of a long one is the same in every build.
        assert_eq!(crate::fnv1a(*b"/data"), 0x2e61_291c_e2d9_239e);
    }

    /// Windows needs a privilege for links.
    #[cfg(unix)]
    #[test]
    fn a_short_link_to_a_long_data_directory_finds_its_app() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let data = dir
            .path()
            .join("a-data-directory-whose-path-is-longer".repeat(3));
        let link = dir.path().join("d");
        std::fs::create_dir(&data).unwrap();
        std::os::unix::fs::symlink(&data, &link).unwrap();

        let (_lock, requests) = first(&Config::with_data_dir(&data), Some(runtime.path()));
        hands_over(
            &Config::with_data_dir(&link),
            Some(runtime.path()),
            requests,
        );
    }

    #[test]
    fn a_launch_waits_for_one_that_is_starting() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(dir.path());
        // Locked, not yet listening: the moment between the two in `claim`.
        let lock = config.lock().unwrap();
        let socket = dir.path().join(SOCKET);
        let starting = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            serve(&socket).unwrap()
        });
        assert!(matches!(
            claim(&config, &activate("second")).unwrap(),
            Claim::Handed
        ));
        let mut requests = starting.join().unwrap();
        assert_eq!(requests.try_recv().ok(), Some(activate("second")));
        drop(lock);
    }

    #[test]
    fn a_launch_waits_for_one_that_is_quitting_and_takes_over() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(dir.path());
        let (lock, requests) = first(&config, None);
        // Quitting: it takes no more requests, and lets go of the data directory later
        // than a starting launch waits.
        drop(requests);
        let quitting = thread::spawn(move || {
            thread::sleep(STARTING + Duration::from_millis(300));
            drop(lock);
        });
        let (_lock, requests) = first(&config, None);
        quitting.join().unwrap();
        hands_over(&config, None, requests);
    }

    #[test]
    fn launches_waiting_for_one_that_is_quitting_find_the_one_that_takes_over() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(dir.path());
        let (lock, requests) = first(&config, None);
        drop(requests);
        let waiting: Vec<_> = (0..2)
            .map(|_| {
                let config = config.clone();
                thread::spawn(move || claim_in(&config, None, &activate("second")).unwrap())
            })
            .collect();
        // Both have been told it is quitting.
        thread::sleep(Duration::from_millis(300));
        drop(lock);
        let claims: Vec<_> = waiting.into_iter().map(|w| w.join().unwrap()).collect();
        let mut firsts = claims.into_iter().filter_map(|claim| match claim {
            Claim::First { lock, requests } => Some((lock, requests.unwrap())),
            Claim::Handed => None,
            Claim::Unavailable => panic!("the data directory was let go of"),
        });
        let (_lock, mut requests) = firsts.next().expect("one takes over");
        assert!(firsts.next().is_none());
        assert_eq!(requests.try_recv().ok(), Some(activate("second")));
    }

    #[test]
    fn a_launch_without_quitting_replies_still_reads_one_as_an_error() {
        #[derive(Deserialize)]
        struct Old {
            error: Option<String>,
        }
        let line = serde_json::to_string(&Reply::quitting()).unwrap();
        assert!(serde_json::from_str::<Old>(&line).unwrap().error.is_some());
        assert_eq!(
            serde_json::to_string(&Reply::default()).unwrap(),
            r#"{"error":null}"#
        );
    }

    #[test]
    fn a_data_directory_held_by_something_else_is_left_to_the_engine_to_report() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(dir.path());
        // As the CLI does: locked, never listening.
        let _cli = config.lock().unwrap();
        assert!(matches!(
            claim(&config, &activate("second")).unwrap(),
            Claim::Unavailable
        ));
    }

    #[test]
    fn another_version_is_told_rather_than_misread() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(dir.path());
        let (_lock, mut requests) = first(&config, None);
        let stream = UnixStream::connect(dir.path().join(SOCKET)).unwrap();
        (&stream)
            .write_all(b"{\"version\":3,\"request\":\"something_new\"}\n")
            .unwrap();
        let reply: Reply = serde_json::from_str(&read_line(&stream).unwrap()).unwrap();
        assert!(reply.error.unwrap().contains("not 3"));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn a_request_arrives_as_sent() {
        let request = Request::Activate {
            activation_token: Some("token".into()),
        };
        let line = serde_json::to_string(&Envelope {
            version: VERSION,
            request: request.clone(),
        })
        .unwrap();
        assert!(line.starts_with("{\"version\":2,\"request\":\"activate\""));
        let back: Envelope = serde_json::from_str(&line).unwrap();
        assert_eq!(back.request, request);
    }
}
