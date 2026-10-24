//! One IMAP connection over any async byte stream: writes commands, reads whole responses
//! and parses them with imap-proto.

use std::future::Future;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use imap_proto::{Capability, MailboxDatum, Outcome, ResponseCode, Status};
use secrecy::zeroize::Zeroizing;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::command::{Command, Literals};
use super::framing::{Framer, MAX_RESPONSE};
use super::{Error, Result};

pub(crate) use crate::Stream;
use crate::net::with_timeout;

/// A response other than a status or a continuation request: EXISTS, FETCH, LIST, …
pub(crate) type Data = imap_proto::Response<'static>;

/// How many bytes the server may send in answer to one command. Each response is capped
/// at [`MAX_RESPONSE`], but not how many there are, and a command's whole answer is kept
/// until it completes. The largest answer we ask for is one whole message, so only a
/// broken or hostile server gets near this.
const MAX_ANSWER: usize = 256 * 1024 * 1024;

/// How many messages with changed flags a connection keeps track of while idling; past
/// that it only remembers that flags changed.
const MAX_CHANGED_FLAGS: usize = 1000;

/// A response code (`[UIDVALIDITY 7]`), or the name of one imap-proto doesn't know
/// (`[NOMODSEQ]`), upper-cased.
#[derive(Debug)]
pub(crate) enum Code {
    Known(ResponseCode<'static>),
    Other(String),
}

impl Code {
    fn of(outcome: Outcome<'static>) -> Option<Self> {
        if let Some(code) = outcome.code {
            return Some(Self::Known(code));
        }
        // imap-proto leaves a code it doesn't know in the text.
        let code = outcome
            .information?
            .strip_prefix('[')?
            .split(']')
            .next()?
            .to_owned();
        let (name, args) = code.split_once(' ').unwrap_or((&code, ""));
        if name.eq_ignore_ascii_case("CAPABILITY") {
            return Some(Self::Known(ResponseCode::Capabilities(capabilities(args))));
        }
        (!name.is_empty()).then(|| Self::Other(name.to_ascii_uppercase()))
    }
}

/// What the server sent while a command ran, up to and including its tagged OK.
#[derive(Debug, Default)]
pub(crate) struct Response {
    pub data: Vec<Data>,
    /// Codes from untagged status responses (`* OK [UIDVALIDITY 7]`) and the tagged OK.
    pub codes: Vec<Code>,
    /// Responses that couldn't be parsed and were dropped. Nonzero means `data` may be
    /// missing something; only [`Connection::execute_lossy`] returns such a response.
    pub skipped: usize,
}

/// What is left of a response for the command to decide on.
enum Step {
    /// The server asks for the rest of the command (`+`).
    Continue,
    /// The command's tagged completion.
    Done(Status, Outcome<'static>),
    Other,
}

/// The server's first words.
pub(crate) struct Greeting {
    pub status: Status,
    pub code: Option<Code>,
    pub text: String,
}

pub(crate) struct Connection {
    stream: Box<dyn Stream>,
    framer: Framer,
    literals: Literals,
    last_tag: u32,
    buf: Box<[u8]>,
    timeout: Duration,
    /// Bytes read since the current command was sent, and how many it may have
    /// ([`MAX_ANSWER`] but in tests).
    received: usize,
    max_answer: usize,
    /// Set once a command is given up half-way through its answer, so the rest can't be
    /// taken for the answer to the next one.
    out_of_step: bool,
    /// What the server said about the selected mailbox in passing, in the answers to any
    /// command (servers report new mail and expunges whenever they can): the last EXISTS
    /// less the EXPUNGEs since, and whether it reported a change since the mailbox was
    /// opened. A change is an EXISTS with a new count, an EXPUNGE, a VANISHED, and while
    /// idling a FETCH (outside IDLE that is usually the answer to our own FETCH).
    exists: u32,
    changed: bool,
    /// The sequence numbers of those FETCHes, ascending, kept up to date with EXPUNGEs;
    /// `None` once it can't tell which messages they were (too many, a VANISHED, a line
    /// it couldn't parse).
    changed_flags: Option<Vec<u32>>,
}

impl Connection {
    pub fn new(stream: Box<dyn Stream>, timeout: Duration) -> Self {
        Self {
            stream,
            framer: Framer::default(),
            literals: Literals::default(),
            last_tag: 0,
            buf: vec![0; 64 * 1024].into_boxed_slice(),
            timeout,
            received: 0,
            max_answer: MAX_ANSWER,
            out_of_step: false,
            exists: 0,
            changed: false,
            changed_flags: Some(Vec::new()),
        }
    }

