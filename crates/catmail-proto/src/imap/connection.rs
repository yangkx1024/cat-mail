//! Drives imap-next's sans-IO client over any async byte stream.

use std::future::Future;
use std::time::Duration;

use imap_next::client::{Client, Event, Options};
use imap_next::imap_types::auth::{AuthMechanism, AuthenticateData};
use imap_next::imap_types::command::{Command, CommandBody};
use imap_next::imap_types::core::Tag;
use imap_next::imap_types::response::{Code, Data, Greeting, Status, StatusKind};
use imap_next::{Interrupt, Io, State};
use secrecy::zeroize::Zeroizing;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::quirks::Quirks;
use super::{Error, Result};

pub(crate) use crate::Stream;
use crate::net::with_timeout;

/// How many bytes the server may send in answer to one command. imap-next caps each
/// response at 100 MiB, but not how many there are, and a command's whole answer is kept
/// until it completes. The largest answer we ask for is one whole message, so only a
/// broken or hostile server gets near this.
const MAX_ANSWER: usize = 256 * 1024 * 1024;

/// What the server sent while a command ran, up to and including its tagged OK.
#[derive(Debug, Default)]
pub(crate) struct Response {
    pub data: Vec<Data<'static>>,
    /// Codes from untagged status responses (`* OK [UIDVALIDITY 7]`) and the tagged OK.
    pub codes: Vec<Code<'static>>,
    /// Lines that couldn't be parsed and were dropped. Nonzero means `data` may be
    /// missing something; only [`Connection::execute_lossy`] returns such a response.
    pub skipped: usize,
}

pub(crate) struct Connection {
    stream: Box<dyn Stream>,
    client: Client,
    /// Repairs what the server sends before `client` parses it.
    quirks: Quirks,
    last_tag: u32,
    buf: Box<[u8]>,
    timeout: Duration,
    /// Bytes read since the current command was sent, and how many it may have
    /// ([`MAX_ANSWER`] but in tests).
    received: usize,
    max_answer: usize,
    /// Set once the server refused a command before all of it was sent: imap-next still
    /// waits to send the rest, so nothing sent after it would ever go out. Also set when
    /// a command is given up half-way through its answer, so the rest can't be taken for
    /// the answer to the next one.
    out_of_step: bool,
    /// What the server said about the selected mailbox in passing, in the answers to any
    /// command (servers report new mail and expunges whenever they can): the last EXISTS
    /// less the EXPUNGEs since, and whether it reported a change since the mailbox was
    /// opened. A change is an EXISTS with a new count, an EXPUNGE, a VANISHED, and while
    /// idling a FETCH (outside IDLE that is usually the answer to our own FETCH).
    exists: u32,
    changed: bool,
}

impl Connection {
    /// `expect_greeting` is false after STARTTLS, where the TLS session has no greeting.
    pub fn new(stream: Box<dyn Stream>, expect_greeting: bool, timeout: Duration) -> Self {
        let mut options = Options::default();
        // Real servers occasionally send bare LF; accepting it costs nothing.
        options.crlf_relaxed = true;
        options.discard_greeting = !expect_greeting;
        Self {
            stream,
            client: Client::new(options),
            quirks: Quirks::default(),
            last_tag: 0,
            buf: vec![0; 64 * 1024].into_boxed_slice(),
            timeout,
            received: 0,
            max_answer: MAX_ANSWER,
            out_of_step: false,
            exists: 0,
            changed: false,
        }
    }

    /// Gives up the stream, e.g. to wrap it in TLS. Any bytes the server sent after the
    /// last response are dropped with the client, so nothing sent in plaintext can be
    /// mistaken for a reply inside the TLS session.
    pub fn into_stream(self) -> Box<dyn Stream> {
        self.stream
    }

