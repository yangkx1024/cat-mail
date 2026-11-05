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
    /// TLS from the first byte was asked for, and the server on `port` answered in
    /// plaintext: it wants STARTTLS (as on 587 and 143), or isn't a mail server.
    NotTls {
        port: u16,
    },
}

impl From<io::Error> for NetError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// An open connection to `server`, already in TLS for [`Security::Tls`]; a STARTTLS
/// connection is upgraded later with [`start_tls`] and the returned name. TCP keepalive
/// is on.
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
    configure(&tcp)?;
    let name = ServerName::try_from(server.host.clone())
        .map_err(|_| NetError::InvalidServerName(server.host.clone()))?;
    let stream: Box<dyn Stream> = match server.security {
        Security::Tls => Box::new(start_tls(tcp, &name, tls, timeout).await.map_err(
            |e| match e {
                NetError::Io(e) if answered_in_plaintext(&e) => {
                    NetError::NotTls { port: server.port }
                }
                e => e,
            },
        )?),
        Security::StartTls | Security::Plain => Box::new(tcp),
    };
    Ok((stream, name))
}

/// Whether the TLS handshake failed on the server's first bytes not being a TLS record:
/// a plaintext greeting (`220 …`, `* OK …`). Said as such, it tells what to change in the
/// settings; rustls' own words ("received corrupt message of type InvalidContentType")
/// don't.
fn answered_in_plaintext(e: &io::Error) -> bool {
    e.get_ref()
        .and_then(|e| e.downcast_ref::<rustls::Error>())
        .is_some_and(|e| {
            matches!(
                e,
                rustls::Error::InvalidMessage(rustls::InvalidMessage::InvalidContentType)
            )
        })
}

fn configure(tcp: &TcpStream) -> io::Result<()> {
    tcp.set_nodelay(true)?;
    // A NAT box that forgets a quiet connection drops it without a word. Probes find
    // that out in a few minutes, rather than at the next command: an IDLE lasts 10.
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(15));
    socket2::SockRef::from(tcp).set_tcp_keepalive(&keepalive)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that greets in plaintext, as on the STARTTLS ports.
    async fn plaintext_server(greeting: &'static [u8]) -> u16 {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            tcp.write_all(greeting).await.unwrap();
            // Until the client gives up on the handshake.
            let mut buf = [0; 1024];
            while tcp.read(&mut buf).await.is_ok_and(|n| n > 0) {}
        });
        port
    }

    #[tokio::test]
    async fn tls_to_a_plaintext_port_says_so() {
        for greeting in [
            &b"220 smtp.example.com ESMTP\r\n"[..],
            b"* OK IMAP4rev1 ready\r\n",
        ] {
            let port = plaintext_server(greeting).await;
            let server = ServerConfig {
                host: "localhost".into(),
                port,
                security: Security::Tls,
            };
            let tls = TlsConfig::with_roots_only([]).unwrap();
            let Err(e) = connect(&server, &tls, Duration::from_secs(5)).await else {
                panic!("connected to a plaintext server in TLS");
            };
            assert!(
                matches!(e, NetError::NotTls { port: p } if p == port),
                "{e:?}"
            );
        }
    }

    #[tokio::test]
    async fn connections_are_kept_alive() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tcp = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        configure(&tcp).unwrap();
        assert!(socket2::SockRef::from(&tcp).keepalive().unwrap());
    }
}
