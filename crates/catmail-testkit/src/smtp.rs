use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::TestCert;
use crate::imap::LoggedCommand;

/// A message the fake server accepted (or took in before dropping the connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedMessage {
    pub from: String,
    pub recipients: Vec<String>,
    /// As sent, with dot-stuffing undone.
    pub data: Vec<u8>,
}

/// Deviations from a well-behaved submission server.
#[derive(Debug, Clone)]
pub struct SmtpFaults {
    /// Greet this many connections with `421` and close them, as a server that is down
    /// for maintenance does.
    pub unavailable: usize,
    /// Offered after EHLO; empty means no AUTH at all.
    pub auth_mechanisms: Vec<&'static str>,
    /// Reject every login (535).
    pub reject_login: bool,
    /// Advertise `SIZE <n>` and refuse bigger messages.
    pub size_limit: Option<u64>,
    /// RCPT TO for these addresses gets this reply code, e.g. 550 or 450.
    pub reject_recipients: Vec<(String, u16)>,
    /// Answer MAIL FROM with 451 this many times.
    pub busy: usize,
    /// Close the connection when MAIL FROM arrives, this many times.
    pub drop_on_mail: usize,
    /// Replies to the end of successive messages instead of 250, e.g. 451 or 554; such
    /// a message is delivered only if the code is 2xx.
    pub end_of_data_replies: Vec<u16>,
    /// Answer EHLO with 421 and close the connection this many times, as a busy or
    /// rate-limiting server does.
    pub ehlo_busy: usize,
    /// Deliver this many messages, but close the connection instead of confirming
    /// them: the confirmation is lost.
    pub drop_after_data: usize,
    /// Right after `220 Ready to start TLS`, also send in plaintext a forged EHLO reply
    /// advertising `SIZE 1`, which the real one only does with `size_limit`.
    pub inject_after_starttls: bool,
    /// Hold the reply to the end of each message until it gets a permit, e.g. to act
    /// while the client waits for the verdict.
    pub end_of_data_gate: Option<Arc<Semaphore>>,
}

impl Default for SmtpFaults {
    fn default() -> Self {
        Self {
            unavailable: 0,
            auth_mechanisms: vec!["PLAIN", "LOGIN"],
            reject_login: false,
            size_limit: None,
            reject_recipients: Vec::new(),
            busy: 0,
            drop_on_mail: 0,
            end_of_data_replies: Vec::new(),
            ehlo_busy: 0,
            drop_after_data: 0,
            inject_after_starttls: false,
            end_of_data_gate: None,
        }
    }
}

#[derive(Debug)]
pub struct SmtpState {
    pub username: String,
    pub password: String,
    /// Delivered messages, in order.
    pub received: Vec<ReceivedMessage>,
    pub faults: SmtpFaults,
    pub log: Vec<LoggedCommand>,
    pub connections: usize,
}

impl SmtpState {
    pub fn new(username: &str, password: &str) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            received: Vec::new(),
            faults: SmtpFaults::default(),
            log: Vec::new(),
            connections: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TlsMode {
    None,
    Implicit,
    StartTls,
}

/// A running fake SMTP server. Dropping it stops accepting connections.
pub struct FakeSmtp {
    addr: SocketAddr,
    state: Arc<Mutex<SmtpState>>,
    task: JoinHandle<()>,
}

impl Drop for FakeSmtp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeSmtp {
    /// Plaintext server on 127.0.0.1.
    pub async fn start(state: SmtpState) -> Self {
        Self::spawn(state, TlsMode::None, None).await
    }

    /// TLS from the first byte.
    pub async fn start_tls(state: SmtpState, cert: &TestCert) -> Self {
        Self::spawn(state, TlsMode::Implicit, Some(cert.clone())).await
    }

    /// Plaintext with STARTTLS; AUTH is only offered once TLS is up.
    pub async fn start_starttls(state: SmtpState, cert: &TestCert) -> Self {
        Self::spawn(state, TlsMode::StartTls, Some(cert.clone())).await
    }

    async fn spawn(state: SmtpState, mode: TlsMode, cert: Option<TestCert>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(Mutex::new(state));
        let task = tokio::spawn({
            let state = state.clone();
            async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let (state, cert) = (state.clone(), cert.clone());
                    tokio::spawn(async move {
                        lock(&state).connections += 1;
                        let result = match (mode, &cert) {
                            (TlsMode::Implicit, Some(cert)) => {
                                match cert.acceptor().accept(tcp).await {
                                    Ok(tls) => serve(Box::new(tls), true, mode, None, state).await,
                                    Err(e) => Err(e),
                                }
                            }
                            _ => serve(Box::new(tcp), false, mode, cert, state).await,
                        };
                        if let Err(e) = result {
                            tracing::debug!(error = %e, "fake SMTP connection ended");
                        }
                    });
                }
            }
        });
        Self { addr, state, task }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Locks the server state, e.g. to set faults or look at what was delivered.
    pub fn state(&self) -> MutexGuard<'_, SmtpState> {
        lock(&self.state)
    }
}