    pub async fn greeting(&mut self) -> Result<Greeting<'static>> {
        loop {
            match self.next_event(&mut 0).await? {
                Event::GreetingReceived { greeting } => return Ok(greeting),
                other => {
                    tracing::debug!(event = event_name(&other), "ignoring event before greeting")
                }
            }
        }
    }

    /// Sends one command and collects everything up to its tagged completion.
    /// NO and BAD become [`Error::Command`]. If any line of the answer couldn't be
    /// parsed, the answer is [`Error::Incomplete`]: most results (LIST, SEARCH, EXAMINE,
    /// flags) are only meaningful whole, and a missing line would read as something
    /// deleted.
    pub async fn execute(&mut self, body: CommandBody<'static>) -> Result<Response> {
        let command = body.name();
        let response = self.execute_lossy(body).await?;
        if response.skipped > 0 {
            return Err(Error::Incomplete(command));
        }
        Ok(response)
    }

    /// Like [`execute`](Self::execute), but returns an answer with unparsable lines too,
    /// counted in [`Response::skipped`]. Only for results that are useful in part and
    /// whose caller notices what is missing.
    pub async fn execute_lossy(&mut self, body: CommandBody<'static>) -> Result<Response> {
        let command = body.name();
        let tag = self.begin(body)?;
        let mut response = Response::default();
        let mut sent = false;
        loop {
            match self.next_event(&mut response.skipped).await? {
                Event::CommandSent { .. } => sent = true,
                Event::CommandRejected { status, .. } => {
                    return Err(Error::Command {
                        command,
                        text: status.text().to_string(),
                    });
                }
                Event::DataReceived { data } => {
                    self.track(&data, false);
                    response.data.push(data);
                }
                Event::StatusReceived { status } => {
                    match self.on_status(&tag, command, status, &mut response) {
                        // imap-next only gives up a command with a pending literal on
                        // BAD (`CommandRejected`); after a NO it still waits to send
                        // the rest, ahead of anything queued later.
                        Some(Err(Error::Command { command, text })) if !sent => {
                            self.out_of_step = true;
                            return Err(Error::Aborted { command, text });
                        }
                        Some(result) => return result.map(|()| response),
                        None => {}
                    }
                }
                Event::ContinuationRequestReceived { .. } => {
                    tracing::debug!(command, "ignoring unexpected continuation request");
                }
                other => {
                    return Err(Error::Protocol(format!(
                        "unexpected {} during {command}",
                        event_name(&other)
                    )));
                }
            }
        }
    }

    /// Runs IDLE (RFC 2177) until the server reports a change to the selected mailbox,
    /// `stop` resolves or `max` passes, then ends it with DONE. Returns whether the server
    /// reported a change. Reads while idling have no timeout but `max`: a quiet server is
    /// the point.
    pub async fn idle(&mut self, stop: impl Future<Output = ()>, max: Duration) -> Result<bool> {
        let tag = self.begin(CommandBody::Idle)?;
        // `sleep` copes with any `max`; adding it to an instant may overflow.
        let mut timer = std::pin::pin!(tokio::time::sleep(max));
        let mut stop = std::pin::pin!(stop);
        // Between the server's `+` and our DONE.
        let mut idling = false;
        let mut response = Response::default();
        loop {
            match self.client.next() {
                Ok(Event::IdleCommandSent { .. } | Event::IdleDoneSent { .. }) => {}
                Ok(Event::IdleAccepted { .. }) => idling = true,
                // imap-next reports a server ending IDLE on its own (a tagged OK before
                // DONE) this way too.
                Ok(Event::IdleRejected { status, .. }) => {
                    return match status {
                        Status::Tagged(tagged) if tagged.body.kind == StatusKind::Ok => {
                            Ok(self.changed)
                        }
                        other => Err(Error::Command {
                            command: "IDLE",
                            text: other.text().to_string(),
                        }),
                    };
                }
                Ok(Event::DataReceived { data }) => self.track(&data, true),
                Ok(Event::StatusReceived { status }) => {
                    if let Some(result) = self.on_status(&tag, "IDLE", status, &mut response) {
                        return result.map(|()| self.changed);
                    }
                }
                Ok(Event::ContinuationRequestReceived { .. }) => {
                    tracing::debug!("ignoring unexpected continuation request during IDLE");
                }
                Ok(other) => {
                    return Err(Error::Protocol(format!(
                        "unexpected {} during IDLE",
                        event_name(&other)
                    )));
                }
                Err(Interrupt::Io(Io::Output(bytes))) => self.write(&bytes).await?,
                Err(Interrupt::Io(Io::NeedMoreInput)) if !idling => self.read().await?,
                Err(Interrupt::Io(Io::NeedMoreInput)) => {
                    // Reading is cancel-safe: nothing read is lost if `stop` wins.
                    let done = self.changed
                        || tokio::select! {
                            read = self.read_untimed() => { read?; false }
                            () = &mut stop => true,
                            () = &mut timer => true,
                        };
                    if done {
                        self.client.set_idle_done();
                        idling = false;
                    }
                }
                // Whatever it was, look at the mailbox again.
                Err(Interrupt::Error(e)) => {
                    tracing::warn!(error = %e, "skipping malformed server response during IDLE");
                    self.changed = true;
                }
            }
        }
    }

    /// Runs `AUTHENTICATE <mechanism>`, answering the first challenge with `initial`.
    /// A rejection becomes [`Error::AuthFailed`].
    pub async fn authenticate(
        &mut self,
        mechanism: AuthMechanism<'static>,
        initial: Zeroizing<Vec<u8>>,
    ) -> Result<Response> {
        let tag = self.begin(CommandBody::authenticate(mechanism))?;
        let mut initial = Some(initial);
        let mut response = Response::default();
        loop {
            // Nothing in the answer is authoritative, so unparsable lines don't matter.
            match self.next_event(&mut response.skipped).await? {
                Event::AuthenticateStarted { .. } => {}
                Event::AuthenticateContinuationRequestReceived { .. } => {
                    // A second challenge means the server didn't like the answer (XOAUTH2
                    // sends an error this way); cancel and let it send the tagged NO.
                    let data = match initial.take() {
                        // imap-next takes its own copy, which it doesn't zero.
                        Some(data) => AuthenticateData::r#continue(data.to_vec()),
                        None => AuthenticateData::Cancel,
                    };
                    self.client
                        .set_authenticate_data(data)
                        .map_err(|_| Error::Protocol("AUTHENTICATE out of order".into()))?;
                }
                Event::AuthenticateStatusReceived { status, .. } => {
                    return match status {
                        Status::Tagged(tagged) if tagged.body.kind == StatusKind::Ok => {
                            response.codes.extend(tagged.body.code);
                            Ok(response)
                        }
                        other => Err(Error::AuthFailed(other.text().to_string())),
                    };
                }
                Event::DataReceived { data } => {
                    self.track(&data, false);
                    response.data.push(data);
                }
                Event::StatusReceived { status } => {
                    if let Some(result) =
                        self.on_status(&tag, "AUTHENTICATE", status, &mut response)
                    {
                        return result.map(|()| response);
                    }
                }
                other => {
                    return Err(Error::Protocol(format!(
                        "unexpected {} during AUTHENTICATE",
                        event_name(&other)
                    )));
                }
            }
        }
    }

    /// Fails every command once the connection is out of step (see `out_of_step`), so it
    /// can't be used by mistake even by a caller that kept it after the error.
    fn check_in_step(&self) -> Result<()> {
        if self.out_of_step {
            return Err(Error::Protocol(
                "connection unusable after a command was given up half-way".into(),
            ));
        }
        Ok(())
    }

    /// Queues a command under a new tag, which it returns.
    fn begin(&mut self, body: CommandBody<'static>) -> Result<Tag<'static>> {
        self.check_in_step()?;
        self.received = 0;
        self.last_tag += 1;
        let tag = Tag::try_from(format!("C{}", self.last_tag)).expect("valid tag");
        self.client.enqueue_command(Command {
            tag: tag.clone(),
            body,
        });
        Ok(tag)
    }

    /// A mailbox was just opened, with `exists` messages: reported changes count from
    /// here.
    pub fn mailbox_opened(&mut self, exists: u32) {
        self.exists = exists;
        self.changed = false;
    }

    /// The selected mailbox's message count, as of the server's last word.
    pub fn exists(&self) -> u32 {
        self.exists
    }

    /// Whether the server reported a change to the selected mailbox since it was opened.
    pub fn mailbox_changed(&self) -> bool {
        self.changed
    }

    /// Returns `Some` once the command identified by `tag` has completed.
    fn on_status(
        &self,
        tag: &Tag<'static>,
        command: &'static str,
        status: Status<'static>,
        response: &mut Response,
    ) -> Option<Result<()>> {
        match status {
            Status::Tagged(tagged) if &tagged.tag == tag => Some(match tagged.body.kind {
                StatusKind::Ok => {
                    response.codes.extend(tagged.body.code);
                    Ok(())
                }
                StatusKind::No | StatusKind::Bad => Err(Error::Command {
                    command,
                    text: tagged.body.text.to_string(),
                }),
            }),
            Status::Tagged(tagged) => {
                tracing::warn!(tag = %tagged.tag.as_ref(), "ignoring status for an unknown tag");
                None
            }
            Status::Untagged(body) => {
                if body.kind != StatusKind::Ok {
                    tracing::warn!(command, kind = ?body.kind, text = %body.text, "server warning");
                }
                response.codes.extend(body.code);
                None
            }
            // The server closes the connection after BYE, except as part of LOGOUT.
            Status::Bye(bye) if command == "LOGOUT" => {
                tracing::debug!(text = %bye.text, "logged out");
                None
            }
            Status::Bye(bye) => Some(Err(Error::Bye(bye.text.to_string()))),
        }
    }

    fn track(&mut self, data: &Data<'_>, idling: bool) {
        match data {
            Data::Exists(n) => {
                self.changed |= self.exists != *n;
                self.exists = *n;
            }
            Data::Expunge(_) => {
                self.exists = self.exists.saturating_sub(1);
                self.changed = true;
            }
            Data::Vanished { .. } => self.changed = true,
            Data::Fetch { .. } if idling => self.changed = true,
            _ => {}
        }
    }

    /// Counts lines it had to skip in `skipped`.
    async fn next_event(&mut self, skipped: &mut usize) -> Result<Event> {
        loop {
            match self.client.next() {
                Ok(event) => return Ok(event),
                // Commands are small and sent whole before reading, so writing and
                // reading in turn can't deadlock against a server that does the same.
                Err(Interrupt::Io(Io::Output(bytes))) => self.write(&bytes).await?,
                Err(Interrupt::Io(Io::NeedMoreInput)) => {
                    self.read().await?;
                    if self.received > self.max_answer {
                        self.out_of_step = true;
                        return Err(Error::Protocol(format!(
                            "server sent more than {} MiB in answer to one command",
                            self.max_answer / (1024 * 1024)
                        )));
                    }
                }
                // The client has already skipped the offending line. One response we
                // can't parse (a buggy BODYSTRUCTURE, say) shouldn't take the whole
                // session down, but the command's result may now be incomplete.
                Err(Interrupt::Error(e)) => {
                    tracing::warn!(error = %e, "skipping malformed server response");
                    *skipped += 1;
                }
            }
        }
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let stream = &mut self.stream;
        with_timeout(self.timeout, async {
            stream.write_all(bytes).await?;
            stream.flush().await
        })
        .await?;
        Ok(())
    }

    /// Hands the next bytes from the server to the client.
    async fn read(&mut self) -> Result<()> {
        match tokio::time::timeout(self.timeout, self.read_untimed()).await {
            Ok(result) => result,
            Err(_) => Err(Error::Timeout),
        }
    }

    async fn read_untimed(&mut self) -> Result<()> {
        let n = self.stream.read(&mut self.buf).await?;
        if n == 0 {
            return Err(Error::Closed);
        }
        self.received = self.received.saturating_add(n);
        // Nothing while a line is still incomplete; the client then asks for more.
        let input = self.quirks.push(&self.buf[..n]);
        if !input.is_empty() {
            self.client.enqueue_input(input);
        }
        Ok(())
    }
}