    /// Gives up the stream, e.g. to wrap it in TLS. Any bytes the server sent after the
    /// last response are dropped with the framer, so nothing sent in plaintext can be
    /// mistaken for a reply inside the TLS session.
    pub fn into_stream(self) -> Box<dyn Stream> {
        self.stream
    }

    /// How literals in commands are sent, from the server's capabilities.
    pub fn set_literals(&mut self, literals: Literals) {
        self.literals = literals;
    }

    pub async fn greeting(&mut self) -> Result<Greeting> {
        let raw = self.next_response(false).await?;
        match parse(&raw) {
            Some(Data::Data { status, outcome }) => Ok(Greeting {
                status,
                text: text(&outcome),
                code: Code::of(outcome),
            }),
            _ => Err(Error::Protocol("unexpected greeting".into())),
        }
    }

    /// Sends one command and collects everything up to its tagged completion.
    /// NO and BAD become [`Error::Command`]. If any response couldn't be parsed, the
    /// answer is [`Error::Incomplete`]: most results (LIST, SEARCH, EXAMINE, flags) are
    /// only meaningful whole, and a missing line would read as something deleted.
    pub async fn execute(&mut self, command: Command) -> Result<Response> {
        let name = command.name;
        let response = self.execute_lossy(command).await?;
        if response.skipped > 0 {
            return Err(Error::Incomplete(name));
        }
        Ok(response)
    }

    /// Like [`execute`](Self::execute), but returns an answer with unparsable responses
    /// too, counted in [`Response::skipped`]. Only for results that are useful in part and
    /// whose caller notices what is missing.
    pub async fn execute_lossy(&mut self, command: Command) -> Result<Response> {
        let tag = self.next_tag()?;
        let mut segments = command.encode(&tag, self.literals).into_iter();
        self.write(&segments.next().expect("a command has a first line"))
            .await?;
        let mut response = Response::default();
        loop {
            let raw = self.next_response(true).await?;
            match self.step(&raw, &tag, command.name, &mut response, false)? {
                Step::Continue => match segments.next() {
                    Some(segment) => self.write(&segment).await?,
                    None => tracing::debug!(
                        command = command.name,
                        "ignoring unexpected continuation request"
                    ),
                },
                Step::Done(Status::Ok, outcome) => {
                    response.codes.extend(Code::of(outcome));
                    return Ok(response);
                }
                // Also a NO before all of the command was sent (an APPEND refused before
                // its message): the rest is simply not sent.
                Step::Done(_, outcome) => {
                    return Err(Error::Command {
                        command: command.name,
                        text: text(&outcome),
                    });
                }
                Step::Other => {}
            }
        }
    }

    /// Runs IDLE (RFC 2177) until the server reports a change to the selected mailbox,
    /// `stop` resolves or `max` passes, then ends it with DONE. Returns whether the server
    /// reported a change. Reads while idling have no timeout but `max`: a quiet server is
    /// the point.
    pub async fn idle(&mut self, stop: impl Future<Output = ()>, max: Duration) -> Result<bool> {
        let tag = self.next_tag()?;
        self.write(&[format!("{tag} IDLE\r\n")]).await?;
        // `sleep` copes with any `max`; adding it to an instant may overflow.
        let mut timer = std::pin::pin!(tokio::time::sleep(max));
        let mut stop = std::pin::pin!(stop);
        // Between the server's `+` and our DONE.
        let mut idling = false;
        // What IDLE brings counts only as a change.
        let mut ignored = Response::default();
        loop {
            let raw = if !idling {
                Some(self.next_response(false).await?)
            } else if self.changed {
                None
            } else {
                // Reading is cancel-safe: nothing read is lost if `stop` wins.
                tokio::select! {
                    raw = self.next_response_untimed() => Some(raw?),
                    () = &mut stop => None,
                    () = &mut timer => None,
                }
            };
            let Some(raw) = raw else {
                self.write(&[b"DONE\r\n"]).await?;
                idling = false;
                continue;
            };
            match self.step(&raw, &tag, "IDLE", &mut ignored, true)? {
                Step::Continue => idling = true,
                // Also how a server ends IDLE on its own: a tagged OK before DONE.
                Step::Done(Status::Ok, _) => return Ok(self.changed),
                Step::Done(_, outcome) => {
                    return Err(Error::Command {
                        command: "IDLE",
                        text: text(&outcome),
                    });
                }
                Step::Other => {}
            }
        }
    }