fn lock(state: &Mutex<SmtpState>) -> MutexGuard<'_, SmtpState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

#[derive(Default)]
struct Transaction {
    from: Option<String>,
    recipients: Vec<String>,
}

async fn serve(
    stream: Box<dyn Stream>,
    mut tls: bool,
    mode: TlsMode,
    cert: Option<TestCert>,
    state: Arc<Mutex<SmtpState>>,
) -> std::io::Result<()> {
    let mut io = BufReader::new(stream);
    let mut authenticated = false;
    let mut tx = Transaction::default();
    let unavailable = take(&mut lock(&state).faults.unavailable);
    if unavailable {
        io.get_mut()
            .write_all(b"421 4.3.2 Service not available\r\n")
            .await?;
        return Ok(());
    }
    io.get_mut().write_all(b"220 fake SMTP ready\r\n").await?;
    loop {
        let Some(line) = read_line(&mut io).await? else {
            return Ok(());
        };
        lock(&state).log.push(LoggedCommand {
            tls,
            line: line.clone(),
        });
        let (verb, arg) = line.split_once(' ').unwrap_or((&line, ""));
        let verb = verb.to_ascii_uppercase();
        let reply: String = match verb.as_str() {
            "EHLO" => {
                let busy = take(&mut lock(&state).faults.ehlo_busy);
                if busy {
                    io.get_mut()
                        .write_all(b"421 4.7.0 Too many connections, try later\r\n")
                        .await?;
                    return Ok(());
                }
                let st = lock(&state);
                let mut lines = vec!["fake.example".to_owned(), "8BITMIME".into()];
                if let Some(limit) = st.faults.size_limit {
                    lines.push(format!("SIZE {limit}"));
                }
                if mode == TlsMode::StartTls && !tls {
                    lines.push("STARTTLS".into());
                } else if !st.faults.auth_mechanisms.is_empty() {
                    lines.push(format!("AUTH {}", st.faults.auth_mechanisms.join(" ")));
                }
                multiline(250, &lines)
            }
            "STARTTLS" if mode == TlsMode::StartTls && !tls => {
                let mut reply = "220 Ready to start TLS\r\n".to_owned();
                if lock(&state).faults.inject_after_starttls {
                    reply += "250-forged\r\n250 SIZE 1\r\n";
                }
                io.get_mut().write_all(reply.as_bytes()).await?;
                let cert = cert.as_ref().expect("STARTTLS needs a certificate");
                // Dropping the reader discards anything pipelined in plaintext.
                let stream = cert.acceptor().accept(io.into_inner()).await?;
                io = BufReader::new(Box::new(stream));
                tls = true;
                tx = Transaction::default();
                continue;
            }
            "AUTH" => {
                let (mechanism, initial) = arg.split_once(' ').unwrap_or((arg, ""));
                let ok = match mechanism.to_ascii_uppercase().as_str() {
                    "PLAIN" => {
                        let initial = if initial.is_empty() {
                            io.get_mut().write_all(b"334 \r\n").await?;
                            read_line(&mut io).await?.unwrap_or_default()
                        } else {
                            initial.to_owned()
                        };
                        let decoded = decode(&initial);
                        let parts: Vec<&[u8]> = decoded.split(|&b| b == 0).collect();
                        let st = lock(&state);
                        parts.len() == 3
                            && parts[1] == st.username.as_bytes()
                            && parts[2] == st.password.as_bytes()
                    }
                    "LOGIN" => {
                        io.get_mut().write_all(b"334 VXNlcm5hbWU6\r\n").await?;
                        let user = decode(&read_line(&mut io).await?.unwrap_or_default());
                        io.get_mut().write_all(b"334 UGFzc3dvcmQ6\r\n").await?;
                        let pass = decode(&read_line(&mut io).await?.unwrap_or_default());
                        let st = lock(&state);
                        user == st.username.as_bytes() && pass == st.password.as_bytes()
                    }
                    _ => false,
                };
                if ok && !lock(&state).faults.reject_login {
                    authenticated = true;
                    "235 Authentication succeeded\r\n".into()
                } else {
                    "535 5.7.8 Authentication credentials invalid\r\n".into()
                }
            }
            "NOOP" => "250 OK\r\n".into(),
            "RSET" => {
                tx = Transaction::default();
                "250 OK\r\n".into()
            }
            "QUIT" => {
                io.get_mut().write_all(b"221 Bye\r\n").await?;
                return Ok(());
            }
            "MAIL" if !authenticated && !lock(&state).faults.auth_mechanisms.is_empty() => {
                "530 5.7.0 Authentication required\r\n".into()
            }
            "MAIL" => {
                let st = &mut *lock(&state);
                if take(&mut st.faults.drop_on_mail) {
                    return Ok(());
                }
                if take(&mut st.faults.busy) {
                    "451 4.3.0 Try again later\r\n".into()
                } else if let Some(size) = param(arg, "SIZE").and_then(|s| s.parse::<u64>().ok())
                    && st.faults.size_limit.is_some_and(|limit| size > limit)
                {
                    "552 5.3.4 Message too big\r\n".into()
                } else {
                    tx = Transaction {
                        from: Some(path(arg)),
                        recipients: Vec::new(),
                    };
                    "250 OK\r\n".into()
                }
            }
            "RCPT" if tx.from.is_none() => "503 5.5.1 MAIL first\r\n".into(),
            "RCPT" => {
                let rcpt = path(arg);
                let st = lock(&state);
                match st.faults.reject_recipients.iter().find(|(a, _)| *a == rcpt) {
                    Some((_, code)) => format!("{code} No such user here\r\n"),
                    None => {
                        tx.recipients.push(rcpt);
                        "250 OK\r\n".into()
                    }
                }
            }
            "DATA" if tx.recipients.is_empty() => "503 5.5.1 RCPT first\r\n".into(),
            "DATA" => {
                io.get_mut()
                    .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                    .await?;
                let Some(data) = read_data(&mut io).await? else {
                    return Ok(());
                };
                let gate = lock(&state).faults.end_of_data_gate.clone();
                if let Some(gate) = gate {
                    gate.acquire().await.expect("gate open").forget();
                }
                let tx = std::mem::take(&mut tx);
                let message = ReceivedMessage {
                    from: tx.from.unwrap_or_default(),
                    recipients: tx.recipients,
                    data,
                };
                let st = &mut *lock(&state);
                if take(&mut st.faults.drop_after_data) {
                    st.received.push(message);
                    return Ok(());
                }
                match (!st.faults.end_of_data_replies.is_empty())
                    .then(|| st.faults.end_of_data_replies.remove(0))
                {
                    Some(code) => {
                        if (200..300).contains(&code) {
                            st.received.push(message);
                        }
                        format!("{code} Fake verdict\r\n")
                    }
                    None => {
                        st.received.push(message);
                        "250 2.0.0 OK queued\r\n".into()
                    }
                }
            }
            _ => "500 5.5.2 Unknown command\r\n".into(),
        };
        io.get_mut().write_all(reply.as_bytes()).await?;
        io.get_mut().flush().await?;
    }
}

