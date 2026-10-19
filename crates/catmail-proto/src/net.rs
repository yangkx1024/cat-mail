//! What IMAP and SMTP connections share below the protocol: opening the TCP connection,
//! TLS (from the start or after STARTTLS), and timeouts on every read and write.

use std::io;
use std::time::Duration;

use catmail_types::{Security, ServerConfig};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;

use crate::TlsConfig;

/// A byte stream a connection can run over: TCP, TLS, or an in-memory pipe.
pub(crate) trait Stream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Stream for T {}

/// Failures below the protocol; each protocol's error type converts from it.
#[derive(Debug)]
pub(crate) enum NetError {
    Io(io::Error),
    Timeout,
    InvalidServerName(String),
}

impl From<io::Error> for NetError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// An open connection to `server`, already in TLS for [`Security::Tls`]; a STARTTLS
/// connection is upgraded later with [`start_tls`] and the returned name.
pub(crate) async fn connect(
    server: &ServerConfig,
    tls: &TlsConfig,
    timeout: Duration,
) -> Result<(Box<dyn Stream>, ServerName<'static>), NetError> {
    let tcp = with_timeout(
        timeout,
        TcpStream::connect((server.host.as_str(), server.port)),
    )
    .await?;
    tcp.set_nodelay(true)?;
    let name = ServerName::try_from(server.host.clone())
        .map_err(|_| NetError::InvalidServerName(server.host.clone()))?;
    let stream: Box<dyn Stream> = match server.security {
        Security::Tls => Box::new(start_tls(tcp, &name, tls, timeout).await?),
        Security::StartTls | Security::Plain => Box::new(tcp),
    };
    Ok((stream, name))
}

/// Wraps `stream` in TLS, verifying the server as `name`.
pub(crate) async fn start_tls<S: Stream>(
    stream: S,
    name: &ServerName<'static>,
    tls: &TlsConfig,
    timeout: Duration,
) -> Result<tokio_rustls::client::TlsStream<S>, NetError> {
    with_timeout(timeout, tls.connector().connect(name.clone(), stream)).await
}

pub(crate) async fn with_timeout<T>(
    timeout: Duration,
    fut: impl Future<Output = io::Result<T>>,
) -> Result<T, NetError> {
    match tokio::time::timeout(timeout, fut).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(NetError::Timeout),
    }
}
