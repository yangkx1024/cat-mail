//! The Streamable HTTP endpoint, `/mcp`, on a local address. Every request carries a
//! token from the token file, the local ones too: a request through `tailscale serve`
//! or a tunnel comes from this machine as well. Encryption and reaching it from other
//! devices are the tunnel's. A web page can't get in either, DNS rebinding or not: it has
//! no token, and rmcp turns away every request with an `Origin`, which a browser sends
//! with every POST. So `Host` isn't checked: the name a tunnel forwards as (`tailscale
//! serve` keeps the `*.ts.net` one) would have to be configured for nothing.

use std::convert::Infallible;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use catmail_core::MailClient;
use http::{Request, Response, StatusCode, header};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::tokens::Tokens;
use crate::tools::Mail;

/// The port `catmail serve` listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 8642;
/// Where on the address agents reach the server.
pub(crate) const PATH: &str = "/mcp";
/// Enough for any tool's arguments; rmcp answers 413 above it.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// How long stopping waits for the requests being answered by default.
const DRAIN: Duration = Duration::from_secs(30);

type Service = StreamableHttpService<Mail, NeverSessionManager>;
type Body = BoxBody<Bytes, Infallible>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeOptions {
    /// Where to listen: this machine only, by default.
    pub addr: SocketAddr,
    /// How long stopping waits for the requests being answered (a sync, a download)
    /// before it cuts them off, and the connections with them.
    pub drain: Duration,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_PORT)),
            drain: DRAIN,
        }
    }
}

/// The running server. Dropping it stops it too, without waiting.
pub struct McpServer {
    addr: SocketAddr,
    stop: DropGuard,
    task: JoinHandle<()>,
}