/// Uses up one of a fault's remaining occurrences, if there is one left.
fn take(remaining: &mut usize) -> bool {
    let fires = *remaining > 0;
    *remaining = remaining.saturating_sub(1);
    fires
}

/// One line without its CRLF; `None` at EOF.
async fn read_line(io: &mut BufReader<Box<dyn Stream>>) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    if io.read_until(b'\n', &mut line).await? == 0 {
        return Ok(None);
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    Ok(Some(String::from_utf8_lossy(&line).into_owned()))
}

/// The message up to the `.` line, unstuffed, CRLF kept; `None` at EOF.
async fn read_data(io: &mut BufReader<Box<dyn Stream>>) -> std::io::Result<Option<Vec<u8>>> {
    let mut data = Vec::new();
    loop {
        let mut line = Vec::new();
        if io.read_until(b'\n', &mut line).await? == 0 {
            return Ok(None);
        }
        if line == b".\r\n" {
            return Ok(Some(data));
        }
        data.extend_from_slice(line.strip_prefix(b".").unwrap_or(&line));
    }
}

fn multiline(code: u16, lines: &[String]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let sep = if i + 1 == lines.len() { ' ' } else { '-' };
        out += &format!("{code}{sep}{line}\r\n");
    }
    out
}

/// `FROM:<a@b> SIZE=10` → `a@b`.
fn path(arg: &str) -> String {
    let start = arg.find('<').map_or(0, |i| i + 1);
    let end = arg[start..].find('>').map_or(arg.len(), |i| start + i);
    arg[start..end].to_owned()
}

/// The value of `NAME=value` among MAIL FROM's parameters.
fn param<'a>(arg: &'a str, name: &str) -> Option<&'a str> {
    arg.split_whitespace().skip(1).find_map(|p| {
        let (key, value) = p.split_once('=')?;
        key.eq_ignore_ascii_case(name).then_some(value)
    })
}

fn decode(line: &str) -> Vec<u8> {
    BASE64_STANDARD.decode(line.trim()).unwrap_or_default()
}
