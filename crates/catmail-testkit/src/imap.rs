use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::TestCert;
use crate::mime;

/// One message in a [`FakeMailbox`].
#[derive(Debug, Clone)]
pub struct FakeMessage {
    pub uid: u32,
    /// Wire form, e.g. `\Seen` or `$Junk`.
    pub flags: Vec<String>,
    pub modseq: u64,
    /// The raw message. BODYSTRUCTURE and body sections are derived from it.
    pub raw: Vec<u8>,
    /// IMAP date-time, e.g. `17-Oct-2026 09:30:01 +0800`.
    pub internal_date: String,
}

/// When a message arrived, on 17 Oct 2026 from 09:30: a second apart in UID order, so
/// newest first is the reverse of UID order, as it is with real mail.
fn time_of(uid: u32) -> String {
    let seconds = 9 * 3600 + 30 * 60 + uid;
    assert!(seconds < 24 * 3600, "UID {uid} would arrive the next day");
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[derive(Debug, Clone)]
pub struct FakeMailbox {
    /// Wire name (modified UTF-7 for non-ASCII).
    pub name: String,
    /// LIST attributes, e.g. `\Sent` or `\Noselect`.
    pub attributes: Vec<String>,
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub highest_modseq: u64,
    pub messages: Vec<FakeMessage>,
}

impl FakeMailbox {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            attributes: Vec::new(),
            uidvalidity: 1,
            uidnext: 1,
            highest_modseq: 1,
            messages: Vec::new(),
        }
    }

    pub fn with_attribute(mut self, attribute: &str) -> Self {
        self.attributes.push(attribute.into());
        self
    }

    /// Appends a message and returns its UID.
    pub fn append(&mut self, raw: &[u8], flags: &[&str]) -> u32 {
        let uid = self.uidnext;
        self.uidnext += 1;
        self.highest_modseq += 1;
        self.messages.push(FakeMessage {
            uid,
            flags: flags.iter().map(|f| f.to_string()).collect(),
            modseq: self.highest_modseq,
            raw: raw.to_vec(),
            internal_date: format!("17-Oct-2026 {} +0800", time_of(uid)),
        });
        uid
    }

    /// Appends a simple message with the given subject and returns its UID.
    pub fn append_simple(&mut self, subject: &str) -> u32 {
        let uid = self.uidnext;
        let raw = format!(
            "From: Alice <alice@example.com>\r\nTo: bob@example.com\r\nSubject: {subject}\r\n\
             Date: Sat, 17 Oct 2026 {} +0800\r\nMessage-ID: <{uid}.{subject}@example.com>\r\n\
             \r\nHello\r\n",
            time_of(uid)
        );
        self.append(raw.as_bytes(), &[])
    }

    pub fn message_mut(&mut self, uid: u32) -> &mut FakeMessage {
        self.messages
            .iter_mut()
            .find(|m| m.uid == uid)
            .expect("no such uid")
    }

    pub fn set_flags(&mut self, uid: u32, flags: &[&str]) {
        self.highest_modseq += 1;
        let modseq = self.highest_modseq;
        let msg = self.message_mut(uid);
        msg.flags = flags.iter().map(|f| f.to_string()).collect();
        msg.modseq = modseq;
    }

    pub fn expunge(&mut self, uid: u32) {
        self.messages.retain(|m| m.uid != uid);
        self.highest_modseq += 1;
    }

    /// Sets or clears `flag` on the message, bumping MODSEQ if that changed anything.
    fn change_flag(&mut self, uid: u32, flag: &str, on: bool) -> bool {
        let msg = self.message_mut(uid);
        let has = msg.flags.iter().any(|f| f.eq_ignore_ascii_case(flag));
        if has == on {
            return false;
        }
        if on {
            msg.flags.push(flag.to_owned());
        } else {
            msg.flags.retain(|f| !f.eq_ignore_ascii_case(flag));
        }
        self.highest_modseq += 1;
        let modseq = self.highest_modseq;
        self.message_mut(uid).modseq = modseq;
        true
    }

    /// Removes the messages with UIDs in `uids` (all of them if `None`) that have
    /// `\Deleted`, and returns the `* n EXPUNGE` lines announcing it.
    fn expunge_deleted(&mut self, uids: Option<&[u32]>) -> String {
        let mut out = String::new();
        // Each EXPUNGE renumbers the messages after it, so announce from the end.
        for i in (0..self.messages.len()).rev() {
            let msg = &self.messages[i];
            if msg
                .flags
                .iter()
                .any(|f| f.eq_ignore_ascii_case("\\Deleted"))
                && uids.is_none_or(|uids| uids.contains(&msg.uid))
            {
                self.messages.remove(i);
                out += &format!("* {} EXPUNGE\r\n", i + 1);
            }
        }
        if !out.is_empty() {
            self.highest_modseq += 1;
        }
        out
    }

    /// Simulates the server rebuilding the mailbox: new UIDVALIDITY, UIDs renumbered
    /// from 1.
    pub fn reset_uidvalidity(&mut self, uidvalidity: u32) {
        self.uidvalidity = uidvalidity;
        for (i, msg) in self.messages.iter_mut().enumerate() {
            msg.uid = i as u32 + 1;
        }
        self.uidnext = self.messages.len() as u32 + 1;
        self.highest_modseq += 1;
    }
}