impl McpServer {
    /// Listens and serves on the current tokio runtime, which must stay up meanwhile:
    /// `catmail serve`'s, or one of the app's own on a thread of its own (the
    /// toolkit's loop isn't tokio). Fails if the address can't be bound.
    pub async fn start(
        client: MailClient,
        tokens: Tokens,
        options: ServeOptions,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(options.addr).await?;
        let addr = listener.local_addr()?;
        let stop = CancellationToken::new();
        let handlers = CancellationToken::new();
        // No sessions: each request stands alone (as in MCP 2026-07-28), so a restart or
        // another tunnel in between loses nothing, and answers are plain JSON.
        let config = StreamableHttpServerConfig::default()
            // Any name: the token and `Origin` keep web pages out (see above).
            .disable_allowed_hosts()
            // Agents aren't web pages: a request from a browser page is turned away.
            .enforce_origin_validation()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_max_request_body_bytes(MAX_REQUEST_BYTES)
            // Not `stop`'s child: rmcp gives up on a response the moment its token is
            // cancelled, so it is cancelled only once the requests had their chance to end.
            .with_cancellation_token(handlers.clone());
        // rmcp makes a handler for every request; they share one client and router.
        let mail = Mail::new(client);
        let service: Arc<Service> = Arc::new(StreamableHttpService::new(
            move || Ok(mail.clone()),
            Arc::default(),
            config,
        ));
        let task = tokio::spawn(accept(
            listener,
            service,
            tokens,
            Stopping {
                stop: stop.clone(),
                handlers,
                drain: options.drain,
            },
        ));
        tracing::info!(%addr, "MCP server listening");
        Ok(Self {
            addr,
            stop: stop.drop_guard(),
            task,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stops listening, lets the requests being answered finish, and returns after.
    pub async fn stop(self) {
        drop(self.stop);
        let _ = self.task.await;
    }
}

/// How [`accept`] stops: `stop` ends listening and lets connections finish, then
/// `handlers` (rmcp's) is cancelled, within `drain`.
struct Stopping {
    stop: CancellationToken,
    handlers: CancellationToken,
    drain: Duration,
}

async fn accept(listener: TcpListener, service: Arc<Service>, tokens: Tokens, stopping: Stopping) {
    let Stopping {
        stop,
        handlers,
        drain,
    } = stopping;
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                // Out of file descriptors and the like: try again shortly, not in a spin.
                Err(e) => {
                    tracing::warn!(error = %e, "MCP server can't accept a connection");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = stop.cancelled() => break,
        };
        let service = service.clone();
        let tokens = tokens.clone();
        let stop = stop.clone();
        connections.spawn(async move {
            let handler = service_fn(move |request| {
                let service = service.clone();
                let tokens = tokens.clone();
                async move { Ok::<_, Infallible>(respond(request, &service, tokens).await) }
            });
            let connection = http1::Builder::new().serve_connection(TokioIo::new(stream), handler);
            let mut connection = std::pin::pin!(connection);
            let result = tokio::select! {
                result = connection.as_mut() => result,
                () = stop.cancelled() => {
                    connection.as_mut().graceful_shutdown();
                    connection.await
                }
            };
            if let Err(e) = result {
                tracing::debug!(error = %e, "MCP connection ended");
            }
        });
        // Reap the finished ones as we go.
        while connections.try_join_next().is_some() {}
    }
    drop(listener);
    // Each connection answers what it is answering, then closes.
    let drained = async { while connections.join_next().await.is_some() {} };
    if tokio::time::timeout(drain, drained).await.is_err() {
        // Waiting on longer may never end: a client can leave its request's body
        // unfinished, which neither rmcp's token nor hyper's graceful shutdown ends.
        tracing::warn!("MCP requests still running after {drain:?}; cutting them off");
        connections.abort_all();
    }
    handlers.cancel();
    while connections.join_next().await.is_some() {}
    tracing::info!("MCP server stopped");
}

async fn respond(request: Request<Incoming>, service: &Service, tokens: Tokens) -> Response<Body> {
    if request.uri().path() != PATH {
        return plain(StatusCode::NOT_FOUND, "not found; the endpoint is /mcp");
    }
    let Some(secret) = bearer(&request) else {
        return unauthorized("an MCP token is required: Authorization: Bearer <token>");
    };
    // The file is small, but reading it is still blocking IO.
    let verified = tokio::task::spawn_blocking(move || tokens.verify(&secret)).await;
    match verified {
        Ok(Ok(Some(token))) => {
            tracing::debug!(token = %token.name, method = %request.method(), "MCP request");
            service.handle(request).await
        }
        Ok(Ok(None)) => {
            tracing::warn!("MCP request with an unknown token turned away");
            unauthorized("unknown MCP token")
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "can't read the MCP tokens");
            plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "can't read the MCP tokens",
            )
        }
        Err(e) => {
            tracing::error!(error = %e, "checking an MCP token failed");
            plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "checking the token failed",
            )
        }
    }
}

/// The token of an `Authorization: Bearer <token>` header.
fn bearer<B>(request: &Request<B>) -> Option<String> {
    let value = request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then(|| token.to_owned())
}

fn plain(status: StatusCode, text: &'static str) -> Response<Body> {
    let mut response = Response::new(Full::new(Bytes::from_static(text.as_bytes())).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn unauthorized(text: &'static str) -> Response<Body> {
    let mut response = plain(StatusCode::UNAUTHORIZED, text);
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        header::HeaderValue::from_static("Bearer"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(authorization: Option<&str>) -> Request<()> {
        let mut builder = Request::builder().uri("/mcp");
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        builder.body(()).unwrap()
    }

    #[test]
    fn bearer_reads_the_token() {
        assert_eq!(bearer(&request(Some("Bearer abc"))).as_deref(), Some("abc"));
        assert_eq!(
            bearer(&request(Some("bearer  abc "))).as_deref(),
            Some("abc")
        );
        assert_eq!(bearer(&request(Some("Basic abc"))), None);
        assert_eq!(bearer(&request(Some("Bearer "))), None);
        assert_eq!(bearer(&request(None)), None);
    }
}
