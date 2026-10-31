//! One app per data directory. The launch that locks the data directory for its engine is
//! the app; a later launch hands its request (show the window) to it over a socket in the
//! data directory and exits, instead of failing to open the engine there. The data
//! directory is the user's alone, so only they can connect. See
//! docs/architecture/background.md.
//!
//! A socket's path is limited to about a hundred bytes. A longer data directory's socket
//! goes in the user's runtime directory instead, named after the data directory.

use std::fs;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
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
const VERSION: u32 = 1;
/// How long a launch waits for an answer.
const PATIENCE: Duration = Duration::from_secs(5);
/// How long a launch waits for the one holding the data directory to start answering: the
/// moment between its locking and its listening. Longer, and it's not an app (the CLI).
const STARTING: Duration = Duration::from_secs(1);
/// A request is one short line; anything longer isn't one.
const MAX_REQUEST: u64 = 64 * 1024;

/// What a launch asks the running app for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Show the main window, on the asking launch's display.
    Activate { display: Display },
}

/// The graphical session a launch was started in. The app may have none (it drops its
/// display connection while no window is open), so it opens the window on this one; on
/// Wayland the activation token lets that window take focus. Empty outside Linux, where a
/// process can only show windows in its own session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Display {
    pub wayland_display: Option<String>,
    pub x11_display: Option<String>,
    pub xdg_runtime_dir: Option<String>,
    pub activation_token: Option<String>,
}

impl Display {
    /// This process's, from its environment.
    pub fn current() -> Self {
        if !cfg!(any(target_os = "linux", target_os = "freebsd")) {
            return Self::default();
        }
        let var = |name| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self {
            wayland_display: var("WAYLAND_DISPLAY"),
            x11_display: var("DISPLAY"),
            xdg_runtime_dir: var("XDG_RUNTIME_DIR"),
            activation_token: var("XDG_ACTIVATION_TOKEN"),
        }
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

#[derive(Debug, Serialize, Deserialize)]
struct Reply {
    error: Option<String>,
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
pub fn claim(config: &Config, request: &Request) -> anyhow::Result<Claim> {
    claim_in(config, runtime_dir().as_deref(), request)
}

/// [`claim`], with `runtime` for sockets too long for the data directory.
fn claim_in(config: &Config, runtime: Option<&Path>, request: &Request) -> anyhow::Result<Claim> {
    let deadline = Instant::now() + STARTING;
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
        // Held, so it exists and has a path.
        if let Ok(socket) = socket_path(config.data_dir(), runtime)
            && hand(&socket, request)?
        {
            return Ok(Claim::Handed);
        }
        if Instant::now() >= deadline {
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
        fnv1a(dir.as_os_str().as_encoded_bytes())
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

/// FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Sends `request` to the app listening on `socket`; false if none does.
fn hand(socket: &Path, request: &Request) -> anyhow::Result<bool> {
    // Missing, or left by a run that ended: nobody to hand it to.
    let Ok(stream) = UnixStream::connect(socket) else {
        return Ok(false);
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
    let reply = ask().context("the running catmail didn't answer")?;
    match reply.error {
        Some(error) => bail!("the running catmail refused: {error}"),
        None => Ok(true),
    }
}

/// Listens on `socket` for the rest of the process.
fn serve(socket: &Path) -> io::Result<Requests> {
    // Left by a run that ended without removing it.
    match fs::remove_file(socket) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(socket)?;
    // The directory already keeps others out unless the user widened it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
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
    let error = match serde_json::from_str::<Version>(&line).map(|v| v.version) {
        Ok(VERSION) => match serde_json::from_str::<Envelope>(&line) {
            Ok(Envelope { request, .. }) => sender
                .unbounded_send(request)
                .err()
                .map(|_| "it is quitting".to_owned()),
            Err(e) => Some(format!("unknown request: {e}")),
        },
        Ok(version) => Some(format!(
            "it speaks version {VERSION}, not {version}; quit it and start this one again"
        )),
        Err(e) => Some(format!("unknown request: {e}")),
    };
    write_line(stream, &Reply { error })
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
            display: Display {
                activation_token: Some(token.into()),
                ..Display::default()
            },
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
        assert_eq!(fnv1a(b"/data"), 0x2e61_291c_e2d9_239e);
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
        fs::create_dir(&data).unwrap();
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
            .write_all(b"{\"version\":2,\"request\":\"something_new\"}\n")
            .unwrap();
        let reply: Reply = serde_json::from_str(&read_line(&stream).unwrap()).unwrap();
        assert!(reply.error.unwrap().contains("not 2"));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn a_request_arrives_as_sent() {
        let request = Request::Activate {
            display: Display {
                wayland_display: Some("wayland-1".into()),
                x11_display: None,
                xdg_runtime_dir: Some("/run/user/1000".into()),
                activation_token: Some("token".into()),
            },
        };
        let line = serde_json::to_string(&Envelope {
            version: VERSION,
            request: request.clone(),
        })
        .unwrap();
        assert!(line.starts_with("{\"version\":1,\"request\":\"activate\""));
        let back: Envelope = serde_json::from_str(&line).unwrap();
        assert_eq!(back.request, request);
    }
}
