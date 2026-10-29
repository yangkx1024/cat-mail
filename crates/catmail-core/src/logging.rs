//! The frontends' log: what `RUST_LOG` asks for on stderr (warnings by default), and
//! the key steps of the engine and the frontend in `catmail.log` in the data directory,
//! for finding out later what happened (the GUI runs on with no window, `catmail watch`
//! for as long as it is left). One process at a time writes it, holding the lock next to
//! it. See docs/architecture/logging.md.

use std::fs::{self, File, TryLockError};
use std::io::{self, Write as _};
use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use catmail_store::private_file_options;
use tracing_subscriber::fmt::writer::OptionalWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer as _};

use crate::{Config, system, version};

/// What the file gets of the engine unless `CATMAIL_LOG` says otherwise; a frontend
/// adds its own.
const ENGINE_FILTER: &str = "warn,catmail_core=info,catmail_proto=info,catmail_store=info";
const FILE_NAME: &str = "catmail.log";
/// The file before, once the current one grew too long.
const OLD_FILE_NAME: &str = "catmail.1.log";
/// Held by the process writing the file, for as long as it does.
const LOCK_NAME: &str = "lock";
/// A file this long makes way for a new one: the two take at most about twice this.
const MAX_FILE_LEN: u64 = 8 * 1024 * 1024;

/// Set once [`write_file`] opens it.
static FILE: OnceLock<LogFile> = OnceLock::new();

/// Logs to stderr, and to the file once [`write_file`] opens it. `frontend_filter` is
/// what the file gets of the frontend, as `EnvFilter` directives. Panics go to the file
/// too. Call once, first thing in `main`.
pub fn init(frontend_filter: &str) {
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr)
        .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")));
    let file = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        // Events before the file opens (or if it can't) go nowhere.
        .with_writer(|| OptionalWriter::from(FILE.get()))
        .with_filter(
            EnvFilter::try_from_env("CATMAIL_LOG")
                .unwrap_or_else(|_| EnvFilter::new(format!("{ENGINE_FILTER},{frontend_filter}"))),
        );
    tracing_subscriber::registry()
        .with(stderr)
        .with(file)
        .init();
    log_panics();
}

/// Begins writing the file in `config`'s data directory for `frontend`, after a header
/// that names the build and the computer: what a report from a user needs read with it.
/// Does nothing while another process writes it.
pub fn write_file(config: &Config, frontend: &str) {
    let Some(file) = open_in(config, MAX_FILE_LEN) else {
        return;
    };
    if FILE.set(file).is_err() {
        return;
    }
    tracing::info!(
        frontend,
        version = env!("CARGO_PKG_VERSION"),
        commit = version::COMMIT,
        profile = env!("CATMAIL_BUILD_PROFILE"),
        target = env!("CATMAIL_BUILD_TARGET"),
        rustc = env!("CATMAIL_BUILD_RUSTC"),
        pid = std::process::id(),
        "started"
    );
    tracing::info!(
        os = system::os(),
        kernel = system::kernel(),
        session = system::session(),
        locale = system::locale(),
        cpus = std::thread::available_parallelism().map_or(0, |n| n.get()),
        // The file's times are UTC; a user tells theirs in local time.
        utc_offset = %jiff::Zoned::now().offset(),
        data_dir = %config.data_dir().display(),
        "running on"
    );
}

/// The file in `config`'s data directory, which is made and checked as the engine does:
/// the CLI starts the file before opening the engine, and someone else's directory could
/// have a link planted in it. `None` if another process writes the file, or the data
/// directory can't be had (opening the engine then says why).
fn open_in(config: &Config, max_len: u64) -> Option<LogFile> {
    crate::config::create_data_dir(config.data_dir()).ok()?;
    let dir = config.log_dir();
    LogFile::open(&dir, max_len, home())
        .inspect_err(|e| tracing::warn!(error = %e, dir = %dir.display(), "can't keep a log file"))
        .ok()?
}

/// The home directory with a separator after it, as it starts the paths the file is to
/// show as `~`. `None` if it is the root, which every path starts with.
fn home() -> Option<String> {
    let dirs = directories::BaseDirs::new()?;
    let home = dirs.home_dir();
    home.parent()?;
    Some(format!("{}{MAIN_SEPARATOR}", home.display()))
}

/// Writes a panic to the file before the default hook prints it: not through `tracing`,
/// whose own panic would come back here.
fn log_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(file) = FILE.get() {
            let backtrace = std::backtrace::Backtrace::force_capture();
            let thread = std::thread::current();
            let name = thread.name().unwrap_or("unnamed");
            let _ = file.write(format!("thread '{name}' {info}\n{backtrace}\n").as_bytes());
        }
        default(info);
    }));
}

/// [`FILE_NAME`], which once longer than its limit becomes [`OLD_FILE_NAME`] (replacing
/// the one before) and starts over. Written straight away, unbuffered: an event is one
/// write, and the last ones before a crash are those wanted most. The home directory is
/// shown as `~` wherever it comes up: logs get shared, and its path names the user.
struct LogFile {
    path: PathBuf,
    old: PathBuf,
    max_len: u64,
    home: Option<String>,
    current: Mutex<Current>,
    _lock: File,
}

struct Current {
    /// `None` if it couldn't be opened again after making way for a new one.
    file: Option<File>,
    len: u64,
}