/// An event's variant, for error messages and logs. Not the event itself: it may hold a
/// command with credentials in it, and imap-types only redacts those in release builds.
fn event_name(event: &Event) -> &'static str {
    match event {
        Event::GreetingReceived { .. } => "greeting",
        Event::CommandSent { .. } => "command sent",
        Event::CommandRejected { .. } => "command rejected",
        Event::AuthenticateStarted { .. } => "AUTHENTICATE started",
        Event::AuthenticateContinuationRequestReceived { .. } => {
            "AUTHENTICATE continuation request"
        }
        Event::AuthenticateStatusReceived { .. } => "AUTHENTICATE status",
        Event::IdleCommandSent { .. } => "IDLE sent",
        Event::IdleAccepted { .. } => "IDLE accepted",
        Event::IdleRejected { .. } => "IDLE rejected",
        Event::IdleDoneSent { .. } => "DONE sent",
        Event::DataReceived { .. } => "data",
        Event::StatusReceived { .. } => "status",
        Event::ContinuationRequestReceived { .. } => "continuation request",
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, BufReader};

    use super::*;

    #[tokio::test]
    async fn an_answer_over_the_limit_is_given_up() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut theirs = BufReader::new(theirs);
            let mut command = String::new();
            theirs.read_line(&mut command).await.unwrap();
            assert_eq!(command, "C1 NOOP\r\n");
            // 12 KiB of untagged responses, each well-formed and small.
            let flood = b"* 1 EXISTS\r\n".repeat(1024);
            _ = theirs.get_mut().write_all(&flood).await;
            _ = theirs.get_mut().write_all(b"C1 OK done\r\n").await;
        });
        let mut conn = Connection::new(Box::new(ours), false, Duration::from_secs(10));
        conn.max_answer = 4096;

        let err = conn.execute(CommandBody::Noop).await.unwrap_err();
        assert!(
            matches!(&err, Error::Protocol(text) if text.contains("more than")),
            "{err}"
        );
        // What is left of the answer must not be read as the next command's.
        let err = conn.execute(CommandBody::Noop).await.unwrap_err();
        assert!(
            matches!(&err, Error::Protocol(text) if text.contains("half-way")),
            "{err}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn the_limit_is_per_command() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut theirs = BufReader::new(theirs);
            for tag in ["C1", "C2", "C3"] {
                let mut command = String::new();
                theirs.read_line(&mut command).await.unwrap();
                let answer = format!("{}{tag} OK done\r\n", "* 1 EXISTS\r\n".repeat(200));
                theirs.get_mut().write_all(answer.as_bytes()).await.unwrap();
            }
        });
        let mut conn = Connection::new(Box::new(ours), false, Duration::from_secs(10));
        conn.max_answer = 4096;
        for _ in 0..3 {
            let response = conn.execute(CommandBody::Noop).await.unwrap();
            assert_eq!(response.data.len(), 200);
        }
        server.await.unwrap();
    }
}
