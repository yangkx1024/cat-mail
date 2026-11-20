//! The Streamable HTTP endpoint, `/mcp`, on a local address. Every request carries a
//! token from the token file, the local ones too: a request through `tailscale serve`
//! or a tunnel comes from this machine as well. Encryption and reaching it from other
//! devices are the tunnel's. A web page can't get in either, DNS rebinding or not: it has
//! no token, and rmcp turns away every request with an `Origin`, which a browser sends
//! with every POST. So `Host` isn't checked: the name a tunnel forwards as (`tailscale
//! serve` keeps the `*.ts.net` one) would have to be configured for nothing.
//!
//! Files for drafts are put to `/mcp/uploads/<handle>` (see [`crate::uploads`]): the
//! handle in the URL stands in for the token there.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future as _;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use catmail_core::MailClient;
use http::{HeaderMap, Method, Request, Response, StatusCode, header};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Body as _, Bytes, Frame, Incoming, SizeHint};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::limits::TokenLimits;
use crate::tokens::{Tier, Tokens};
use crate::tools::Mail;
use crate::uploads::{self, PutRefused, Uploads};

/// The port `catmail serve` listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 8642;
pub(crate) const PATH: &str = "/mcp";
/// Enough for any tool's arguments, attachments given to a draft too (base64, at most
/// [`uploads::MAX_FILE_BYTES`]); rmcp answers 413 above it. Read only once the token is
/// checked.
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// Enough for the arguments of every tool but those given files, which only a token of
/// the draft tier calls: other tokens can't have that much held in memory.
const MAX_SMALL_REQUEST_BYTES: usize = 1024 * 1024;
/// How long stopping waits for the requests being answered by default.
const DRAIN: Duration = Duration::from_secs(30);
/// How long a client may take to send a request's headers: hyper's default, which takes
/// effect only with a timer.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a request's body may go without any of it arriving, as nginx's
/// `client_body_timeout`: a client that stops sending doesn't keep what was buffered.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(60);
/// How many requests to `/mcp` a token may have being answered at once: each may buffer
/// its whole body (16 MB for a draft token) while it arrives, however slowly. Its
/// uploads are as many at most, one for each it may prepare.
const REQUESTS_PER_TOKEN: usize = 16;

type Service = StreamableHttpService<Mail, NeverSessionManager>;
type Body = BoxBody<Bytes, Infallible>;
type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeOptions {
    /// Where to listen: this machine only, by default.
    pub addr: SocketAddr,
    /// How long stopping waits for the requests being answered (a sync, a download)
    /// before it cuts them off, and the connections with them.
    pub drain: Duration,
    pub limits: TokenLimits,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_PORT)),
            drain: DRAIN,
            limits: TokenLimits::default(),
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
        let uploads = Arc::new(Uploads::default());
        // rmcp makes a handler for every request; they share one client and router.
        let mail = Mail::new(client, options.limits, uploads.clone());
        let service: Arc<Service> = Arc::new(StreamableHttpService::new(
            move || Ok(mail.clone()),
            Arc::default(),
            config,
        ));
        let task = tokio::spawn(accept(
            listener,
            Routes {
                service,
                tokens,
                uploads,
                in_flight: Arc::default(),
                addr,
            },
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

/// What answers requests: rmcp's service those to `/mcp` with a token, the uploads
/// those to their URLs.
#[derive(Clone)]
struct Routes {
    service: Arc<Service>,
    tokens: Tokens,
    uploads: Arc<Uploads>,
    in_flight: Arc<InFlight>,
    /// Where the server listens.
    addr: SocketAddr,
}

/// Where a request came to, as its client reached it: `<scheme>://<host>`, for the URLs
/// a tool gives out (`prepare_upload`), which the agent's shell then reaches as its
/// client did. Among the request's extensions, as its token is.
#[derive(Clone)]
pub(crate) struct BaseUrl(pub String);

/// How [`accept`] stops: `stop` ends listening and lets connections finish, then
/// `handlers` (rmcp's) is cancelled, within `drain`.
struct Stopping {
    stop: CancellationToken,
    handlers: CancellationToken,
    drain: Duration,
}

async fn accept(listener: TcpListener, routes: Routes, stopping: Stopping) {
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
        let routes = routes.clone();
        let stop = stop.clone();
        connections.spawn(async move {
            let handler = service_fn(move |request| {
                let routes = routes.clone();
                async move { Ok::<_, Infallible>(respond(request, routes).await) }
            });
            let connection = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_READ_TIMEOUT)
                .serve_connection(TokioIo::new(stream), handler);
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

async fn respond(request: Request<Incoming>, routes: Routes) -> Response<Body> {
    let Routes {
        service,
        tokens,
        uploads,
        in_flight,
        addr,
    } = routes;
    let mut request = request.map(ReadTimeout::new);
    if let Some(handle) = request.uri().path().strip_prefix(uploads::PATH) {
        let handle = handle.to_owned();
        return upload(request, &handle, &uploads).await;
    }
    if request.uri().path() != PATH {
        return text(StatusCode::NOT_FOUND, "not found; the endpoint is /mcp");
    }
    let Some(secret) = bearer(&request) else {
        return unauthorized("an MCP token is required: Authorization: Bearer <token>");
    };
    // The file is small, but reading it is still blocking IO.
    let verified = tokio::task::spawn_blocking(move || tokens.verify(&secret)).await;
    match verified {
        Ok(Ok(Some(token))) => {
            let Some(_answering) = in_flight.enter(&token.name) else {
                tracing::warn!(token = %token.name, "MCP request turned away: too many at once");
                return text(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "this token has {REQUESTS_PER_TOKEN} requests being answered already; \
                         wait for one to end\n"
                    ),
                );
            };
            tracing::debug!(token = %token.name, method = %request.method(), "MCP request");
            let small = token.access.tier < Tier::Draft;
            // For the tools: rmcp hands them the request's parts (`tools::token_of`).
            let base = base_url(request.headers(), addr);
            request.extensions_mut().insert(token);
            request.extensions_mut().insert(base);
            if small {
                let request = request.map(|body| Limited::new(body, MAX_SMALL_REQUEST_BYTES));
                service.handle(request).await
            } else {
                service.handle(request).await
            }
        }
        Ok(Ok(None)) => {
            tracing::warn!("MCP request with an unknown token turned away");
            unauthorized("unknown MCP token")
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "can't read the MCP tokens");
            text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "can't read the MCP tokens",
            )
        }
        Err(e) => {
            tracing::error!(error = %e, "checking an MCP token failed");
            text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "checking the token failed",
            )
        }
    }
}