impl LogFile {
    /// Continues the file in `dir`, creating both if need be (`dir`'s parent must exist).
    /// `None` if another process holds it.
    fn open(dir: &Path, max_len: u64, home: Option<String>) -> io::Result<Option<Self>> {
        catmail_store::create_private_dir(dir)?;
        let lock = private_file_options()
            .write(true)
            .truncate(false)
            .open(dir.join(LOCK_NAME))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(e),
        }
        let path = dir.join(FILE_NAME);
        let file = private_file_options().append(true).open(&path)?;
        let len = file.metadata()?.len();
        Ok(Some(Self {
            path,
            old: dir.join(OLD_FILE_NAME),
            max_len,
            home,
            current: Mutex::new(Current {
                file: Some(file),
                len,
            }),
            _lock: lock,
        }))
    }

    /// Writes all of `buf`, after making way for a new file if it would make this one too
    /// long.
    fn write(&self, buf: &[u8]) -> io::Result<usize> {
        let shown = self.show_home(buf);
        let bytes = shown.as_deref().unwrap_or(buf);
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if current.len > 0 && current.len + bytes.len() as u64 > self.max_len {
            // Closed first: Windows doesn't rename a file that is open.
            current.file = None;
            current.len = 0;
            // Started over in place if it couldn't be renamed (someone has it open).
            let _ = fs::rename(&self.path, &self.old);
            let file = private_file_options()
                .write(true)
                .truncate(true)
                .open(&self.path);
            current.file = file.ok();
        }
        if let Some(file) = current.file.as_mut() {
            file.write_all(bytes)?;
            current.len += bytes.len() as u64;
        }
        Ok(buf.len())
    }

    /// `buf` with the home directory as `~`, if it is in it.
    fn show_home(&self, buf: &[u8]) -> Option<Vec<u8>> {
        let home = self.home.as_deref()?;
        let text = std::str::from_utf8(buf).ok()?;
        text.contains(home).then(|| {
            text.replace(home, &format!("~{MAIN_SEPARATOR}"))
                .into_bytes()
        })
    }
}

impl io::Write for &LogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        LogFile::write(self, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: PathBuf) -> String {
        fs::read_to_string(path).unwrap()
    }

    fn open(dir: &Path, max_len: u64) -> LogFile {
        LogFile::open(dir, max_len, None).unwrap().unwrap()
    }

    #[test]
    fn a_long_file_makes_way_for_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        let file = open(&logs, 10);
        file.write(b"first\n").unwrap();
        file.write(b"second\n").unwrap();
        file.write(b"third\n").unwrap();
        assert_eq!(read(logs.join(OLD_FILE_NAME)), "second\n");
        assert_eq!(read(logs.join(FILE_NAME)), "third\n");
    }

    #[test]
    fn a_launch_continues_the_file_and_counts_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        open(dir.path(), 10).write(b"before\n").unwrap();
        open(dir.path(), 10).write(b"after\n").unwrap();
        assert_eq!(read(dir.path().join(OLD_FILE_NAME)), "before\n");
        assert_eq!(read(dir.path().join(FILE_NAME)), "after\n");
    }

    #[test]
    fn an_event_longer_than_the_limit_is_kept_whole() {
        let dir = tempfile::tempdir().unwrap();
        open(dir.path(), 4).write(b"longer than four\n").unwrap();
        assert_eq!(read(dir.path().join(FILE_NAME)), "longer than four\n");
    }

    #[test]
    fn a_new_data_directory_gets_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::with_data_dir(dir.path().join("new").join("data"));
        let file = open_in(&config, 10).unwrap();
        file.write(b"first\n").unwrap();
        assert_eq!(read(config.log_dir().join(FILE_NAME)), "first\n");
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_written_in_a_data_directory_of_another_user() {
        use std::os::unix::fs::MetadataExt as _;
        // Root's, and writable by anyone: without the check the file would go in it.
        let shared = Path::new("/tmp");
        let root_owned = fs::metadata(shared).is_ok_and(|m| m.uid() == 0);
        if rustix::process::geteuid().is_root() || !root_owned {
            return;
        }
        // As if someone planted a link to a directory of theirs in a shared location.
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("catmail");
        std::os::unix::fs::symlink(shared, &data).unwrap();
        assert!(open_in(&Config::with_data_dir(&data), 10).is_none());
    }

    #[test]
    fn one_process_at_a_time_writes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let writing = open(dir.path(), 10);
        assert!(LogFile::open(dir.path(), 10, None).unwrap().is_none());
        drop(writing);
        assert!(LogFile::open(dir.path(), 10, None).unwrap().is_some());
    }

    #[test]
    fn the_home_directory_shows_as_a_tilde() {
        let dir = tempfile::tempdir().unwrap();
        let home = format!("{MAIN_SEPARATOR}home{MAIN_SEPARATOR}someone{MAIN_SEPARATOR}");
        let file = LogFile::open(dir.path(), 100, Some(home.clone()))
            .unwrap()
            .unwrap();
        let line = format!("data_dir={home}.local and {home}x\n");
        assert_eq!(file.write(line.as_bytes()).unwrap(), line.len());
        let tilde = format!("~{MAIN_SEPARATOR}");
        assert_eq!(
            read(dir.path().join(FILE_NAME)),
            format!("data_dir={tilde}.local and {tilde}x\n")
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_the_user_can_read_the_log() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        open(&logs, 10);
        let mode = |path: PathBuf| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(logs.clone()), 0o700);
        assert_eq!(mode(logs.join(FILE_NAME)), 0o600);
    }
}