    /// Runs `AUTHENTICATE <mechanism>`, answering the first challenge with `initial`.
    /// A rejection becomes [`Error::AuthFailed`].
    pub async fn authenticate(
        &mut self,
        mechanism: &str,
        initial: Zeroizing<Vec<u8>>,
    ) -> Result<Response> {
        let tag = self.next_tag()?;
        self.write(&[format!("{tag} AUTHENTICATE {mechanism}\r\n")])
            .await?;
        let mut initial = Some(initial);
        // Nothing in the answer is authoritative, so unparsable responses don't matter.
        let mut response = Response::default();
        loop {
            let raw = self.next_response(true).await?;
            match self.step(&raw, &tag, "AUTHENTICATE", &mut response, false)? {
                Step::Continue => {
                    // A second challenge means the server didn't take the answer; cancel
                    // and let it send the tagged NO.
                    let answer = match initial.take() {
                        Some(data) => Zeroizing::new(STANDARD.encode(&*data).into_bytes()),
                        None => Zeroizing::new(b"*".to_vec()),
                    };
                    self.write(&[&answer[..], &b"\r\n"[..]]).await?;
                }
                Step::Done(Status::Ok, outcome) => {
                    response.codes.extend(Code::of(outcome));
                    return Ok(response);
                }
                Step::Done(_, outcome) => return Err(Error::AuthFailed(text(&outcome))),
                Step::Other => {}
            }
        }
    }

    /// Takes in one response to the command tagged `tag`, as every command does: keeps
    /// data in `response` (only tracks it while `idling`) and the codes of untagged
    /// statuses, counts what can't be parsed, fails on BYE. Returns what is left for
    /// the command itself.
    fn step(
        &mut self,
        raw: &[u8],
        tag: &str,
        command: &'static str,
        response: &mut Response,
        idling: bool,
    ) -> Result<Step> {
        let Some(data) = parse(raw) else {
            // One response we can't parse (a buggy BODYSTRUCTURE, say) shouldn't take the
            // whole session down, but the result may now be incomplete; while idling,
            // whatever it was, look at the mailbox (and its flags) again.
            tracing::warn!(
                command,
                bytes = raw.len(),
                "skipping malformed server response"
            );
            response.skipped += 1;
            if idling {
                self.changed = true;
                self.changed_flags = None;
            }
            return Ok(Step::Other);
        };
        match data {
            Data::Continue(_) => return Ok(Step::Continue),
            Data::Done {
                tag: done,
                status,
                outcome,
            } if done.0 == tag => return Ok(Step::Done(status, outcome)),
            Data::Done { tag, .. } => {
                tracing::warn!(tag = %tag.0, "ignoring status for an unknown tag");
            }
            Data::Data { status, outcome } => {
                match status {
                    Status::Bye => return Err(Error::Bye(text(&outcome))),
                    Status::No | Status::Bad => {
                        tracing::warn!(command, ?status, text = %text(&outcome), "server warning");
                    }
                    Status::Ok | Status::PreAuth => {}
                }
                response.codes.extend(Code::of(outcome));
            }
            data => {
                self.track(&data, idling);
                if !idling {
                    response.data.push(data);
                }
            }
        }
        Ok(Step::Other)
    }

    /// A new tag, once the connection is known to be in step (see `out_of_step`): a caller
    /// that kept it after such an error can't use it by mistake.
    fn next_tag(&mut self) -> Result<String> {
        if self.out_of_step {
            return Err(Error::Protocol(
                "connection unusable after a command was given up half-way".into(),
            ));
        }
        self.received = 0;
        self.last_tag += 1;
        Ok(format!("C{}", self.last_tag))
    }