/// Keeps the file put to an upload's URL: the body as it is. The handle is what lets it
/// in, not a token: the agent's shell has none. Like `/mcp`, no web page may.
async fn upload(
    request: Request<ReadTimeout<Incoming>>,
    handle: &str,
    uploads: &Uploads,
) -> Response<Body> {
    if request.method() != Method::PUT && request.method() != Method::POST {
        let mut response = text(
            StatusCode::METHOD_NOT_ALLOWED,
            "put the file's bytes here: curl -T <file> <this URL>",
        );
        response
            .headers_mut()
            .insert(header::ALLOW, header::HeaderValue::from_static("PUT, POST"));
        return response;
    }
    if request.headers().contains_key(header::ORIGIN) {
        return text(StatusCode::FORBIDDEN, "uploads aren't for web pages");
    }
    // `Content-Length`, which `curl -T` sends: the put holds only that much of the
    // token's room while it lasts, not a whole file's.
    let declared = request
        .body()
        .size_hint()
        .exact()
        .map(|size| usize::try_from(size).unwrap_or(usize::MAX));
    let receiving = match uploads.begin(handle, declared, Instant::now()) {
        Ok(receiving) => receiving,
        Err(refused) => return refused_upload(refused),
    };
    // Dropping `receiving` without putting the file, below or when the request is cut
    // off, lets it be put again. A body that hasn't arrived by the time the upload
    // expires is given up, so it can't hold its room and slot on.
    let body = Limited::new(request.into_body(), receiving.limit).collect();
    let content = match tokio::time::timeout_at(receiving.until.into(), body).await {
        Ok(Ok(body)) => body.to_bytes(),
        Ok(Err(e)) if e.downcast_ref::<LengthLimitError>().is_some() => {
            return refused_upload(receiving.too_large());
        }
        Ok(Err(e)) if e.downcast_ref::<BodyTimedOut>().is_some() => {
            return text(
                StatusCode::REQUEST_TIMEOUT,
                format!("{e}; put the file again\n"),
            );
        }
        Ok(Err(e)) => {
            // The connection broke off: no one to answer.
            tracing::debug!(error = %e, "MCP upload broke off");
            return text(StatusCode::BAD_REQUEST, "the file didn't arrive whole");
        }
        Err(_) => {
            return text(
                StatusCode::REQUEST_TIMEOUT,
                "the upload expired before the file arrived whole; prepare_upload gives \
                 another\n",
            );
        }
    };
    let size = content.len();
    let token = match receiving.put(content) {
        Ok(token) => token,
        Err(refused) => return refused_upload(refused),
    };
    // The start of the handle, which is what lets the file in.
    let upload = handle.get(..8).unwrap_or(handle);
    tracing::info!(%token, %upload, size, "MCP upload received");
    text(
        StatusCode::OK,
        format!(
            "received {}; attach it with {{\"name\": \"<file name>\", \"upload\": \"{handle}\"}}\n",
            catmail_core::display::size(size as u64)
        ),
    )
}

