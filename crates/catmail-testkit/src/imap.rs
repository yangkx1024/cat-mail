use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::TestCert;

/// One message in a [`FakeMailbox`].
#[derive(Debug, Clone)]
pub struct FakeMessage {
    pub uid: u32,
    /// Wire form, e.g. `\Seen` or `$Junk`.
    pub flags: Vec<String>,
    pub modseq: u64,
    /// The raw message; only the header part is ever served.
    pub raw: Vec<u8>,
    /// IMAP date-time, e.g. `17-Oct-2026 09:30:00 +0800`.
    pub internal_date: String,
    /// Serve a multipart/mixed BODYSTRUCTURE with a PDF attachment.
    pub attachment: bool,
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
            internal_date: "17-Oct-2026 09:30:00 +0800".into(),
            attachment: false,
        });
        uid
    }

    /// Appends a simple message with the given subject and returns its UID.
    pub fn append_simple(&mut self, subject: &str) -> u32 {
        let raw = format!(
            "From: Alice <alice@example.com>\r\nTo: bob@example.com\r\nSubject: {subject}\r\n\
             Date: Sat, 17 Oct 2026 09:30:00 +0800\r\nMessage-ID: <{}.{subject}@example.com>\r\n\
             \r\nHello\r\n",
            self.uidnext
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
    /// Send every FETCH response twice.
    pub duplicate_fetch: bool,
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
}

impl Default for Faults {
    fn default() -> Self {
        Self {
            condstore: true,
            auth_plain: false,
            drop_on_fetch: None,
            hang_on_fetch: false,
            duplicate_fetch: false,
            reverse_fetch: false,
            unsolicited: false,
            inject_after_starttls: false,
            reject_login: false,
            preauth: false,
            garble: None,
            omit: None,
            split_fetch: false,
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
    pub connections: usize,
    header_fetches: usize,
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
            header_fetches: 0,
        }
    }

    pub fn mailbox_mut(&mut self, name: &str) -> &mut FakeMailbox {
        self.mailboxes
            .iter_mut()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("no mailbox {name}"))
    }

    pub fn inbox(&mut self) -> &mut FakeMailbox {
        self.mailbox_mut("INBOX")
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
}

fn lock(state: &Mutex<ServerState>) -> MutexGuard<'_, ServerState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

struct Session {
    tls: bool,
    mode: TlsMode,
    authenticated: bool,
    selected: Option<usize>,
    condstore_enabled: bool,
}

enum Next {
    Continue,
    Close,
    StartTls,
}

async fn serve(
    stream: Box<dyn Stream>,
    tls: bool,
    mode: TlsMode,
    cert: Option<TestCert>,
    state: Arc<Mutex<ServerState>>,
) -> std::io::Result<()> {
    let mut io = BufReader::new(stream);
    let mut session = Session {
        tls,
        mode,
        authenticated: false,
        selected: None,
        condstore_enabled: false,
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
        let Some(line) = read_command(&mut io).await? else {
            return Ok(());
        };
        match handle(&mut io, &mut session, &state, &line).await? {
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

/// Reads one command line, inlining `{n}` / `{n+}` literals. `None` at EOF.
async fn read_command(io: &mut BufReader<Box<dyn Stream>>) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
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
                if !non_sync {
                    io.get_mut()
                        .write_all(b"+ Ready for literal data\r\n")
                        .await?;
                }
                let mut data = vec![0; len];
                io.read_exact(&mut data).await?;
                // Inline as a quoted string so the tokenizer sees one token.
                line.push(b'"');
                for b in data {
                    if b == b'"' || b == b'\\' {
                        line.push(b'\\');
                    }
                    line.push(b);
                }
                line.push(b'"');
            }
            None => {
                line.extend_from_slice(&chunk);
                return Ok(Some(line));
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
    caps.join(" ")
}

async fn handle(
    io: &mut BufReader<Box<dyn Stream>>,
    session: &mut Session,
    state: &Arc<Mutex<ServerState>>,
    raw: &[u8],
) -> std::io::Result<Next> {
    let line = String::from_utf8_lossy(raw).into_owned();
    let (tag, rest) = line.split_once(' ').unwrap_or((&line, ""));
    let tag = tag.to_owned();
    lock(state).log.push(LoggedCommand {
        tls: session.tls,
        line: rest.to_owned(),
    });
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
                    out += &format!("{tag} OK [READ-ONLY] {command} completed\r\n");
                }
                _ => {
                    session.selected = None;
                    out += &format!("{tag} NO Mailbox doesn't exist: {name}\r\n");
                }
            }
        }
        "UID SEARCH" => {
            let st = lock(state);
            match session.selected.and_then(|i| st.mailboxes.get(i)) {
                Some(mb) => {
                    // `ALL`, or `UID <set>`.
                    let set = args
                        .first()
                        .is_some_and(|t| t.text().eq_ignore_ascii_case("UID"))
                        .then(|| args.get(1).map(Tok::text))
                        .flatten();
                    let uids: Vec<String> = mb
                        .messages
                        .iter()
                        .filter(|m| {
                            set.as_ref()
                                .is_none_or(|set| uid_set_contains(set, m.uid, mb))
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
        "UID FETCH" => {
            if header_item(&fetch_items(args)).is_some() {
                let hang = {
                    let mut st = lock(state);
                    st.header_fetches += 1;
                    if st.faults.drop_on_fetch == Some(st.header_fetches) {
                        st.faults.drop_on_fetch = None;
                        return Ok(Next::Close);
                    }
                    st.faults.hang_on_fetch
                };
                if hang {
                    std::future::pending::<()>().await;
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
    let out = garble(
        &mut lock(state),
        fetched.unwrap_or_else(|| out.into_bytes()),
    );
    io.get_mut().write_all(&out).await?;
    io.get_mut().flush().await?;
    Ok(next)
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
        let mut parts = vec![format!("UID {}", msg.uid)];
        if wants("FLAGS") {
            parts.push(format!("FLAGS ({})", msg.flags.join(" ")));
        }
        if condstore && (wants("MODSEQ") || changed_since.is_some()) {
            parts.push(format!("MODSEQ ({})", msg.modseq));
        }
        if faults.split_fetch && parts.len() > 1 && (header_item.is_some() || wants("INTERNALDATE"))
        {
            responses.push(format!("* {} FETCH ({})\r\n", i + 1, parts.join(" ")).into_bytes());
            parts.truncate(1);
        }
        if wants("INTERNALDATE") {
            parts.push(format!("INTERNALDATE \"{}\"", msg.internal_date));
        }
        if wants("RFC822.SIZE") {
            parts.push(format!("RFC822.SIZE {}", msg.raw.len()));
        }
        if wants("BODYSTRUCTURE") {
            parts.push(format!("BODYSTRUCTURE {}", bodystructure(msg)));
        }
        let mut literal = None;
        if let Some(item) = header_item {
            let spec = &item["BODY.PEEK".len()..];
            let fields: Vec<String> = spec
                .trim_start_matches("[HEADER.FIELDS")
                .trim_matches(|c| c == ' ' || c == '(' || c == ')' || c == ']')
                .split_whitespace()
                .map(str::to_ascii_lowercase)
                .collect();
            let headers = filter_headers(&msg.raw, &fields);
            parts.push(format!("BODY{spec} {{{}}}", headers.len()));
            literal = Some(headers);
        }
        let mut response = format!("* {} FETCH ({}", i + 1, parts.join(" ")).into_bytes();
        // The literal comes last, so the closing paren follows it.
        if let Some(data) = literal {
            response.extend_from_slice(b"\r\n");
            response.extend_from_slice(&data);
        }
        response.extend_from_slice(b")\r\n");
        responses.push(response);
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
    let max_uid = mb.messages.iter().map(|m| m.uid).max().unwrap_or(0);
    let bound = |s: &str| {
        if s == "*" {
            max_uid
        } else {
            s.parse().unwrap_or(0)
        }
    };
    set.split(',').any(|part| match part.split_once(':') {
        Some((a, b)) => {
            let (a, b) = (bound(a), bound(b));
            (a.min(b)..=a.max(b)).contains(&uid)
        }
        None => bound(part) == uid,
    })
}

fn bodystructure(msg: &FakeMessage) -> String {
    let text = format!(
        "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"UTF-8\") NIL NIL \"7BIT\" {} 1 NIL NIL NIL NIL)",
        msg.raw.len()
    );
    if msg.attachment {
        format!(
            "({text}(\"APPLICATION\" \"PDF\" (\"NAME\" \"report.pdf\") NIL NIL \"BASE64\" 1024 NIL \
             (\"ATTACHMENT\" (\"FILENAME\" \"report.pdf\")) NIL NIL) \"MIXED\" (\"BOUNDARY\" \"b1\") NIL NIL NIL)"
        )
    } else {
        text
    }
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

fn quote(s: &str) -> String {
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
