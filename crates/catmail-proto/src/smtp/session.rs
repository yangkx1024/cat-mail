use std::collections::HashMap;
use std::time::Duration;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use catmail_types::{Security, ServerConfig};
use secrecy::ExposeSecret;
use secrecy::zeroize::Zeroizing;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::{Error, Result};
use crate::mime::fits_in_angle_brackets;
use crate::net::{self, with_timeout};
use crate::{Credentials, Stream, TlsConfig};

/// What EHLO is given as the client's name. An address literal is valid (RFC 5321
/// §4.1.3) and, unlike the host name or the local address, tells the server nothing about
/// the machine. Submission servers authenticate the user instead of checking it.
const CLIENT_NAME: &str = "[127.0.0.1]";

/// RFC 5321 §4.5.3.2.6: the server may take this long to answer the end of the message
/// (it often scans it first). Giving up earlier would only turn a delivery into an
/// unknown outcome.
const END_OF_DATA_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Longest reply accepted, all lines together. Real ones are a few hundred bytes.
const MAX_REPLY: u64 = 64 * 1024;

/// The message is dot-stuffed and written this much at a time, rather than copied whole.
const DATA_CHUNK: usize = 64 * 1024;

/// An SMTP connection, from greeting to QUIT.
pub struct Session {
    io: BufReader<Box<dyn Stream>>,
    /// EHLO keywords, upper-cased, with their parameters.
    extensions: HashMap<String, String>,
    timeout: Duration,
}

struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Reply {
    /// On one line: it ends up in errors, and from there in `last_error` and the UI.
    fn text(&self) -> String {
        self.lines.join(" ")
    }
}

impl Session {
    /// Connects, reads the greeting and introduces itself. With STARTTLS, a server that
    /// doesn't offer it is an error; there is no silent downgrade.
    pub async fn connect(
        server: &ServerConfig,
        tls: &TlsConfig,
        timeout: Duration,
    ) -> Result<Self> {
        let (stream, server_name) = net::connect(server, tls, timeout).await?;
        let mut session = Self::new(stream, timeout);

        let greeting = session.read_reply(timeout).await?;
        if greeting.code != 220 {
            return Err(reply_error("connection", greeting));
        }
        session.ehlo().await?;

        if server.security == Security::StartTls {
            if !session.has("STARTTLS") {
                return Err(Error::Unsupported("STARTTLS"));
            }
            session.command("STARTTLS", "STARTTLS", 220).await?;
            // Dropping the reader drops anything the server sent after its reply, so
            // nothing sent in plaintext can be taken for a reply inside TLS.
            let plain = session.io.into_inner();
            let tls_stream = net::start_tls(plain, &server_name, tls, timeout).await?;
            // What the server said before TLS may have been tampered with.
            session = Self::new(Box::new(tls_stream), timeout);
            session.ehlo().await?;
        }
        Ok(session)
    }

    fn new(stream: Box<dyn Stream>, timeout: Duration) -> Self {
        Self {
            io: BufReader::new(stream),
            extensions: HashMap::new(),
            timeout,
        }
    }

    /// The largest message the server accepts, if it says (SIZE, RFC 1870).
    pub fn max_size(&self) -> Option<u64> {
        self.extensions
            .get("SIZE")
            .and_then(|limit| limit.trim().parse().ok())
            .filter(|&limit| limit > 0)
    }

    /// Authenticates with AUTH PLAIN, else AUTH LOGIN. A server that doesn't offer AUTH
    /// is taken to need none (a local relay); STARTTLS can't have been stripped, since
    /// it is required whenever it is configured.
    pub async fn login(&mut self, credentials: &Credentials) -> Result<()> {
        let Some(mechanisms) = self.extensions.get("AUTH") else {
            tracing::debug!("server offers no AUTH; sending without logging in");
            return Ok(());
        };
        let mechanisms: Vec<String> = mechanisms
            .split_whitespace()
            .map(str::to_ascii_uppercase)
            .collect();
        let has = |m: &str| mechanisms.iter().any(|x| x == m);
        let reply = match credentials {
            Credentials::Password { username, password } => {
                if has("PLAIN") {
                    let initial = Zeroizing::new(
                        BASE64_STANDARD.encode(Credentials::sasl_plain(username, password)),
                    );
                    let reply = self.send_parts(&["AUTH PLAIN ", &initial]).await?;
                    if reply.code == 334 {
                        // A server that wants the response on its own line (RFC 4954 allows
                        // either) gets it again.
                        self.send_line(&initial).await?
                    } else {
                        reply
                    }
                } else if has("LOGIN") {
                    let mut reply = self.send_line("AUTH LOGIN").await?;
                    for answer in [username.as_str(), password.expose_secret()] {
                        if reply.code != 334 {
                            break;
                        }
                        let answer = Zeroizing::new(BASE64_STANDARD.encode(answer));
                        reply = self.send_line(&answer).await?;
                    }
                    reply
                } else {
                    return Err(Error::Unsupported("a password login mechanism"));
                }
            }
        };
        match reply.code {
            235 => Ok(()),
            // 454: temporary failure, e.g. the authentication backend is down.
            400..500 => Err(reply_error("AUTH", reply)),
            _ => Err(Error::AuthFailed(format!(
                "{} {}",
                reply.code,
                reply.text()
            ))),
        }
    }