fn refused_upload(refused: PutRefused) -> Response<Body> {
    match refused {
        PutRefused::Unknown => text(
            StatusCode::NOT_FOUND,
            "no such upload: it expired, was used, or the host restarted; \
             prepare_upload gives another\n",
        ),
        PutRefused::Taken => text(
            StatusCode::CONFLICT,
            "a file was put to this upload already; prepare_upload gives another\n",
        ),
        PutRefused::TooLarge => text(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "a file can't be larger than {}\n",
                catmail_core::display::size(uploads::MAX_FILE_BYTES as u64)
            ),
        ),
        PutRefused::Full { room } => text(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "this token's uploads not used yet, and those being put, hold all but {}; \
                 attach them, or wait until they are put or expire\n",
                catmail_core::display::size(room as u64)
            ),
        ),
    }
}

/// The requests to `/mcp` being answered, by the name of their token.
#[derive(Default)]
struct InFlight(Mutex<HashMap<String, usize>>);

impl InFlight {
    /// Counts a request of `token` while the guard lasts, unless the token has
    /// [`REQUESTS_PER_TOKEN`] being answered already.
    fn enter(&self, token: &str) -> Option<Answering<'_>> {
        let mut counts = self.0.lock().expect("requests in flight");
        let count = counts.entry(token.to_owned()).or_default();
        if *count >= REQUESTS_PER_TOKEN {
            return None;
        }
        *count += 1;
        Some(Answering {
            in_flight: self,
            token: token.to_owned(),
        })
    }
}

/// A request to `/mcp` being answered: counted until it is dropped, with the response
/// made or the request cut off.
struct Answering<'a> {
    in_flight: &'a InFlight,
    token: String,
}

impl Drop for Answering<'_> {
    fn drop(&mut self) {
        // Not `expect`: this may run while a panic unwinds.
        if let Ok(mut counts) = self.in_flight.0.lock()
            && let Some(count) = counts.get_mut(&self.token)
        {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.token);
            }
        }
    }
}

/// A request's body that fails once none of it arrived for [`BODY_READ_TIMEOUT`]: the
/// time between pieces, not the whole, so a large body over a slow link still arrives.
struct ReadTimeout<B> {
    body: B,
    timer: Pin<Box<tokio::time::Sleep>>,
}

/// Why a body ended: none of it arrived for [`BODY_READ_TIMEOUT`].
#[derive(Debug, thiserror::Error)]
#[error("nothing more of the request arrived for {} s", BODY_READ_TIMEOUT.as_secs())]
struct BodyTimedOut;

impl<B> ReadTimeout<B> {
    fn new(body: B) -> Self {
        Self {
            body,
            timer: Box::pin(tokio::time::sleep(BODY_READ_TIMEOUT)),
        }
    }
}

