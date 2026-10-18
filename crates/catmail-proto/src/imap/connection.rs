//! Drives imap-next's sans-IO client over any async byte stream.

use std::time::Duration;

use imap_next::client::{Client, Event, Options};
use imap_next::imap_types::auth::{AuthMechanism, AuthenticateData};
use imap_next::imap_types::command::{Command, CommandBody};
use imap_next::imap_types::core::Tag;
use imap_next::imap_types::response::{Code, Data, Greeting, Status, StatusKind};
use imap_next::{Interrupt, Io, State};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{Error, Result};

/// A byte stream an IMAP connection can run over: TCP, TLS, or an in-memory pipe.
pub(crate) trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

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
    last_tag: u32,
    buf: Box<[u8]>,
    timeout: Duration,
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
            last_tag: 0,
            buf: vec![0; 64 * 1024].into_boxed_slice(),
            timeout,
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
                other => tracing::debug!(?other, "ignoring event before greeting"),
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
        let tag = self.next_tag();
        self.client.enqueue_command(Command {
            tag: tag.clone(),
            body,
        });
        let mut response = Response::default();
        loop {
            match self.next_event(&mut response.skipped).await? {
                Event::CommandSent { .. } => {}
                Event::CommandRejected { status, .. } => {
                    return Err(Error::Command {
                        command,
                        text: status.text().to_string(),
                    });
                }
                Event::DataReceived { data } => response.data.push(data),
                Event::StatusReceived { status } => {
                    if let Some(result) = self.on_status(&tag, command, status, &mut response) {
                        return result.map(|()| response);
                    }
                }
                Event::ContinuationRequestReceived { .. } => {
                    tracing::debug!(command, "ignoring unexpected continuation request");
                }
                other => {
                    return Err(Error::Protocol(format!(
                        "unexpected event during {command}: {other:?}"
                    )));
                }
            }
        }
    }

    /// Runs `AUTHENTICATE <mechanism>`, answering the first challenge with `initial`.
    /// A rejection becomes [`Error::AuthFailed`].
    pub async fn authenticate(
        &mut self,
        mechanism: AuthMechanism<'static>,
        initial: Vec<u8>,
    ) -> Result<Response> {
        let tag = self.next_tag();
        self.client.enqueue_command(Command {
            tag: tag.clone(),
            body: CommandBody::authenticate(mechanism),
        });
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
                        Some(data) => AuthenticateData::r#continue(data),
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
                Event::DataReceived { data } => response.data.push(data),
                Event::StatusReceived { status } => {
                    if let Some(result) =
                        self.on_status(&tag, "AUTHENTICATE", status, &mut response)
                    {
                        return result.map(|()| response);
                    }
                }
                other => {
                    return Err(Error::Protocol(format!(
                        "unexpected event during AUTHENTICATE: {other:?}"
                    )));
                }
            }
        }
    }

    fn next_tag(&mut self) -> Tag<'static> {
        self.last_tag += 1;
        Tag::try_from(format!("C{}", self.last_tag)).expect("valid tag")
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

    /// Counts lines it had to skip in `skipped`.
    async fn next_event(&mut self, skipped: &mut usize) -> Result<Event> {
        loop {
            match self.client.next() {
                Ok(event) => return Ok(event),
                // Commands are small and sent whole before reading, so writing and
                // reading in turn can't deadlock against a server that does the same.
                Err(Interrupt::Io(Io::Output(bytes))) => {
                    let stream = &mut self.stream;
                    with_timeout(self.timeout, async {
                        stream.write_all(&bytes).await?;
                        stream.flush().await
                    })
                    .await?;
                }
                Err(Interrupt::Io(Io::NeedMoreInput)) => {
                    let n = with_timeout(self.timeout, self.stream.read(&mut self.buf)).await?;
                    if n == 0 {
                        return Err(Error::Closed);
                    }
                    self.client.enqueue_input(&self.buf[..n]);
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
}

pub(super) async fn with_timeout<T>(
    timeout: Duration,
    fut: impl Future<Output = std::io::Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(timeout, fut).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(Error::Timeout),
    }
}