    /// A mailbox was just opened, with `exists` messages: reported changes count from
    /// here.
    pub fn mailbox_opened(&mut self, exists: u32) {
        self.exists = exists;
        self.changed = false;
        self.changed_flags = Some(Vec::new());
    }

    /// The selected mailbox's message count, as of the server's last word.
    pub fn exists(&self) -> u32 {
        self.exists
    }

    /// Whether the server reported a change to the selected mailbox since it was opened.
    pub fn mailbox_changed(&self) -> bool {
        self.changed
    }

    /// The sequence numbers of the messages whose flags the server reported changed in
    /// the selected mailbox while idling, since it was opened or this was last called;
    /// `None` if flags changed but it can't tell where.
    pub fn take_changed_flags(&mut self) -> Option<Vec<u32>> {
        self.changed_flags.replace(Vec::new())
    }

    fn track(&mut self, data: &Data, idling: bool) {
        match data {
            Data::MailboxData(MailboxDatum::Exists(n)) => {
                self.changed |= self.exists != *n;
                self.exists = *n;
            }
            Data::Expunge(seq) => {
                self.exists = self.exists.saturating_sub(1);
                self.changed = true;
                // Later messages move down one.
                let seq = *seq;
                if let Some(changed) = &mut self.changed_flags {
                    let i = changed.partition_point(|&n| n < seq);
                    if changed.get(i) == Some(&seq) {
                        changed.remove(i);
                    }
                    changed[i..].iter_mut().for_each(|n| *n -= 1);
                }
            }
            Data::Vanished { .. } => {
                self.changed = true;
                // Renumbers by UIDs, which the sequence numbers can't be matched with.
                if self.changed_flags.as_ref().is_some_and(|c| !c.is_empty()) {
                    self.changed_flags = None;
                }
            }
            Data::Fetch(seq, _) if idling => {
                self.changed = true;
                if let Some(changed) = &mut self.changed_flags
                    && let Err(i) = changed.binary_search(seq)
                {
                    changed.insert(i, *seq);
                    if changed.len() > MAX_CHANGED_FLAGS {
                        self.changed_flags = None;
                    }
                }
            }
            _ => {}
        }
    }

    /// The next whole response. With `limited`, counts it against the command's answer
    /// (see [`MAX_ANSWER`]).
    async fn next_response(&mut self, limited: bool) -> Result<Vec<u8>> {
        loop {
            if let Some(raw) = self.framer.next() {
                return Ok(raw);
            }
            match tokio::time::timeout(self.timeout, self.read()).await {
                Ok(result) => result?,
                Err(_) => return Err(Error::Timeout),
            }
            if limited && self.received > self.max_answer {
                self.out_of_step = true;
                return Err(Error::Protocol(format!(
                    "server sent more than {} MiB in answer to one command",
                    self.max_answer / (1024 * 1024)
                )));
            }
        }
    }

    /// Like `next_response` without a timeout, for IDLE. Cancel-safe.
    async fn next_response_untimed(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(raw) = self.framer.next() {
                return Ok(raw);
            }
            self.read().await?;
        }
    }

    /// Writes `chunks` one after the other, then flushes.
    async fn write(&mut self, chunks: &[impl AsRef<[u8]>]) -> Result<()> {
        let stream = &mut self.stream;
        with_timeout(self.timeout, async {
            for chunk in chunks {
                stream.write_all(chunk.as_ref()).await?;
            }
            stream.flush().await
        })
        .await?;
        Ok(())
    }