/// Deviations from a well-behaved server.
#[derive(Debug, Clone)]
pub struct Faults {
    /// Advertise CONDSTORE and report MODSEQs.
    pub condstore: bool,
    /// Advertise AUTH=PLAIN (otherwise only LOGIN is available).
    pub auth_plain: bool,
    /// Close the connection without answering when the n-th header FETCH arrives
    /// (1-based, counted across connections). Fires once.
    pub drop_on_fetch: Option<usize>,
    /// Never answer header FETCHes; the connection stays open.
    pub hang_on_fetch: bool,
    /// Hold each header FETCH until it gets a permit, e.g. to keep a sync from finishing.
    pub fetch_gate: Option<Arc<Semaphore>>,
    /// Like `drop_on_fetch`, for FETCHes of body content (`BODY.PEEK[]`,
    /// `BODY.PEEK[<section>]`).
    pub drop_on_body_fetch: Option<usize>,
    /// Never answer body FETCHes; the connection stays open.
    pub hang_on_body_fetch: bool,
    /// Hold each body FETCH until it gets a permit, e.g. to keep several connections
    /// busy at once.
    pub body_fetch_gate: Option<Arc<Semaphore>>,
    /// Answer `BODY.PEEK[<section>]` with NIL, as if the part had no content.
    pub nil_sections: bool,
    /// Send every FETCH response twice.
    pub duplicate_fetch: bool,
    /// Report each RFC822.SIZE one byte larger than the message, as some servers count
    /// differently than the bytes they send.
    pub inexact_sizes: bool,
    /// Send FETCH responses in descending UID order.
    pub reverse_fetch: bool,
    /// Interleave unsolicited `* n FETCH (FLAGS …)` without UID, and `* n EXISTS`.
    pub unsolicited: bool,
    /// Right after `OK Begin TLS`, also send a forged CAPABILITY in plaintext.
    pub inject_after_starttls: bool,
    /// Reject every login.
    pub reject_login: bool,
    /// Greet with PREAUTH: the connection starts out authenticated.
    pub preauth: bool,
    /// Replace the first untagged response line containing this text with one no
    /// parser accepts, e.g. `"* SEARCH"` or `"* LIST"`. Fires once.
    pub garble: Option<String>,
    /// Like `garble`, but leave the line out entirely, as a non-conforming server would.
    pub omit: Option<String>,
    /// Split each FETCH response in two: UID + FLAGS + MODSEQ, then UID + the rest.
    pub split_fetch: bool,
    /// UIDs whose BODYSTRUCTURE is sent in a form no parser accepts, every time (unlike
    /// `garble`, which fires once). The rest of their FETCH responses is well-formed.
    pub broken_bodystructure: Vec<u32>,
    /// Advertise UIDPLUS and answer APPEND with APPENDUID.
    pub uidplus: bool,
    /// Refuse this many APPENDs (with NO) before accepting them again.
    pub reject_appends: usize,
    /// Refuse APPENDs with NO right away, instead of asking for the message (`+`), as
    /// Dovecot does when it can't store anything.
    pub reject_append_literal: bool,
    /// Store the next APPENDed message, then close the connection without answering:
    /// the client can't tell whether it was saved. Fires once.
    pub drop_after_append: bool,
    /// Store the next APPENDed message and answer OK, but with a line no parser accepts
    /// before that. Fires once.
    pub garble_append: bool,
    /// Advertise MOVE (otherwise clients have to COPY, mark `\Deleted` and expunge).
    pub move_command: bool,
    /// Refuse STORE, COPY and MOVE with `NO <text>`, e.g. `[CANNOT] Permission denied`.
    pub refuse_changes: Option<String>,
    /// Refuse this many STORE, COPY and MOVE commands, with `NO [CANNOT] Refused`, before
    /// making them again.
    pub refuse_next_changes: usize,
    /// Make the next STORE, COPY, MOVE or EXPUNGE, then close the connection without
    /// answering. Fires once.
    pub drop_after_change: bool,
    /// Close the connection when the next STORE, COPY, MOVE or EXPUNGE arrives, without
    /// making it. Fires once.
    pub drop_before_change: bool,
    /// Like `drop_after_change`, but only for the next EXPUNGE (or UID EXPUNGE): the
    /// changes before it are answered. Fires once.
    pub drop_after_expunge: bool,
    /// When the next UID COPY arrives, first add this message to the mailbox with this
    /// name, as another client might right then. Fires once.
    pub deliver_before_copy: Option<(String, Vec<u8>)>,
    /// Answer COPY and MOVE with `[COPYUID <this>]` instead of the true one, e.g.
    /// `"1 1:4294967295 1:4294967295"`.
    pub forged_copyuid: Option<String>,
    /// Advertise IDLE, and report changes to the selected mailbox while idling.
    pub idle: bool,
    /// Advertise `APPENDLIMIT=<n>` (RFC 7889). Larger APPENDs are not refused.
    pub append_limit: Option<u64>,
    /// Accept every IDLE, then close the connection, as a misbehaving proxy might.
    pub drop_on_idle: bool,
    /// Accept every IDLE, then end it at once with OK, before the client's DONE.
    pub end_idle_early: bool,
    /// Accept every IDLE, then send a line no parser accepts.
    pub garble_idle: bool,
    /// Answer each command that starts with this text (`EXAMINE`) only after this long.
    pub slow: Option<(String, std::time::Duration)>,
}

impl Default for Faults {
    fn default() -> Self {
        Self {
            condstore: true,
            auth_plain: false,
            drop_on_fetch: None,
            hang_on_fetch: false,
            fetch_gate: None,
            drop_on_body_fetch: None,
            hang_on_body_fetch: false,
            body_fetch_gate: None,
            nil_sections: false,
            duplicate_fetch: false,
            inexact_sizes: false,
            reverse_fetch: false,
            unsolicited: false,
            inject_after_starttls: false,
            reject_login: false,
            preauth: false,
            garble: None,
            omit: None,
            split_fetch: false,
            broken_bodystructure: Vec::new(),
            uidplus: true,
            reject_appends: 0,
            reject_append_literal: false,
            drop_after_append: false,
            garble_append: false,
            move_command: true,
            refuse_changes: None,
            refuse_next_changes: 0,
            drop_after_change: false,
            drop_before_change: false,
            drop_after_expunge: false,
            deliver_before_copy: None,
            forged_copyuid: None,
            idle: true,
            append_limit: None,
            drop_on_idle: false,
            end_idle_early: false,
            garble_idle: false,
            slow: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedCommand {
    /// Whether the command arrived over TLS.
    pub tls: bool,
    /// The command line without the tag; literals inlined.
    pub line: String,
}

#[derive(Debug)]
pub struct ServerState {
    pub username: String,
    pub password: String,
    pub mailboxes: Vec<FakeMailbox>,
    pub faults: Faults,
    pub log: Vec<LoggedCommand>,
    /// Connections accepted so far.
    pub connections: usize,
    /// Connections open now.
    pub open_connections: usize,
    header_fetches: usize,
    body_fetches: usize,
    /// Connections numbered up to this one are closed at their next command.
    dropped_through: usize,
}

impl ServerState {
    pub fn new(username: &str, password: &str) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            mailboxes: vec![FakeMailbox::new("INBOX")],
            faults: Faults::default(),
            log: Vec::new(),
            connections: 0,
            open_connections: 0,
            header_fetches: 0,
            body_fetches: 0,
            dropped_through: 0,
        }
    }

    pub fn mailbox_mut(&mut self, name: &str) -> &mut FakeMailbox {
        self.mailboxes
            .iter_mut()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("no mailbox {name}"))
    }

    /// Closes every connection open now, as a server or NAT box does with idle ones:
    /// each answers its next command by hanging up.
    pub fn drop_open_connections(&mut self) {
        self.dropped_through = self.connections;
    }

    pub fn inbox(&mut self) -> &mut FakeMailbox {
        self.mailbox_mut("INBOX")
    }

