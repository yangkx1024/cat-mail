//! The MCP server the app hosts for AI agents (`catmail_mcp`), and what Settings › Agents
//! says of it. The server runs on a thread of its own, on a current-thread tokio runtime
//! of its own: the toolkit's loop isn't tokio, and the client's futures don't need it.
//! It starts, moves to another port and stops as the settings say. See
//! `docs/architecture/mcp.md`.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::thread::JoinHandle;
use std::time::Duration;

use catmail_core::MailClient;
use catmail_mcp::{McpServer, ServeOptions};
use tokio::sync::mpsc;

use crate::commands::Confirm;
use crate::settings::McpSettings;

pub use catmail_mcp::{Access, LOG_FILTER, NewToken, Tier, Token, Tokens, connect};

/// How long turning the server off, or quitting, lets the requests being answered run
/// on: shorter than `catmail serve`'s, as someone is waiting on the app.
const DRAIN: Duration = Duration::from_secs(5);

/// Why there are no tokens, nor a server to present them to.
pub const NO_CONFIG_DIR: &str = "There is no config directory to keep the tokens in.";

/// Where the server is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Not listening: turned off, not started yet, or moving to another port.
    Off,
    Listening(SocketAddr),
    /// Why it isn't listening, as a sentence.
    Failed(String),
}

impl Status {
    /// Under the switch while it is on: where agents connect, or why they can't. Not
    /// listening yet, it is starting.
    pub fn line(&self) -> String {
        match self {
            Status::Listening(addr) => connect::local_url(addr.port()),
            Status::Failed(why) => why.clone(),
            Status::Off => "Starting…".to_owned(),
        }
    }
}

/// What the server listens with, if it is on.
fn serve_options(settings: &McpSettings) -> Option<ServeOptions> {
    settings.enabled.then(|| ServeOptions {
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, settings.port)),
        drain: DRAIN,
        ..ServeOptions::default()
    })
}

/// The server's thread, for as long as the app runs.
pub struct Host {
    /// What it should be listening with, the newest last: `None` for off.
    wants: mpsc::UnboundedSender<Option<ServeOptions>>,
    thread: JoinHandle<()>,
}

impl Host {
    /// Starts the thread, with the server off; [`want`](Self::want) turns it on.
    /// `status` hears where it is, on that thread.
    pub fn start(
        client: MailClient,
        tokens: Tokens,
        status: impl Fn(Status) + Send + 'static,
    ) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (wants, wanted) = mpsc::unbounded_channel();
        let thread = std::thread::Builder::new()
            .name("catmail-mcp".into())
            .spawn(move || runtime.block_on(follow(client, tokens, wanted, status)))?;
        Ok(Self { wants, thread })
    }

    /// The server as `settings` say: started, moved or stopped if they differ from what
    /// it listens with. Doesn't wait.
    pub fn want(&self, settings: &McpSettings) {
        // The thread ends only with the host.
        let _ = self.wants.send(serve_options(settings));
    }

    /// Stops the server, letting the requests being answered finish for a while, and
    /// returns once its thread has ended. Blocks.
    pub fn shut_down(self) {
        drop(self.wants);
        if self.thread.join().is_err() {
            tracing::warn!("the MCP server's thread panicked");
        }
    }
}

/// Runs the server as `wanted` says, until the host is gone.
async fn follow(
    client: MailClient,
    tokens: Tokens,
    mut wanted: mpsc::UnboundedReceiver<Option<ServeOptions>>,
    status: impl Fn(Status),
) {
    let mut running: Option<McpServer> = None;
    // What was asked last: asked again (by a change to another setting), it isn't tried
    // again.
    let mut current = None;
    while let Some(mut want) = wanted.recv().await {
        // Only the newest counts: earlier ones were asked while a server stopped.
        while let Ok(newer) = wanted.try_recv() {
            want = newer;
        }
        if want == current {
            continue;
        }
        current.clone_from(&want);
        if let Some(server) = running.take() {
            // Moving: no longer where it was.
            if want.is_some() {
                status(Status::Off);
            }
            server.stop().await;
        }
        let Some(options) = want else {
            status(Status::Off);
            continue;
        };
        let addr = options.addr;
        match McpServer::start(client.clone(), tokens.clone(), options).await {
            Ok(server) => {
                status(Status::Listening(server.local_addr()));
                running = Some(server);
            }
            Err(e) => {
                tracing::warn!(%addr, error = %e, "couldn't start the MCP server");
                status(Status::Failed(failed(addr, &e)));
            }
        }
    }
    if let Some(server) = running {
        server.stop().await;
    }
}

/// Why the server couldn't listen on `addr`.
fn failed(addr: SocketAddr, e: &io::Error) -> String {
    let port = addr.port();
    match e.kind() {
        io::ErrorKind::AddrInUse => format!("Port {port} is in use by another program."),
        io::ErrorKind::PermissionDenied => format!("Not allowed to listen on port {port}."),
        _ => format!("Couldn't listen on port {port}: {e}."),
    }
}