    /// Starts a transaction: MAIL FROM, RCPT TO for every recipient, DATA. If the server
    /// refuses any recipient, the transaction is reset and the refusal (naming the
    /// address) returned: nobody gets a message that some of its recipients won't.
    pub async fn begin(&mut self, from: &str, recipients: &[String], message: &[u8]) -> Result<()> {
        if recipients.is_empty() {
            return Err(Error::InvalidAddress(String::new()));
        }
        for address in std::iter::once(from).chain(recipients.iter().map(String::as_str)) {
            if !is_envelope_address(address) {
                return Err(Error::InvalidAddress(address.to_owned()));
            }
        }
        if let Some(limit) = self.max_size()
            && message.len() as u64 > limit
        {
            return Err(Error::TooLarge {
                size: message.len(),
                limit,
            });
        }
        let mut mail_from = format!("MAIL FROM:<{from}>");
        if self.has("SIZE") {
            mail_from += &format!(" SIZE={}", message.len());
        }
        if !message.is_ascii() && self.has("8BITMIME") {
            mail_from += " BODY=8BITMIME";
        }
        self.command(&mail_from, "MAIL FROM", 250).await?;
        for rcpt in recipients {
            let reply = self.send_line(&format!("RCPT TO:<{rcpt}>")).await?;
            if !matches!(reply.code, 250 | 251) {
                self.reset().await;
                return Err(Error::Reply {
                    command: "RCPT TO",
                    code: reply.code,
                    text: format!("<{rcpt}>: {}", reply.text()),
                });
            }
        }
        if let Err(e) = self.command("DATA", "DATA", 354).await {
            if !e.is_fatal() {
                self.reset().await;
            }
            return Err(e);
        }
        Ok(())
    }

    /// Sends the message, dot-stuffed and with CRLF line endings, but not the end
    /// marker: until [`end_message`](Self::end_message), the server discards it if
    /// anything goes wrong.
    pub async fn write_message(&mut self, message: &[u8]) -> Result<()> {
        let mut stuffer = DotStuffer::default();
        let mut out = Vec::with_capacity(DATA_CHUNK + DATA_CHUNK / 8);
        for chunk in message.chunks(DATA_CHUNK) {
            out.clear();
            stuffer.push(chunk, &mut out);
            self.write(&out).await?;
        }
        out.clear();
        stuffer.finish(&mut out);
        self.write(&out).await
    }

    /// Ends the message and waits for the server's verdict. From the moment this is
    /// called the server may accept the message, so a failure without a reply (lost
    /// connection, timeout) means it may or may not have been delivered.
    pub async fn end_message(&mut self) -> Result<()> {
        self.write(b".\r\n").await?;
        let reply = self.read_reply(END_OF_DATA_TIMEOUT).await?;
        match reply.code {
            // RFC 5321 only has 250 here; any positive completion means the same.
            200..300 => Ok(()),
            400..600 => Err(reply_error("message", reply)),
            // Neither yes nor no, so whether the server kept the message can't be told.
            code => Err(Error::Protocol(format!(
                "unexpected reply to the end of the message: {code} {}",
                reply.text()
            ))),
        }
    }

    /// Ends the session politely. Errors are not interesting at this point.
    pub async fn quit(mut self) {
        if let Err(e) = self.send_line("QUIT").await {
            tracing::debug!(error = %e, "QUIT failed");
        }
    }

    /// Abandons a transaction after a refusal, so the connection can be used again.
    async fn reset(&mut self) {
        if let Err(e) = self.send_line("RSET").await {
            tracing::debug!(error = %e, "RSET failed");
        }
    }

    async fn ehlo(&mut self) -> Result<()> {
        let reply = self.send_line(&format!("EHLO {CLIENT_NAME}")).await?;
        match reply.code {
            250 => {}
            // Busy or rate limited (421): worth trying again later.
            400..500 => return Err(reply_error("EHLO", reply)),
            // Every submission server speaks ESMTP; without it there is no AUTH either.
            _ => return Err(Error::Unsupported("EHLO")),
        }
        // The first line is the server's greeting, the rest one extension each.
        self.extensions = reply
            .lines
            .iter()
            .skip(1)
            .map(|line| {
                let (keyword, params) = line.split_once(' ').unwrap_or((line, ""));
                (keyword.to_ascii_uppercase(), params.to_owned())
            })
            .collect();
        Ok(())
    }

    fn has(&self, extension: &str) -> bool {
        self.extensions.contains_key(extension)
    }

    /// Sends `line` and fails unless the reply has the expected code.
    async fn command(&mut self, line: &str, name: &'static str, expect: u16) -> Result<()> {
        let reply = self.send_line(line).await?;
        if reply.code == expect {
            Ok(())
        } else {
            Err(reply_error(name, reply))
        }
    }

    async fn send_line(&mut self, line: &str) -> Result<Reply> {
        self.send_parts(&[line]).await
    }