impl<B> hyper::body::Body for ReadTimeout<B>
where
    B: hyper::body::Body + Unpin,
    B::Error: Into<BoxError>,
{
    type Data = B::Data;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<B::Data>, BoxError>>> {
        let this = &mut *self;
        if let Poll::Ready(frame) = Pin::new(&mut this.body).poll_frame(cx) {
            let next = tokio::time::Instant::now() + BODY_READ_TIMEOUT;
            this.timer.as_mut().reset(next);
            return Poll::Ready(frame.map(|frame| frame.map_err(Into::into)));
        }
        match this.timer.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Some(Err(BodyTimedOut.into()))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

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

/// The request's [`BaseUrl`]: the name a proxy says it was asked for (`X-Forwarded-Host`),
/// else `Host`, else where the server listens. The scheme is the one the proxy says
/// (`X-Forwarded-Proto`); without one, `http` on this machine and `https` on any other
/// name, as only a proxy brings one (`tailscale serve`, a tunnel), and those serve HTTPS.
fn base_url(headers: &HeaderMap, addr: SocketAddr) -> BaseUrl {
    let header = |name: &str| {
        let value = headers.get(name)?.to_str().ok()?;
        // A list when it passed several proxies: the first is the client's.
        let first = value.split(',').next()?.trim();
        (!first.is_empty()).then_some(first)
    };
    let host = header("x-forwarded-host")
        .or_else(|| header(header::HOST.as_str()))
        .filter(|host| is_host(host))
        .map_or_else(|| addr.to_string(), str::to_owned);
    let scheme = match header("x-forwarded-proto") {
        Some(proto) if proto.eq_ignore_ascii_case("https") => "https",
        Some(proto) if proto.eq_ignore_ascii_case("http") => "http",
        _ if is_local(&host) => "http",
        _ => "https",
    };
    BaseUrl(format!("{scheme}://{host}"))
}

/// Whether `host` is a name or address with an optional port, and nothing else that
/// would change what a URL on it says.
fn is_host(host: &str) -> bool {
    host.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-._:[]".contains(&b))
}

/// Whether `host` (with a port, maybe) is this machine.
fn is_local(host: &str) -> bool {
    let name = match host.rsplit_once(':') {
        // `[::1]:8642`, or a name or IPv4 address with a port.
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) && !name.ends_with(':') => {
            name
        }
        _ => host,
    };
    let name = name.trim_start_matches('[').trim_end_matches(']');
    name.eq_ignore_ascii_case("localhost")
        || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn text(status: StatusCode, body: impl Into<Bytes>) -> Response<Body> {
    let mut response = Response::new(Full::new(body.into()).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn unauthorized(message: &'static str) -> Response<Body> {
    let mut response = text(StatusCode::UNAUTHORIZED, message);
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

    #[test]
    fn the_base_url_is_the_address_the_client_used() {
        let addr = SocketAddr::from(([127, 0, 0, 1], 8642));
        let url_of = |pairs: &[(&'static str, &str)]| {
            let mut headers = HeaderMap::new();
            for (name, value) in pairs {
                headers.insert(*name, value.parse().unwrap());
            }
            base_url(&headers, addr).0
        };
        assert_eq!(
            url_of(&[("host", "localhost:8642")]),
            "http://localhost:8642"
        );
        assert_eq!(url_of(&[("host", "[::1]:8642")]), "http://[::1]:8642");
        // Through `tailscale serve`, which keeps the name, and says the scheme or not.
        assert_eq!(
            url_of(&[("host", "mac.tailnet.ts.net")]),
            "https://mac.tailnet.ts.net"
        );
        assert_eq!(
            url_of(&[
                ("host", "127.0.0.1:8642"),
                ("x-forwarded-host", "mac.tailnet.ts.net, proxy.internal"),
                ("x-forwarded-proto", "http"),
            ]),
            "http://mac.tailnet.ts.net"
        );
        // Nothing that would take a URL elsewhere.
        assert_eq!(
            url_of(&[("host", "evil.example/x?")]),
            "http://127.0.0.1:8642"
        );
        assert_eq!(url_of(&[]), "http://127.0.0.1:8642");
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_times_out_only_once_none_of_it_arrives_for_long() {
        use futures_util::StreamExt as _;
        use http_body_util::StreamBody;

        let piece = || Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"ab")));
        // Longer in all than the timeout, but never that long between pieces.
        let slow = futures_util::stream::unfold(0, |n| async move {
            (n < 3).then_some(())?;
            tokio::time::sleep(BODY_READ_TIMEOUT - Duration::from_secs(1)).await;
            Some((piece(), n + 1))
        });
        let body = ReadTimeout::new(StreamBody::new(Box::pin(slow)));
        assert_eq!(body.collect().await.unwrap().to_bytes(), "ababab");

        let stalled = futures_util::stream::iter([piece()]).chain(futures_util::stream::pending());
        let body = ReadTimeout::new(StreamBody::new(Box::pin(stalled)));
        let error = body.collect().await.err().unwrap();
        assert!(error.is::<BodyTimedOut>(), "{error}");
    }

    #[test]
    fn a_token_has_only_so_many_requests_answered_at_once() {
        let in_flight = InFlight::default();
        let answering: Vec<_> = (0..REQUESTS_PER_TOKEN)
            .map(|_| in_flight.enter("laptop").unwrap())
            .collect();
        assert!(in_flight.enter("laptop").is_none());
        assert!(in_flight.enter("phone").is_some());
        drop(answering);
        assert!(in_flight.enter("laptop").is_some());
        assert!(in_flight.0.lock().unwrap().is_empty());
    }
}