    /// How many commands the server received that start with `prefix` (`IDLE`, `LIST`).
    pub fn received(&self, prefix: &str) -> usize {
        self.log
            .iter()
            .filter(|c| c.line.starts_with(prefix))
            .count()
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.mailboxes.iter().position(|m| {
            m.name == name || (name.eq_ignore_ascii_case("INBOX") && m.name == "INBOX")
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TlsMode {
    None,
    Implicit,
    StartTls,
}

/// A running fake server. Dropping it stops accepting connections.
pub struct FakeImap {
    addr: SocketAddr,
    state: Arc<Mutex<ServerState>>,
    task: JoinHandle<()>,
}

impl Drop for FakeImap {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeImap {
    /// Plaintext server on 127.0.0.1.
    pub async fn start(state: ServerState) -> Self {
        Self::spawn(state, TlsMode::None, None).await
    }

    /// TLS from the first byte.
    pub async fn start_tls(state: ServerState, cert: &TestCert) -> Self {
        Self::spawn(state, TlsMode::Implicit, Some(cert.clone())).await
    }

    /// Plaintext with STARTTLS; LOGIN is refused until TLS is up.
    pub async fn start_starttls(state: ServerState, cert: &TestCert) -> Self {
        Self::spawn(state, TlsMode::StartTls, Some(cert.clone())).await
    }

    async fn spawn(state: ServerState, mode: TlsMode, cert: Option<TestCert>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(Mutex::new(state));
        let task = tokio::spawn({
            let state = state.clone();
            async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let state = state.clone();
                    let cert = cert.clone();
                    tokio::spawn(async move {
                        let id = {
                            let mut st = lock(&state);
                            st.connections += 1;
                            st.open_connections += 1;
                            st.connections
                        };
                        let result = match (mode, &cert) {
                            (TlsMode::Implicit, Some(cert)) => {
                                match cert.acceptor().accept(tcp).await {
                                    Ok(tls) => {
                                        serve(Box::new(tls), id, true, mode, None, state.clone())
                                            .await
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                            _ => serve(Box::new(tcp), id, false, mode, cert, state.clone()).await,
                        };
                        lock(&state).open_connections -= 1;
                        if let Err(e) = result {
                            tracing::debug!(error = %e, "fake IMAP connection ended");
                        }
                    });
                }
            }
        });
        Self { addr, state, task }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Locks the server state, e.g. to add messages or set faults between syncs.
    pub fn state(&self) -> MutexGuard<'_, ServerState> {
        lock(&self.state)
    }

    /// Waits until `ready` holds for the server state, looking every few milliseconds.
    /// Panics after 10 seconds.
    pub async fn wait_until(&self, ready: impl Fn(&ServerState) -> bool) {
        let poll = async {
            while !ready(&self.state()) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        };
        if tokio::time::timeout(std::time::Duration::from_secs(10), poll)
            .await
            .is_err()
        {
            panic!("the server never got into the state waited for");
        }
    }
}

fn lock(state: &Mutex<ServerState>) -> MutexGuard<'_, ServerState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

struct Session {
    id: usize,
    tls: bool,
    mode: TlsMode,
    authenticated: bool,
    selected: Option<usize>,
    /// Opened with SELECT rather than EXAMINE.
    writable: bool,
    condstore_enabled: bool,
    /// The selected mailbox as this connection was last told about it.
    view: Option<View>,
}

/// What a connection knows about its selected mailbox. Changes others make since are
/// reported like a server does: new messages with the answer to any command, the rest
/// while idling.
#[derive(Debug, Clone, PartialEq)]
struct View {
    uidvalidity: u32,
    /// (UID, flags), in mailbox order.
    messages: Vec<(u32, Vec<String>)>,
}

impl View {
    fn of(mb: &FakeMailbox) -> Self {
        Self {
            uidvalidity: mb.uidvalidity,
            messages: mb
                .messages
                .iter()
                .map(|m| (m.uid, m.flags.clone()))
                .collect(),
        }
    }

    /// The untagged responses that bring this view up to `now`, which it then becomes:
    /// EXPUNGEs, then EXISTS, then FETCH for changed flags.
    fn catch_up(&mut self, now: View) -> String {
        let mut out = String::new();
        for i in (0..self.messages.len()).rev() {
            let uid = self.messages[i].0;
            if !now.messages.iter().any(|(u, _)| *u == uid) {
                self.messages.remove(i);
                out += &format!("* {} EXPUNGE\r\n", i + 1);
            }
        }
        if now.messages.len() != self.messages.len() {
            out += &format!("* {} EXISTS\r\n", now.messages.len());
        }
        for (i, (uid, flags)) in now.messages.iter().enumerate() {
            let known = self.messages.iter().find(|(u, _)| u == uid);
            if known.is_some_and(|(_, f)| f != flags) {
                out += &format!(
                    "* {} FETCH (UID {uid} FLAGS ({}))\r\n",
                    i + 1,
                    flags.join(" ")
                );
            }
        }
        *self = now;
        out
    }
}

enum Next {
    Continue,
    Close,
    StartTls,
}

async fn serve(
    stream: Box<dyn Stream>,
    id: usize,
    tls: bool,
    mode: TlsMode,
    cert: Option<TestCert>,
    state: Arc<Mutex<ServerState>>,
) -> std::io::Result<()> {
    let mut io = BufReader::new(stream);
    let mut session = Session {
        id,
        tls,
        mode,
        authenticated: false,
        selected: None,
        writable: false,
        condstore_enabled: false,
        view: None,
    };
    if lock(&state).faults.preauth {
        session.authenticated = true;
        io.get_mut()
            .write_all(b"* PREAUTH fake IMAP ready\r\n")
            .await?;
    } else {
        io.get_mut().write_all(b"* OK fake IMAP ready\r\n").await?;
    }
    loop {
        let refuse = lock(&state).faults.reject_append_literal;
        let Some((line, literals)) = read_command(&mut io, refuse).await? else {
            return Ok(());
        };
        match handle(&mut io, &mut session, &state, &line, literals).await? {
            Next::Continue => {}
            Next::Close => return Ok(()),
            Next::StartTls => {
                let cert = cert.as_ref().expect("STARTTLS needs a certificate");
                // Dropping the BufReader discards anything the client pipelined in
                // plaintext, as a careful server must.
                let tls = cert.acceptor().accept(io.into_inner()).await?;
                io = BufReader::new(Box::new(tls));
                session.tls = true;
            }
        }
    }
}

/// Reads one command line, inlining `{n}` / `{n+}` literals, which are also returned as
/// they came (APPEND needs the exact bytes). `None` at EOF. With `refuse_append`, an
/// APPEND is answered with NO instead of being asked for its message, and skipped.
async fn read_command(
    io: &mut BufReader<Box<dyn Stream>>,
    refuse_append: bool,
) -> std::io::Result<Option<(Vec<u8>, Vec<Vec<u8>>)>> {
    let mut line = Vec::new();
    let mut literals = Vec::new();
    loop {
        let mut chunk = Vec::new();
        if io.read_until(b'\n', &mut chunk).await? == 0 {
            return Ok(None);
        }
        while matches!(chunk.last(), Some(b'\n' | b'\r')) {
            chunk.pop();
        }
        let literal = parse_literal_suffix(&chunk);
        match literal {
            Some((len, non_sync, prefix_len)) => {
                line.extend_from_slice(&chunk[..prefix_len]);
                let text = String::from_utf8_lossy(&line).to_ascii_uppercase();
                if refuse_append && !non_sync && text.split(' ').nth(1) == Some("APPEND") {
                    let tag = text.split(' ').next().unwrap_or_default();
                    io.get_mut()
                        .write_all(format!("{tag} NO [SERVERBUG] Internal error\r\n").as_bytes())
                        .await?;
                    // Answered; read the next command instead.
                    line.clear();
                    literals.clear();
                    continue;
                }
                if !non_sync {
                    io.get_mut()
                        .write_all(b"+ Ready for literal data\r\n")
                        .await?;
                }
                let mut data = vec![0; len];
                io.read_exact(&mut data).await?;
                // Inline as a quoted string so the tokenizer sees one token.
                line.push(b'"');
                for &b in &data {
                    if b == b'"' || b == b'\\' {
                        line.push(b'\\');
                    }
                    line.push(b);
                }
                line.push(b'"');
                literals.push(data);
            }
            None => {
                line.extend_from_slice(&chunk);
                return Ok(Some((line, literals)));
            }
        }
    }
}

/// `… {12}` → (12, false, offset of `{`).
fn parse_literal_suffix(chunk: &[u8]) -> Option<(usize, bool, usize)> {
    if chunk.last() != Some(&b'}') {
        return None;
    }
    let open = chunk.iter().rposition(|&b| b == b'{')?;
    let inner = std::str::from_utf8(&chunk[open + 1..chunk.len() - 1]).ok()?;
    let (digits, non_sync) = match inner.strip_suffix('+') {
        Some(d) => (d, true),
        None => (inner, false),
    };
    Some((digits.parse().ok()?, non_sync, open))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Atom(String),
    Str(String),
    List(Vec<Tok>),
}

impl Tok {
    fn text(&self) -> String {
        match self {
            Tok::Atom(s) | Tok::Str(s) => s.clone(),
            Tok::List(items) => items.iter().map(Tok::text).collect::<Vec<_>>().join(" "),
        }
    }
}

fn tokenize(input: &str) -> Vec<Tok> {
    fn parse(
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        until: Option<char>,
    ) -> Vec<Tok> {
        let mut out = Vec::new();
        while let Some(&c) = chars.peek() {
            match c {
                ' ' => {
                    chars.next();
                }
                ')' if until == Some(')') => {
                    chars.next();
                    return out;
                }
                '(' => {
                    chars.next();
                    out.push(Tok::List(parse(chars, Some(')'))));
                }
                '"' => {
                    chars.next();
                    let mut s = String::new();
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => s.extend(chars.next()),
                            '"' => break,
                            c => s.push(c),
                        }
                    }
                    out.push(Tok::Str(s));
                }
                _ => {
                    // Brackets may contain spaces: BODY.PEEK[HEADER.FIELDS (DATE FROM)].
                    let mut s = String::new();
                    let mut depth = 0;
                    while let Some(&c) = chars.peek() {
                        if depth == 0 && matches!(c, ' ' | '(' | ')') {
                            break;
                        }
                        match c {
                            '[' => depth += 1,
                            ']' => depth -= 1,
                            _ => {}
                        }
                        s.push(c);
                        chars.next();
                    }
                    if s.is_empty() {
                        chars.next(); // A stray `)`.
                    } else {
                        out.push(Tok::Atom(s));
                    }
                }
            }
        }
        out
    }
    parse(&mut input.chars().peekable(), None)
}

fn capabilities(state: &ServerState, session: &Session) -> String {
    let append_limit = state
        .faults
        .append_limit
        .map(|l| format!("APPENDLIMIT={l}"));
    let mut caps = vec!["IMAP4rev1"];
    if session.mode == TlsMode::StartTls && !session.tls {
        caps.extend(["STARTTLS", "LOGINDISABLED"]);
    }
    if state.faults.auth_plain {
        caps.push("AUTH=PLAIN");
    }
    if state.faults.condstore {
        caps.push("CONDSTORE");
    }
    if state.faults.uidplus {
        caps.push("UIDPLUS");
    }
    if state.faults.move_command {
        caps.push("MOVE");
    }
    if state.faults.idle {
        caps.push("IDLE");
    }
    caps.extend(append_limit.as_deref());
    caps.join(" ")
}

async fn handle(
    io: &mut BufReader<Box<dyn Stream>>,
    session: &mut Session,
    state: &Arc<Mutex<ServerState>>,
    raw: &[u8],
    literals: Vec<Vec<u8>>,
) -> std::io::Result<Next> {
    let line = String::from_utf8_lossy(raw).into_owned();
    let (tag, rest) = line.split_once(' ').unwrap_or((&line, ""));
    let tag = tag.to_owned();
    if session.id <= lock(state).dropped_through {
        return Ok(Next::Close);
    }
    lock(state).log.push(LoggedCommand {
        tls: session.tls,
        line: rest.to_owned(),
    });
    let slow = lock(state)
        .faults
        .slow
        .clone()
        .filter(|(prefix, _)| rest.starts_with(prefix.as_str()));
    if let Some((_, delay)) = slow {
        tokio::time::sleep(delay).await;
    }
    let toks = tokenize(rest);
    let word = |i: usize| {
        toks.get(i)
            .map(|t| t.text().to_ascii_uppercase())
            .unwrap_or_default()
    };
    let mut out = String::new();
    let mut fetched: Option<Vec<u8>> = None;
    let mut next = Next::Continue;

    let command = word(0);
    let (command, args) = if command == "UID" {
        (format!("UID {}", word(1)), &toks[2.min(toks.len())..])
    } else {
        (command, &toks[1.min(toks.len())..])
    };

    match command.as_str() {
        "CAPABILITY" => {
            let caps = capabilities(&lock(state), session);
            out += &format!("* CAPABILITY {caps}\r\n{tag} OK CAPABILITY completed\r\n");
        }
        "NOOP" => out += &format!("{tag} OK NOOP completed\r\n"),
        "LOGOUT" => {
            out += &format!("* BYE logging out\r\n{tag} OK LOGOUT completed\r\n");
            next = Next::Close;
        }
        "STARTTLS" if session.mode == TlsMode::StartTls && !session.tls => {
            out += &format!("{tag} OK Begin TLS negotiation now\r\n");
            if lock(state).faults.inject_after_starttls {
                out += "* CAPABILITY IMAP4rev1 X-INJECTED AUTH=PLAIN\r\n";
            }
            next = Next::StartTls;
        }
        "LOGIN" | "AUTHENTICATE" if session.authenticated => {
            out += &format!("{tag} BAD Already authenticated\r\n");
        }
        "LOGIN" => {
            let st = lock(state);
            if session.mode == TlsMode::StartTls && !session.tls {
                out += &format!("{tag} NO [PRIVACYREQUIRED] LOGIN disabled before STARTTLS\r\n");
            } else if !st.faults.reject_login
                && args.first().map(Tok::text).as_deref() == Some(st.username.as_str())
                && args.get(1).map(Tok::text).as_deref() == Some(st.password.as_str())
            {
                session.authenticated = true;
                let caps = capabilities(&st, session);
                out += &format!("{tag} OK [CAPABILITY {caps}] Logged in\r\n");
            } else {
                out += &format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n");
            }
        }
        "AUTHENTICATE" if word(1) == "PLAIN" => {
            io.get_mut().write_all(b"+ \r\n").await?;
            let mut response = String::new();
            io.read_line(&mut response).await?;
            let decoded = BASE64_STANDARD
                .decode(response.trim_end())
                .unwrap_or_default();
            let parts: Vec<&[u8]> = decoded.split(|&b| b == 0).collect();
            let st = lock(state);
            let ok = !st.faults.reject_login
                && parts.len() == 3
                && parts[1] == st.username.as_bytes()
                && parts[2] == st.password.as_bytes();
            if ok {
                session.authenticated = true;
                out += &format!("{tag} OK Logged in\r\n");
            } else {
                out += &format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n");
            }
        }
        _ if !session.authenticated => out += &format!("{tag} NO Not authenticated\r\n"),
        "IDLE" if lock(state).faults.idle => return idle(io, session, state, &tag).await,
        "LIST" => {
            let st = lock(state);
            for mb in &st.mailboxes {
                let attrs = mb.attributes.join(" ");
                out += &format!("* LIST ({attrs}) \"/\" {}\r\n", quote(&mb.name));
            }
            out += &format!("{tag} OK LIST completed\r\n");
        }
        "EXAMINE" | "SELECT" => {
            let st = lock(state);
            let name = args.first().map(Tok::text).unwrap_or_default();
            match st.find(&name) {
                Some(i)
                    if !st.mailboxes[i]
                        .attributes
                        .iter()
                        .any(|a| a.eq_ignore_ascii_case("\\Noselect")) =>
                {
                    let mb = &st.mailboxes[i];
                    session.selected = Some(i);
                    session.view = Some(View::of(mb));
                    session.writable = command == "SELECT";
                    session.condstore_enabled = st.faults.condstore
                        && args
                            .get(1)
                            .is_some_and(|t| t.text().eq_ignore_ascii_case("CONDSTORE"));
                    out += "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n";
                    out += &format!("* {} EXISTS\r\n* 0 RECENT\r\n", mb.messages.len());
                    out += &format!("* OK [UIDVALIDITY {}] UIDs valid\r\n", mb.uidvalidity);
                    out += &format!("* OK [UIDNEXT {}] Predicted next UID\r\n", mb.uidnext);
                    if st.faults.condstore {
                        out += &format!("* OK [HIGHESTMODSEQ {}] Highest\r\n", mb.highest_modseq);
                    }
                    let access = if session.writable {
                        "READ-WRITE"
                    } else {
                        "READ-ONLY"
                    };
                    out += &format!("{tag} OK [{access}] {command} completed\r\n");
                }
                _ => {
                    session.selected = None;
                    session.view = None;
                    out += &format!("{tag} NO Mailbox doesn't exist: {name}\r\n");
                }
            }
        }
        "APPEND" => {
            let st = &mut *lock(state);
            let name = args.first().map(Tok::text).unwrap_or_default();
            let flags: Vec<String> = match args.get(1) {
                Some(Tok::List(flags)) => flags.iter().map(Tok::text).collect(),
                _ => Vec::new(),
            };
            match (st.find(&name), literals.last()) {
                (Some(_), Some(_)) if st.faults.reject_appends > 0 => {
                    st.faults.reject_appends -= 1;
                    out += &format!("{tag} NO [OVERQUOTA] Quota exceeded\r\n");
                }
                (Some(i), Some(raw)) => {
                    let mb = &mut st.mailboxes[i];
                    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
                    let uid = mb.append(raw, &flags);
                    if std::mem::take(&mut st.faults.drop_after_append) {
                        return Ok(Next::Close);
                    }
                    if std::mem::take(&mut st.faults.garble_append) {
                        out += "* GARBLED (((\r\n";
                    }
                    let code = if st.faults.uidplus {
                        format!("[APPENDUID {} {uid}] ", st.mailboxes[i].uidvalidity)
                    } else {
                        String::new()
                    };
                    out += &format!("{tag} OK {code}APPEND completed\r\n");
                }
                (None, _) => out += &format!("{tag} NO [TRYCREATE] No such mailbox\r\n"),
                (_, None) => out += &format!("{tag} BAD Missing message\r\n"),
            }
        }
        "STATUS" => {
            let st = lock(state);
            let name = args.first().map(Tok::text).unwrap_or_default();
            match st.find(&name) {
                Some(i) => {
                    let mb = &st.mailboxes[i];
                    out += &format!(
                        "* STATUS {} (UIDVALIDITY {} UIDNEXT {})\r\n{tag} OK STATUS completed\r\n",
                        quote(&mb.name),
                        mb.uidvalidity,
                        mb.uidnext
                    );
                }
                None => out += &format!("{tag} NO Mailbox doesn't exist: {name}\r\n"),
            }
        }
        "UID SEARCH" => {
            let st = lock(state);
            match session.selected.and_then(|i| st.mailboxes.get(i)) {
                Some(mb) => {
                    // `ALL`, `UID <set>`, or `HEADER <field> <substring>`.
                    let key = |i: usize| args.get(i).map(Tok::text);
                    let first = key(0).unwrap_or_default().to_ascii_uppercase();
                    let set = (first == "UID").then(|| key(1)).flatten();
                    let header = (first == "HEADER").then(|| key(1).zip(key(2))).flatten();
                    let uids: Vec<String> = mb
                        .messages
                        .iter()
                        .filter(|m| {
                            set.as_ref()
                                .is_none_or(|set| uid_set_contains(set, m.uid, mb))
                        })
                        .filter(|m| {
                            header.as_ref().is_none_or(|(field, value)| {
                                let found = filter_headers(&m.raw, &[field.to_ascii_lowercase()]);
                                String::from_utf8_lossy(&found)
                                    .to_ascii_lowercase()
                                    .contains(&value.to_ascii_lowercase())
                            })
                        })
                        .map(|m| m.uid.to_string())
                        .collect();
                    out += &format!("* SEARCH {}\r\n", uids.join(" "))
                        .replace("SEARCH \r", "SEARCH\r");
                    out += &format!("{tag} OK SEARCH completed\r\n");
                }
                None => out += &format!("{tag} BAD No mailbox selected\r\n"),
            }
        }
        "UID STORE" | "UID COPY" | "UID MOVE" | "UID EXPUNGE" | "EXPUNGE" => {
            let st = &mut *lock(state);
            if command == "UID COPY"
                && let Some((mailbox, raw)) = st.faults.deliver_before_copy.take()
            {
                st.mailbox_mut(&mailbox).append(&raw, &[]);
            }
            let refusal = st.faults.refuse_changes.clone();
            match session.selected {
                None => out += &format!("{tag} BAD No mailbox selected\r\n"),
                Some(_) if !session.writable => {
                    out += &format!("{tag} NO [READ-ONLY] Mailbox is read-only\r\n");
                }
                Some(_)
                    if (refusal.is_some() || st.faults.refuse_next_changes > 0)
                        && !command.ends_with("EXPUNGE") =>
                {
                    let refusal = refusal.unwrap_or_else(|| {
                        st.faults.refuse_next_changes -= 1;
                        "[CANNOT] Refused".into()
                    });
                    out += &format!("{tag} NO {refusal}\r\n");
                }
                Some(_) if std::mem::take(&mut st.faults.drop_before_change) => {
                    return Ok(Next::Close);
                }
                Some(i) => {
                    out += &change(st, i, &command, args, &tag);
                    // Its own changes are in the answer (or silent, as asked).
                    session.view = Some(View::of(&st.mailboxes[i]));
                    if std::mem::take(&mut st.faults.drop_after_change)
                        || (command.ends_with("EXPUNGE")
                            && std::mem::take(&mut st.faults.drop_after_expunge))
                    {
                        return Ok(Next::Close);
                    }
                }
            }
        }
        // By sequence number, as the client knows them (it may not have been told of
        // every expunge yet); only UID.
        "FETCH" => match &session.view {
            Some(view)
                if fetch_items(args)
                    .iter()
                    .all(|i| i.eq_ignore_ascii_case("UID")) =>
            {
                let set = args.first().map(Tok::text).unwrap_or_default();
                for (i, (uid, _)) in view.messages.iter().enumerate() {
                    let seq = i as u32 + 1;
                    if set_contains(&set, seq, || view.messages.len() as u32) {
                        out += &format!("* {seq} FETCH (UID {uid})\r\n");
                    }
                }
                out += &format!("{tag} OK FETCH completed\r\n");
            }
            Some(_) => out += &format!("{tag} BAD Only UID by sequence number\r\n"),
            None => out += &format!("{tag} BAD No mailbox selected\r\n"),
        },
        "UID FETCH" => {
            let items = fetch_items(args);
            if header_item(&items).is_some() {
                let hang = {
                    let st = &mut *lock(state);
                    let f = &mut st.faults;
                    count_fetch(
                        &mut st.header_fetches,
                        &mut f.drop_on_fetch,
                        f.hang_on_fetch,
                    )
                };
                match hang {
                    None => return Ok(Next::Close),
                    Some(true) => std::future::pending::<()>().await,
                    Some(false) => {}
                }
                let gate = lock(state).faults.fetch_gate.clone();
                if let Some(gate) = gate {
                    gate.acquire().await.expect("gate open").forget();
                }
            }
            if items.iter().any(|i| body_section(i).is_some()) {
                let hang = {
                    let st = &mut *lock(state);
                    let f = &mut st.faults;
                    count_fetch(
                        &mut st.body_fetches,
                        &mut f.drop_on_body_fetch,
                        f.hang_on_body_fetch,
                    )
                };
                match hang {
                    None => return Ok(Next::Close),
                    Some(true) => std::future::pending::<()>().await,
                    Some(false) => {}
                }
                let gate = lock(state).faults.body_fetch_gate.clone();
                if let Some(gate) = gate {
                    gate.acquire().await.expect("gate open").forget();
                }
            }
            let st = lock(state);
            match session.selected.and_then(|i| st.mailboxes.get(i)) {
                // Header literals may not be UTF-8, so FETCH output is bytes.
                Some(mb) => {
                    fetched = Some(fetch(mb, args, session.condstore_enabled, &st.faults, &tag))
                }
                None => out += &format!("{tag} BAD No mailbox selected\r\n"),
            }
        }
        _ => out += &format!("{tag} BAD Unknown command\r\n"),
    }
    let mut out = fetched.unwrap_or_else(|| out.into_bytes());
    if !matches!(command.as_str(), "EXAMINE" | "SELECT" | "LOGOUT") {
        let arrived = new_messages(session, &lock(state));
        insert_before_last_line(&mut out, arrived.as_bytes());
    }
    let out = garble(&mut lock(state), out);
    io.get_mut().write_all(&out).await?;
    io.get_mut().flush().await?;
    Ok(next)
}

/// `* n EXISTS` if messages arrived in the selected mailbox since the connection was last
/// told, as servers report new mail with the answer to any command. (Other changes wait
/// for IDLE: a server can't announce expunges during FETCH, STORE or SEARCH.)
fn new_messages(session: &mut Session, st: &ServerState) -> String {
    let (Some(i), Some(view)) = (session.selected, &mut session.view) else {
        return String::new();
    };
    let Some(mb) = st.mailboxes.get(i) else {
        return String::new();
    };
    let new: Vec<_> = mb
        .messages
        .iter()
        .filter(|m| m.uid > view.messages.last().map_or(0, |(u, _)| *u))
        .map(|m| (m.uid, m.flags.clone()))
        .collect();
    if new.is_empty() || mb.uidvalidity != view.uidvalidity {
        return String::new();
    }
    view.messages.extend(new);
    format!("* {} EXISTS\r\n", view.messages.len())
}

/// Inserts `lines` before the last line of `out`, the tagged answer.
fn insert_before_last_line(out: &mut Vec<u8>, lines: &[u8]) {
    if lines.is_empty() {
        return;
    }
    let body = &out[..out.len().saturating_sub(2)];
    let at = body
        .windows(2)
        .rposition(|w| w == b"\r\n")
        .map_or(0, |i| i + 2);
    out.splice(at..at, lines.iter().copied());
}

/// IDLE until DONE: reports changes to the selected mailbox as they happen. A new
/// UIDVALIDITY ends the connection with BYE, as Dovecot does; so does
/// [`ServerState::drop_open_connections`] (without the BYE).
async fn idle(
    io: &mut BufReader<Box<dyn Stream>>,
    session: &mut Session,
    state: &Arc<Mutex<ServerState>>,
    tag: &str,
) -> std::io::Result<Next> {
    io.get_mut().write_all(b"+ idling\r\n").await?;
    io.get_mut().flush().await?;
    let faults = lock(state).faults.clone();
    if faults.drop_on_idle {
        return Ok(Next::Close);
    }
    if faults.end_idle_early {
        let answer = format!("{tag} OK IDLE terminated\r\n");
        io.get_mut().write_all(answer.as_bytes()).await?;
        io.get_mut().flush().await?;
        return Ok(Next::Continue);
    }
    if faults.garble_idle {
        io.get_mut().write_all(b"* GARBLED (((\r\n").await?;
        io.get_mut().flush().await?;
    }
    loop {
        let (out, next) = {
            let st = lock(state);
            if session.id <= st.dropped_through {
                return Ok(Next::Close);
            }
            match (
                session.selected.and_then(|i| st.mailboxes.get(i)),
                &mut session.view,
            ) {
                (Some(mb), Some(view)) if mb.uidvalidity != view.uidvalidity => {
                    ("* BYE UIDVALIDITY changed\r\n".to_owned(), Next::Close)
                }
                (Some(mb), Some(view)) => (view.catch_up(View::of(mb)), Next::Continue),
                _ => (String::new(), Next::Continue),
            }
        };
        if !out.is_empty() {
            io.get_mut().write_all(out.as_bytes()).await?;
            io.get_mut().flush().await?;
        }
        if let Next::Close = next {
            return Ok(next);
        }
        // `fill_buf` keeps what it read if the timer wins.
        tokio::select! {
            ready = io.fill_buf() => {
                if ready?.is_empty() {
                    return Ok(Next::Close);
                }
                let mut line = String::new();
                io.read_line(&mut line).await?;
                let answer = if line.trim_end().eq_ignore_ascii_case("DONE") {
                    format!("{tag} OK IDLE terminated\r\n")
                } else {
                    format!("{tag} BAD Expected DONE\r\n")
                };
                io.get_mut().write_all(answer.as_bytes()).await?;
                io.get_mut().flush().await?;
                return Ok(Next::Continue);
            }
            () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
        }
    }
}

/// STORE, COPY, MOVE and EXPUNGE (all but the last by UID) on mailbox `selected`.
fn change(st: &mut ServerState, selected: usize, command: &str, args: &[Tok], tag: &str) -> String {
    let set = args.first().map(Tok::text).unwrap_or_default();
    let uids: Vec<u32> = {
        let mb = &st.mailboxes[selected];
        mb.messages
            .iter()
            .map(|m| m.uid)
            .filter(|&uid| uid_set_contains(&set, uid, mb))
            .collect()
    };
    let mut out = String::new();
    match command {
        "UID STORE" => {
            // `+FLAGS.SILENT (\Seen)`, `-FLAGS (\Flagged)`, `FLAGS (…)`.
            let item = args
                .get(1)
                .map(Tok::text)
                .unwrap_or_default()
                .to_ascii_uppercase();
            let flags: Vec<String> = match args.get(2) {
                Some(Tok::List(flags)) => flags.iter().map(Tok::text).collect(),
                Some(other) => vec![other.text()],
                None => Vec::new(),
            };
            let mb = &mut st.mailboxes[selected];
            for &uid in &uids {
                if item.starts_with("FLAGS") {
                    let keep: Vec<&str> = flags.iter().map(String::as_str).collect();
                    mb.set_flags(uid, &keep);
                } else {
                    for flag in &flags {
                        mb.change_flag(uid, flag, item.starts_with('+'));
                    }
                }
                if !item.ends_with(".SILENT") {
                    let seq = mb.messages.iter().position(|m| m.uid == uid).unwrap() + 1;
                    let msg = mb.message_mut(uid);
                    out += &format!(
                        "* {seq} FETCH (UID {uid} FLAGS ({}))\r\n",
                        msg.flags.join(" ")
                    );
                }
            }
            out += &format!("{tag} OK STORE completed\r\n");
        }
        "UID COPY" | "UID MOVE" => {
            let name = args.get(1).map(Tok::text).unwrap_or_default();
            let Some(target) = st.find(&name) else {
                return format!("{tag} NO [TRYCREATE] No such mailbox\r\n");
            };
            let copies: Vec<FakeMessage> = st.mailboxes[selected]
                .messages
                .iter()
                .filter(|m| uids.contains(&m.uid))
                .cloned()
                .collect();
            let mut new_uids = Vec::new();
            for copy in copies {
                let flags: Vec<&str> = copy.flags.iter().map(String::as_str).collect();
                new_uids.push(st.mailboxes[target].append(&copy.raw, &flags));
            }
            let list = |uids: &[u32]| {
                uids.iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let code = if let Some(forged) = &st.faults.forged_copyuid {
                format!("[COPYUID {forged}] ")
            } else if st.faults.uidplus && !uids.is_empty() {
                format!(
                    "[COPYUID {} {} {}] ",
                    st.mailboxes[target].uidvalidity,
                    list(&uids),
                    list(&new_uids)
                )
            } else {
                String::new()
            };
            if command == "UID MOVE" {
                // RFC 6851: COPYUID comes untagged, before the expunges.
                if !code.is_empty() {
                    out += &format!("* OK {code}Moved\r\n");
                }
                let mb = &mut st.mailboxes[selected];
                for &uid in &uids {
                    mb.change_flag(uid, "\\Deleted", true);
                }
                out += &mb.expunge_deleted(Some(&uids));
                out += &format!("{tag} OK MOVE completed\r\n");
            } else {
                out += &format!("{tag} OK {code}COPY completed\r\n");
            }
        }
        "UID EXPUNGE" => {
            out += &st.mailboxes[selected].expunge_deleted(Some(&uids));
            out += &format!("{tag} OK EXPUNGE completed\r\n");
        }
        _ => {
            out += &st.mailboxes[selected].expunge_deleted(None);
            out += &format!("{tag} OK EXPUNGE completed\r\n");
        }
    }
    out
}

fn fetch(mb: &FakeMailbox, args: &[Tok], condstore: bool, faults: &Faults, tag: &str) -> Vec<u8> {
    let set = args.first().map(Tok::text).unwrap_or_default();
    let items = fetch_items(args);
    let changed_since: Option<u64> = match args.get(2) {
        Some(Tok::List(m))
            if m.first()
                .is_some_and(|t| t.text().eq_ignore_ascii_case("CHANGEDSINCE")) =>
        {
            m.get(1).and_then(|t| t.text().parse().ok())
        }
        _ => None,
    };
    let in_set = |uid| uid_set_contains(&set, uid, mb);
    let upper: Vec<String> = items.iter().map(|i| i.to_ascii_uppercase()).collect();
    let wants = |name: &str| upper.iter().any(|i| i == name);
    let header_item = header_item(&items);

    let mut responses = Vec::new();
    for (i, msg) in mb.messages.iter().enumerate() {
        if !in_set(msg.uid) || changed_since.is_some_and(|since| msg.modseq <= since) {
            continue;
        }
        let mut parts: Vec<Vec<u8>> = vec![format!("UID {}", msg.uid).into_bytes()];
        if wants("FLAGS") {
            parts.push(format!("FLAGS ({})", msg.flags.join(" ")).into_bytes());
        }
        if condstore && (wants("MODSEQ") || changed_since.is_some()) {
            parts.push(format!("MODSEQ ({})", msg.modseq).into_bytes());
        }
        if faults.split_fetch && parts.len() > 1 && (header_item.is_some() || wants("INTERNALDATE"))
        {
            responses.push(fetch_response(i + 1, &parts));
            parts.truncate(1);
        }
        if wants("INTERNALDATE") {
            parts.push(format!("INTERNALDATE \"{}\"", msg.internal_date).into_bytes());
        }
        if wants("RFC822.SIZE") {
            let size = msg.raw.len() + usize::from(faults.inexact_sizes);
            parts.push(format!("RFC822.SIZE {size}").into_bytes());
        }
        if wants("BODYSTRUCTURE") {
            let mut part = b"BODYSTRUCTURE ".to_vec();
            if faults.broken_bodystructure.contains(&msg.uid) {
                part.extend(b"(\"TEXT\")");
            } else {
                part.extend(mime::body_structure(&msg.raw));
            }
            parts.push(part);
        }
        for spec in items.iter().filter_map(|i| body_section(i)) {
            let mut part = format!("BODY[{spec}] ").into_bytes();
            match mime::section(&msg.raw, spec) {
                Some(_) if faults.nil_sections && !spec.is_empty() => part.extend(b"NIL"),
                Some(data) => part.extend(literal(&data)),
                None => part.extend(b"NIL"),
            }
            parts.push(part);
        }
        if let Some(item) = header_item {
            let spec = &item["BODY.PEEK".len()..];
            let fields: Vec<String> = spec
                .trim_start_matches("[HEADER.FIELDS")
                .trim_matches(|c| c == ' ' || c == '(' || c == ')' || c == ']')
                .split_whitespace()
                .map(str::to_ascii_lowercase)
                .collect();
            let headers = filter_headers(&msg.raw, &fields);
            let mut part = format!("BODY{spec} ").into_bytes();
            part.extend(literal(&headers));
            parts.push(part);
        }
        responses.push(fetch_response(i + 1, &parts));
    }
    if faults.reverse_fetch {
        responses.reverse();
    }
    let mut out = Vec::new();
    for response in responses {
        if faults.unsolicited {
            out.extend_from_slice(b"* 1 FETCH (FLAGS (\\Seen))\r\n");
        }
        out.extend_from_slice(&response);
        if faults.duplicate_fetch {
            out.extend_from_slice(&response);
        }
    }
    if faults.unsolicited {
        out.extend_from_slice(format!("* {} EXISTS\r\n", mb.messages.len()).as_bytes());
    }
    out.extend_from_slice(format!("{tag} OK FETCH completed\r\n").as_bytes());
    out
}

fn fetch_response(seq: usize, items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = format!("* {seq} FETCH (").into_bytes();
    out.extend(items.join(&b' '));
    out.extend(b")\r\n");
    out
}

/// Counts a FETCH for the drop and hang faults: `None` to drop the connection (once, at
/// the `drop_on`-th), else whether to hang.
fn count_fetch(count: &mut usize, drop_on: &mut Option<usize>, hang: bool) -> Option<bool> {
    *count += 1;
    if *drop_on == Some(*count) {
        *drop_on = None;
        return None;
    }
    Some(hang)
}

pub(crate) fn literal(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{{{}}}\r\n", data.len()).into_bytes();
    out.extend(data);
    out
}

/// `BODY.PEEK[]` → `""`, `BODY.PEEK[1.2]` → `"1.2"`; `None` for other items, including
/// `BODY.PEEK[HEADER.FIELDS …]`.
fn body_section(item: &str) -> Option<&str> {
    const PREFIX: &str = "BODY.PEEK[";
    let spec = item
        .get(..PREFIX.len())?
        .eq_ignore_ascii_case(PREFIX)
        .then(|| &item[PREFIX.len()..])?
        .strip_suffix(']')?;
    spec.bytes()
        .all(|b| b.is_ascii_digit() || b == b'.')
        .then_some(spec)
}

/// The item list of `UID FETCH <set> (<items>) [modifiers]`.
fn fetch_items(args: &[Tok]) -> Vec<String> {
    match args.get(1) {
        Some(Tok::List(items)) => items.iter().map(Tok::text).collect(),
        Some(other) => vec![other.text()],
        None => Vec::new(),
    }
}

/// The `BODY.PEEK[HEADER.FIELDS (…)]` item, if the FETCH asks for headers.
fn header_item(items: &[String]) -> Option<&String> {
    items.iter().find(|i| {
        i.to_ascii_uppercase()
            .starts_with("BODY.PEEK[HEADER.FIELDS")
    })
}

/// Applies [`Faults::garble`] and [`Faults::omit`] to a response about to be sent.
fn garble(state: &mut ServerState, out: Vec<u8>) -> Vec<u8> {
    let out = replace_line(&mut state.faults.garble, out, b"* GARBLED (((\r\n");
    replace_line(&mut state.faults.omit, out, b"")
}

/// Replaces the first line containing `pattern` and clears `pattern` once it fired.
fn replace_line(pattern: &mut Option<String>, out: Vec<u8>, with: &[u8]) -> Vec<u8> {
    let Some(p) = pattern.as_deref() else {
        return out;
    };
    let mut result = Vec::with_capacity(out.len());
    let mut done = false;
    for line in out.split_inclusive(|&b| b == b'\n') {
        if !done && line.windows(p.len()).any(|w| w == p.as_bytes()) {
            result.extend_from_slice(with);
            done = true;
        } else {
            result.extend_from_slice(line);
        }
    }
    if done {
        *pattern = None;
    }
    result
}

/// Whether `uid` is in an IMAP UID set like `1:3,7,9:*`, with `*` meaning the highest
/// UID in the mailbox (so `5:*` matches it even when it is below 5).
fn uid_set_contains(set: &str, uid: u32, mb: &FakeMailbox) -> bool {
    // Called for every message: finding the highest UID each time would make a FETCH
    // on a large mailbox quadratic.
    let max_uid = std::cell::OnceCell::new();
    set_contains(set, uid, || {
        *max_uid.get_or_init(|| mb.messages.iter().map(|m| m.uid).max().unwrap_or(0))
    })
}

/// Whether the sequence set or UID set `set` has `n`; `star` is what `*` stands for.
fn set_contains(set: &str, n: u32, star: impl Fn() -> u32) -> bool {
    let bound = |s: &str| {
        if s == "*" {
            star()
        } else {
            s.parse().unwrap_or(0)
        }
    };
    set.split(',').any(|part| match part.split_once(':') {
        Some((a, b)) => {
            let (a, b) = (bound(a), bound(b));
            (a.min(b)..=a.max(b)).contains(&n)
        }
        None => bound(part) == n,
    })
}

/// The header lines (with continuations) whose names are in `fields`, plus the blank
/// line that ends a header block.
fn filter_headers(raw: &[u8], fields: &[String]) -> Vec<u8> {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(raw.len(), |p| p + 2);
    let mut out = Vec::new();
    let mut keep = false;
    for line in raw[..end].split_inclusive(|&b| b == b'\n') {
        if !line.starts_with(b" ") && !line.starts_with(b"\t") {
            let name = line.split(|&b| b == b':').next().unwrap_or_default();
            keep = fields.contains(&String::from_utf8_lossy(name).trim().to_ascii_lowercase());
        }
        if keep {
            out.extend_from_slice(line);
        }
    }
    out.extend_from_slice(b"\r\n");
    out
}

pub(crate) fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_fetch_arguments() {
        let toks = tokenize(
            "UID FETCH 1:3,5 (UID FLAGS BODY.PEEK[HEADER.FIELDS (DATE FROM)]) (CHANGEDSINCE 7)",
        );
        assert_eq!(toks[2], Tok::Atom("1:3,5".into()));
        let Tok::List(items) = &toks[3] else {
            panic!("{toks:?}")
        };
        assert_eq!(
            items[2],
            Tok::Atom("BODY.PEEK[HEADER.FIELDS (DATE FROM)]".into())
        );
        assert_eq!(
            toks[4],
            Tok::List(vec![
                Tok::Atom("CHANGEDSINCE".into()),
                Tok::Atom("7".into())
            ])
        );
        assert_eq!(
            tokenize(r#"LOGIN "a\"b" pw"#),
            vec![
                Tok::Atom("LOGIN".into()),
                Tok::Str("a\"b".into()),
                Tok::Atom("pw".into())
            ]
        );
    }

    #[test]
    fn filters_header_fields() {
        let raw = b"From: a\r\nX-Spam: yes\r\n continued\r\nSubject: hi\r\n there\r\n\r\nbody";
        assert_eq!(
            filter_headers(raw, &["subject".into(), "from".into()]),
            b"From: a\r\nSubject: hi\r\n there\r\n\r\n"
        );
    }
}