    /// Sends the parts as one line. It is put together in one buffer of its final size,
    /// zeroed when dropped: the line may carry an AUTH answer.
    async fn send_parts(&mut self, parts: &[&str]) -> Result<Reply> {
        let len = parts.iter().map(|part| part.len()).sum::<usize>() + 2;
        let mut line = Zeroizing::new(String::with_capacity(len));
        for part in parts {
            line.push_str(part);
        }
        line.push_str("\r\n");
        self.write(line.as_bytes()).await?;
        self.read_reply(self.timeout).await
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        let stream = self.io.get_mut();
        with_timeout(self.timeout, async {
            stream.write_all(data).await?;
            stream.flush().await
        })
        .await?;
        Ok(())
    }

    /// A reply of one or more lines (`250-first`, `250 last`).
    async fn read_reply(&mut self, timeout: Duration) -> Result<Reply> {
        let mut lines = Vec::new();
        let mut budget = MAX_REPLY;
        loop {
            let mut line = Vec::new();
            let n = with_timeout(
                timeout,
                (&mut self.io).take(budget).read_until(b'\n', &mut line),
            )
            .await?;
            if n == 0 {
                return Err(if budget == 0 {
                    Error::Protocol("reply too long".into())
                } else {
                    Error::Closed
                });
            }
            budget -= n as u64;
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            let code = line
                .get(..3)
                .and_then(|c| c.parse::<u16>().ok())
                .filter(|c| (200..600).contains(c))
                .ok_or_else(|| Error::Protocol(format!("malformed reply `{line}`")))?;
            let (more, rest) = match line.as_bytes().get(3) {
                Some(b'-') => (true, &line[4..]),
                Some(b' ') => (false, &line[4..]),
                None => (false, ""),
                Some(_) => return Err(Error::Protocol(format!("malformed reply `{line}`"))),
            };
            lines.push(rest.to_owned());
            if !more {
                return Ok(Reply { code, lines });
            }
        }
    }
}

fn reply_error(command: &'static str, reply: Reply) -> Error {
    Error::Reply {
        command,
        code: reply.code,
        text: reply.text(),
    }
}

/// Whether `address` can go into `MAIL FROM:<…>` / `RCPT TO:<…>` as is: nothing that
/// could end the command or the path early, and no non-ASCII (that needs SMTPUTF8).
pub fn is_envelope_address(address: &str) -> bool {
    address
        .split_once('@')
        .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
        && fits_in_angle_brackets(address)
}

/// Turns a message into DATA content (RFC 5321 §4.5.2), a piece at a time: every line
/// break CRLF, a line starting with `.` gets another one, and the last line ends with
/// CRLF. A bare CR or LF becomes CRLF, so no server can see the end marker in a
/// different place than we do.
struct DotStuffer {
    line_start: bool,
    /// The last byte was a CR, already written as CRLF; an LF right after it is not a
    /// line break of its own.
    after_cr: bool,
    empty: bool,
}

impl Default for DotStuffer {
    fn default() -> Self {
        Self {
            line_start: true,
            after_cr: false,
            empty: true,
        }
    }
}

impl DotStuffer {
    fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &b in input {
            if std::mem::take(&mut self.after_cr) && b == b'\n' {
                continue;
            }
            self.empty = false;
            match b {
                b'\r' | b'\n' => {
                    out.extend_from_slice(b"\r\n");
                    self.line_start = true;
                    self.after_cr = b == b'\r';
                }
                b => {
                    if self.line_start && b == b'.' {
                        out.push(b'.');
                    }
                    out.push(b);
                    self.line_start = false;
                }
            }
        }
    }

    fn finish(self, out: &mut Vec<u8>) {
        if !self.line_start || self.empty {
            out.extend_from_slice(b"\r\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dot_stuff(message: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut stuffer = DotStuffer::default();
        // One byte at a time, so state carried across pieces is covered too.
        for b in message.chunks(1) {
            stuffer.push(b, &mut out);
        }
        stuffer.finish(&mut out);
        out
    }

    #[test]
    fn dot_stuffs_and_normalizes_line_endings() {
        assert_eq!(dot_stuff(b"a\r\n.b\r\n"), b"a\r\n..b\r\n");
        assert_eq!(dot_stuff(b".\r\nx"), b"..\r\nx\r\n");
        // A bare LF or CR can't smuggle an end marker past the stuffing.
        assert_eq!(dot_stuff(b"a\n.\nb"), b"a\r\n..\r\nb\r\n");
        assert_eq!(dot_stuff(b"a\r.\rb"), b"a\r\n..\r\nb\r\n");
        assert_eq!(dot_stuff(b""), b"\r\n");
        assert_eq!(dot_stuff(b"x\r\n"), b"x\r\n");
    }

    #[test]
    fn checks_addresses() {
        assert!(is_envelope_address("a@example.com"));
        assert!(is_envelope_address("a+tag@[192.0.2.1]"));
        for bad in [
            "",
            "no-at",
            "@example.com",
            "a@",
            "a@example.com>\r\nRCPT TO:<evil@x",
            "a b@example.com",
            "张三@example.com",
        ] {
            assert!(!is_envelope_address(bad), "{bad:?}");
        }
    }
}