    /// Hands the next bytes from the server to the framer. Cancel-safe: the bytes are
    /// handed over in the same step that reads them.
    async fn read(&mut self) -> Result<()> {
        let n = self.stream.read(&mut self.buf).await?;
        if n == 0 {
            return Err(Error::Closed);
        }
        self.received = self.received.saturating_add(n);
        if self.framer.push(&self.buf[..n]).is_err() {
            self.out_of_step = true;
            return Err(Error::Protocol(format!(
                "server sent a response over {} MiB",
                MAX_RESPONSE / (1024 * 1024)
            )));
        }
        Ok(())
    }
}

/// The response in `raw`, if imap-proto parses all of it.
fn parse(raw: &[u8]) -> Option<Data> {
    match imap_proto::Response::parse(raw) {
        Ok((&[], response)) => Some(response.into_owned()),
        _ => capability_data(raw),
    }
}

/// `* CAPABILITY …`, which imap-proto refuses without IMAP4rev1: a server may offer only
/// IMAP4rev2 (RFC 9051). Also the code `[CAPABILITY …]`, see [`Code::of`].
fn capability_data(raw: &[u8]) -> Option<Data> {
    let line = std::str::from_utf8(raw).ok()?.strip_suffix("\r\n")?;
    let (star, rest) = line.split_at_checked(13)?;
    star.eq_ignore_ascii_case("* CAPABILITY ")
        .then(|| Data::Capabilities(capabilities(rest)))
}

/// A space-separated capability list.
fn capabilities(list: &str) -> Vec<Capability<'static>> {
    list.split(' ')
        .filter(|c| !c.is_empty())
        .map(|c| match c.split_at_checked(5) {
            _ if c.eq_ignore_ascii_case("IMAP4rev1") => Capability::Imap4rev1,
            Some((auth, mechanism)) if auth.eq_ignore_ascii_case("AUTH=") => {
                Capability::Auth(mechanism.to_owned().into())
            }
            _ => Capability::Atom(c.to_owned().into()),
        })
        .collect()
}

fn text(outcome: &Outcome<'_>) -> String {
    outcome
        .information
        .as_deref()
        .unwrap_or_default()
        .to_owned()
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
        let mut conn = Connection::new(Box::new(ours), Duration::from_secs(10));
        conn.max_answer = 4096;

        let err = conn.execute(Command::new("NOOP")).await.unwrap_err();
        assert!(
            matches!(&err, Error::Protocol(text) if text.contains("more than")),
            "{err}"
        );
        // What is left of the answer must not be read as the next command's.
        let err = conn.execute(Command::new("NOOP")).await.unwrap_err();
        assert!(
            matches!(&err, Error::Protocol(text) if text.contains("half-way")),
            "{err}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn changed_flags_follow_expunges() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut theirs = BufReader::new(theirs);
            let mut command = String::new();
            theirs.read_line(&mut command).await.unwrap();
            assert_eq!(command, "C1 IDLE\r\n");
            let reports = "+ idling\r\n* 3 FETCH (FLAGS (\\Seen))\r\n* 4 FETCH (FLAGS ())\r\n\
                           * 1 EXPUNGE\r\n* 2 EXPUNGE\r\n";
            theirs
                .get_mut()
                .write_all(reports.as_bytes())
                .await
                .unwrap();
            command.clear();
            theirs.read_line(&mut command).await.unwrap();
            assert_eq!(command, "DONE\r\n");
            theirs.get_mut().write_all(b"C1 OK done\r\n").await.unwrap();
        });
        let mut conn = Connection::new(Box::new(ours), Duration::from_secs(10));
        conn.mailbox_opened(5);

        assert!(
            conn.idle(std::future::pending(), Duration::from_secs(10))
                .await
                .unwrap()
        );
        // 3 became 2 and then went; 4 became 3 and then 2.
        assert_eq!(conn.take_changed_flags(), Some(vec![2]));
        assert_eq!(conn.take_changed_flags(), Some(Vec::new()));
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
        let mut conn = Connection::new(Box::new(ours), Duration::from_secs(10));
        conn.max_answer = 4096;
        for _ in 0..3 {
            let response = conn.execute(Command::new("NOOP")).await.unwrap();
            assert_eq!(response.data.len(), 200);
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_unknown_code_is_kept_by_name() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut theirs = BufReader::new(theirs);
            let mut command = String::new();
            theirs.read_line(&mut command).await.unwrap();
            let answer =
                "* OK [NOMODSEQ] Sorry\r\n* OK [UIDNEXT 4] x\r\nC1 OK [READ-ONLY] done\r\n";
            theirs.get_mut().write_all(answer.as_bytes()).await.unwrap();
        });
        let mut conn = Connection::new(Box::new(ours), Duration::from_secs(10));
        let response = conn.execute(Command::new("NOOP")).await.unwrap();
        let codes: Vec<String> = response.codes.iter().map(|c| format!("{c:?}")).collect();
        assert_eq!(
            codes,
            [
                r#"Other("NOMODSEQ")"#,
                "Known(UidNext(4))",
                "Known(ReadOnly)"
            ]
        );
        server.await.unwrap();
    }
}