/// A tier, as the menu of a new token and the row of one name it.
pub fn tier_label(tier: Tier) -> &'static str {
    match tier {
        Tier::Read => "Read only",
        Tier::Organize => "Read and organize",
        Tier::Draft => "Read, organize and write drafts",
        Tier::Send => "Also send, asking first",
        Tier::SendUnasked => "Also send, without asking",
    }
}

/// The accounts a token reaches, by address; every account with `None`.
pub fn accounts_label(accounts: Option<&[String]>) -> String {
    match accounts {
        None => "All accounts".to_owned(),
        Some(list) => list.join(", "),
    }
}

/// Under a token's name: what it may do, with which accounts, and `made`, when it was.
pub fn token_line(access: &Access, made: &str) -> String {
    format!(
        "{} · {} · {made}",
        tier_label(access.tier),
        accounts_label(access.accounts.as_deref())
    )
}

/// Before taking the token named `name` away.
pub fn remove_token(name: &str) -> Confirm {
    Confirm {
        title: format!("Remove the token “{name}”?"),
        description: "Agents using it are turned away at once.".into(),
        action: "Remove",
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::mpsc as std_mpsc;

    use catmail_core::{Config, MemorySecrets};

    use super::*;

    fn on(port: u16) -> McpSettings {
        McpSettings {
            enabled: true,
            port,
        }
    }

    /// A host on an engine without accounts, and what it says, as it says it.
    struct Fixture {
        host: Host,
        statuses: std_mpsc::Receiver<Status>,
        _dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let client = MailClient::builder(Config::with_data_dir(dir.path().join("data")))
                .secrets(Arc::new(MemorySecrets::default()))
                .open()
                .unwrap();
            let tokens = Tokens::new(dir.path().join("config"));
            let (said, statuses) = std_mpsc::channel();
            let host = Host::start(client, tokens, move |status| {
                let _ = said.send(status);
            })
            .unwrap();
            Self {
                host,
                statuses,
                _dir: dir,
            }
        }

        fn next(&self) -> Status {
            self.statuses
                .recv_timeout(Duration::from_secs(10))
                .expect("the host says where the server is")
        }

        fn listening(&self) -> SocketAddr {
            match self.next() {
                Status::Listening(addr) => addr,
                other => panic!("not listening: {other:?}"),
            }
        }
    }

    fn reachable(addr: SocketAddr) -> bool {
        std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok()
    }

    #[test]
    fn follows_the_settings() {
        let fixture = Fixture::new();
        // Port 0: any free one.
        fixture.host.want(&on(0));
        let first = fixture.listening();
        assert!(reachable(first));
        // Another setting changed: the server is left alone.
        fixture.host.want(&on(0));
        // Elsewhere: the first stops before the next starts.
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        fixture.host.want(&on(port));
        assert_eq!(fixture.next(), Status::Off);
        let second = fixture.listening();
        assert_eq!(second.port(), port);
        assert!(!reachable(first));
        // Off: said once the port is free.
        fixture.host.want(&McpSettings::default());
        assert_eq!(fixture.next(), Status::Off);
        assert!(!reachable(second));
        fixture.host.shut_down();
    }

    #[test]
    fn says_why_it_cant_listen() {
        let fixture = Fixture::new();
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        fixture.host.want(&on(port));
        assert_eq!(
            fixture.next(),
            Status::Failed(format!("Port {port} is in use by another program."))
        );
        // Turned off and on again once the port is free, it is tried again.
        drop(taken);
        fixture.host.want(&McpSettings::default());
        assert_eq!(fixture.next(), Status::Off);
        fixture.host.want(&on(port));
        assert_eq!(fixture.listening().port(), port);
        fixture.host.shut_down();
    }

    #[test]
    fn shutting_down_stops_the_server() {
        let fixture = Fixture::new();
        fixture.host.want(&on(0));
        let addr = fixture.listening();
        fixture.host.shut_down();
        assert!(!reachable(addr));
    }

    #[test]
    fn options_follow_the_settings() {
        assert_eq!(serve_options(&McpSettings::default()), None);
        let options = serve_options(&McpSettings {
            enabled: true,
            port: 9000,
        })
        .unwrap();
        assert_eq!(options.addr, SocketAddr::from(([127, 0, 0, 1], 9000)));
    }

    #[test]
    fn the_line_under_the_switch() {
        let addr = SocketAddr::from(([127, 0, 0, 1], 9000));
        assert_eq!(Status::Listening(addr).line(), "http://127.0.0.1:9000/mcp");
        assert_eq!(Status::Off.line(), "Starting…");
        assert_eq!(Status::Failed("Why.".into()).line(), "Why.");
    }

    #[test]
    fn what_a_token_may_do() {
        let all = Access::default();
        assert_eq!(
            token_line(&all, "Today"),
            "Read only · All accounts · Today"
        );
        let some = Access {
            tier: Tier::Organize,
            accounts: Some(vec!["a@example.com".into(), "b@example.com".into()]),
        };
        assert_eq!(
            token_line(&some, "Today"),
            "Read and organize · a@example.com, b@example.com · Today"
        );
    }
}
