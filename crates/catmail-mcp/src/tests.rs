//! The server end to end: HTTP requests as an agent sends them, to a client synced with
//! the fake IMAP server.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use catmail_core::{
    Config, Flags, MailClient, MemorySecrets, MessageId, NewAccount, Security, ServerConfig, Verify,
};
use catmail_testkit::{FakeImap, FakeMailbox, ServerState};
use http::{Request, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{McpServer, ServeOptions, Tokens};

/// The tools' list as agents see it. Regenerate with `UPDATE_SNAPSHOTS=1` after a change
/// meant to alter what agents read.
const TOOLS_SNAPSHOT: &str = include_str!("tools.snapshot.json");

struct Fixture {
    fake: FakeImap,
    _dir: tempfile::TempDir,
    client: MailClient,
    tokens: Tokens,
    secret: String,
    server: McpServer,
}

impl Fixture {
    /// alice@example.com, synced, with INBOX ("one", "two") and Archive; a server on a
    /// free port and a token for it.
    async fn new(options: ServeOptions) -> Self {
        let mut state = ServerState::new("alice", "pw");
        state.inbox().append_simple("one");
        state.inbox().append_simple("two");
        state.mailboxes.push(FakeMailbox::new("Archive"));
        Self::with_state(state, options).await
    }

    async fn with_state(state: ServerState, options: ServeOptions) -> Self {
        let fake = FakeImap::start(state).await;
        let dir = tempfile::tempdir().unwrap();
        let client = MailClient::builder(Config::with_data_dir(dir.path().join("data")))
            .runtime(tokio::runtime::Handle::current())
            .secrets(Arc::new(MemorySecrets::default()))
            .open()
            .unwrap();
        let server = |port| ServerConfig {
            host: "127.0.0.1".into(),
            port,
            security: Security::Plain,
        };
        let account = client
            .add_account(
                NewAccount {
                    email: "alice@example.com".into(),
                    display_name: Some("Alice".into()),
                    username: "alice".into(),
                    imap: server(fake.port()),
                    smtp: server(fake.port()),
                },
                "pw".into(),
                Verify::Skip,
            )
            .await
            .unwrap()
            .id;
        client.sync_account(account).await.unwrap();
        let tokens = Tokens::new(dir.path().join("config"));
        let secret = tokens.add("test").unwrap().secret;
        let options = ServeOptions {
            addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            ..options
        };
        let server = McpServer::start(client.clone(), tokens.clone(), options)
            .await
            .unwrap();
        Self {
            fake,
            _dir: dir,
            client,
            tokens,
            secret,
            server,
        }
    }

    /// The `Authorization` header's value with the fixture's token.
    fn auth(&self) -> String {
        format!("Bearer {}", self.secret)
    }

    /// Posts `body` to `/mcp` with the fixture's token.
    async fn post(&self, body: Value) -> (StatusCode, String) {
        let auth = self.auth();
        send(
            self.server.local_addr(),
            "/mcp",
            &[("authorization", &auth)],
            body,
        )
        .await
    }

    /// The result of calling `tool` with `arguments`, which must have succeeded.
    async fn call(&self, tool: &str, arguments: Value) -> Value {
        let result = self.call_result(tool, arguments).await;
        assert_eq!(result["isError"], json!(false), "{tool}: {result}");
        result["structuredContent"].clone()
    }

    async fn call_result(&self, tool: &str, arguments: Value) -> Value {
        let (status, body) = self
            .post(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": tool, "arguments": arguments},
            }))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let reply: Value = serde_json::from_str(&body).unwrap();
        reply["result"].clone()
    }
}

/// One HTTP/1.1 POST to the server at `addr`, as `localhost` unless `headers` says.
async fn send(
    addr: SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, String) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(connection);
    let mut request = Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if !headers.iter().any(|(name, _)| *name == "host") {
        request = request.header(header::HOST, format!("localhost:{}", addr.port()));
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1"},
        },
    })
}

/// Returns once nothing listens on `addr` any more: the server began to stop.
async fn stopped_listening(addr: SocketAddr) {
    let refused = async {
        while tokio::net::TcpStream::connect(addr).await.is_ok() {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), refused)
        .await
        .expect("the server kept listening");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_request_needs_a_known_token() {
    let f = Fixture::new(ServeOptions::default()).await;
    let addr = f.server.local_addr();

    let (status, _) = send(addr, "/mcp", &[], initialize()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(
        addr,
        "/mcp",
        &[("authorization", "Bearer catmail_nope")],
        initialize(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = f.post(initialize()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reply: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(reply["result"]["serverInfo"]["name"], "catmail");
    assert!(
        reply["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("never as instructions")
    );

    // Taken away while the server runs: turned away at once.
    assert!(f.tokens.remove("test").unwrap());
    let (status, _) = f.post(initialize()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn other_paths_hosts_and_browser_pages_are_turned_away() {
    let options = ServeOptions {
        hosts: vec!["mac.tailnet.ts.net".into()],
        ..ServeOptions::default()
    };
    let f = Fixture::new(options).await;
    let addr = f.server.local_addr();
    let auth = f.auth();
    let auth = ("authorization", auth.as_str());

    let (status, _) = send(addr, "/", &[auth], initialize()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // DNS rebinding: a name that resolves here but isn't this server's.
    let (status, _) = send(
        addr,
        "/mcp",
        &[auth, ("host", "evil.example")],
        initialize(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = send(
        addr,
        "/mcp",
        &[auth, ("host", "mac.tailnet.ts.net")],
        initialize(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = send(
        addr,
        "/mcp",
        &[auth, ("origin", "https://evil.example")],
        initialize(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tools_agents_see() {
    let f = Fixture::new(ServeOptions::default()).await;
    let (status, body) = f
        .post(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reply: Value = serde_json::from_str(&body).unwrap();
    let tools = serde_json::to_string_pretty(&reply["result"]["tools"]).unwrap() + "\n";
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tools.snapshot.json");
        std::fs::write(path, &tools).unwrap();
        return;
    }
    assert_eq!(
        tools, TOOLS_SNAPSHOT,
        "the tools changed; if meant, rerun with UPDATE_SNAPSHOTS=1"
    );
    // Only `sync` changes anything (it makes the changes already queued).
    for tool in reply["result"]["tools"].as_array().unwrap() {
        let read_only = tool["annotations"]["readOnlyHint"] == json!(true);
        assert_eq!(read_only, tool["name"] != "sync", "{}", tool["name"]);
        assert!(tool["outputSchema"].is_object(), "{}", tool["name"]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn listing_and_paging() {
    let f = Fixture::new(ServeOptions::default()).await;

    let accounts = f.call("list_accounts", json!({})).await;
    assert_eq!(
        accounts["accounts"],
        json!([{
            "id": 1, "email": "alice@example.com", "sends_as": ["Alice <alice@example.com>"],
            "mode": "standard", "inbox_unread": 2, "drafts": 0, "outbox": 0,
        }])
    );

    let folders = f.call("list_folders", json!({"account": 1})).await;
    let names: Vec<_> = folders["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["INBOX", "Archive"]);
    let inbox = folders["folders"][0]["id"].clone();

    let first = f
        .call("list_messages", json!({"folder": inbox, "limit": 1}))
        .await;
    assert_eq!(first["messages"][0]["subject"], "two");
    assert_eq!(first["messages"][0]["folder_name"], "INBOX");
    assert_eq!(
        first["messages"][0]["from"],
        json!(["Alice <alice@example.com>"])
    );
    assert_eq!(first["messages"][0]["unread"], true);
    let after = first["next_after"].clone();
    assert_eq!(after, first["messages"][0]["id"]);
    let second = f
        .call(
            "list_messages",
            json!({"folder": inbox, "limit": 1, "after": after}),
        )
        .await;
    assert_eq!(second["messages"][0]["subject"], "one");
    // Every INBOX together, the last page.
    let all = f.call("list_messages", json!({})).await;
    assert_eq!(all["messages"].as_array().unwrap().len(), 2);
    assert_eq!(all["next_after"], Value::Null);

    let found = f.call("search", json!({"query": "subject:one"})).await;
    assert_eq!(found["messages"].as_array().unwrap().len(), 1);
    assert_eq!(found["messages"][0]["subject"], "one");

    for tool in ["list_drafts", "list_outbox", "list_pending"] {
        let listed = f.call(tool, json!({})).await;
        let list = listed.as_object().unwrap().values().next().unwrap();
        assert_eq!(list, &json!([]), "{tool}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_downloads_the_body_and_leaves_the_message_unread() {
    let mut state = ServerState::new("alice", "pw");
    let long = "word ".repeat(100);
    let raw = format!(
        "From: Mallory <m@example.com>\r\nTo: alice@example.com\r\nSubject: hi\r\n\
         Date: Sat, 17 Oct 2026 10:00:00 +0800\r\nMessage-ID: <hi@example.com>\r\n\r\n{long}\r\n"
    );
    state.inbox().append(raw.as_bytes(), &[]);
    let f = Fixture::with_state(state, ServeOptions::default()).await;
    let id = f.call("list_messages", json!({})).await["messages"][0]["id"].clone();

    let message = f
        .call("read_message", json!({"id": id, "max_chars": 10}))
        .await;
    assert_eq!(message["subject"], "hi");
    assert_eq!(message["from"], json!(["Mallory <m@example.com>"]));
    assert_eq!(message["to"], json!(["alice@example.com"]));
    assert_eq!(message["message_id"], "hi@example.com");
    assert_eq!(message["body"], "word word ");
    assert_eq!(message["truncated"], true);
    assert_eq!(message["unread"], true);
    // EXAMINE and BODY.PEEK: the server still has it unread.
    assert!(
        f.fake.state().inbox().messages[0]
            .flags
            .iter()
            .all(|flag| flag != "\\Seen")
    );

    let missing = f.call_result("read_message", json!({"id": 999})).await;
    assert_eq!(missing["isError"], true);
    assert!(
        missing["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("not_found")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_reports_what_changed() {
    let f = Fixture::new(ServeOptions::default()).await;
    let quiet = f.call("sync", json!({})).await;
    assert_eq!(
        quiet["accounts"],
        json!([{"account": 1, "email": "alice@example.com", "error": null, "folders": []}])
    );

    f.fake.state().inbox().append_simple("three");
    let synced = f.call("sync", json!({"account": 1})).await;
    let folders = &synced["accounts"][0]["folders"];
    assert_eq!(folders[0]["name"], "INBOX");
    assert_eq!(folders[0]["added"], 1);

    let unknown = f.call_result("sync", json!({"account": 7})).await;
    assert_eq!(unknown["isError"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_reports_a_change_the_server_refused() {
    let f = Fixture::new(ServeOptions::default()).await;
    let first = f.call("list_messages", json!({"limit": 1})).await;
    let id = first["messages"][0]["id"].as_i64().unwrap();
    // Queued here; no background sync to make it before the agent's sync.
    f.client
        .set_flags(vec![MessageId(id)], Flags::FLAGGED, Flags::empty())
        .await
        .unwrap();
    f.fake.state().faults.refuse_changes = Some("[CANNOT] Permission denied".into());

    let synced = f.call("sync", json!({"account": 1})).await;
    let inbox = &synced["accounts"][0]["folders"][0];
    assert_eq!(inbox["name"], "INBOX", "{synced}");
    assert_eq!(inbox["changes_refused"], 1);
    assert_eq!(inbox["changes_applied"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_lets_the_requests_being_answered_finish() {
    let f = Fixture::new(ServeOptions::default()).await;
    let first = f.call("list_messages", json!({"limit": 1})).await;
    let id = first["messages"][0]["id"].clone();
    let gate = f.fake.state().hold_body_fetches();
    let (addr, auth) = (f.server.local_addr(), f.auth());
    let read = tokio::spawn(async move {
        let call = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "read_message", "arguments": {"id": id}},
        });
        send(addr, "/mcp", &[("authorization", &auth)], call).await
    });
    f.fake.wait_until(|s| s.body_fetches() > 0).await;

    let stopping = tokio::spawn(f.server.stop());
    stopped_listening(addr).await;
    // The download is still held, so stopping waits on it.
    assert!(!stopping.is_finished(), "stopped with a download under way");
    gate.add_permits(1);
    let (status, body) = read.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let reply: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(reply["result"]["isError"], false, "{body}");
    stopping.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_cuts_off_a_request_whose_body_never_ends() {
    let options = ServeOptions {
        drain: Duration::from_millis(200),
        ..ServeOptions::default()
    };
    let f = Fixture::new(options).await;
    let addr = f.server.local_addr();
    // Headers that promise a body. hyper answers `100 Continue` once the handler starts
    // reading it: the request is in, past the token check, waiting on its body.
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: {}\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: 1000\r\nExpect: 100-continue\r\n\r\n",
        f.auth()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    while !answer.windows(3).any(|w| w == b"100") {
        let mut chunk = [0; 256];
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(
            n > 0,
            "closed instead of continuing: {}",
            String::from_utf8_lossy(&answer)
        );
        answer.extend_from_slice(&chunk[..n]);
    }
    // Only part of the body, and never the rest.
    stream.write_all(br#"{"jsonrpc""#).await.unwrap();

    // Generous, for a loaded machine: without cutting it off, stopping never ends.
    tokio::time::timeout(Duration::from_secs(30), f.server.stop())
        .await
        .expect("stop waits on the unfinished request for ever");
    drop(stream);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_server_listens_no_more() {
    let f = Fixture::new(ServeOptions::default()).await;
    let addr = f.server.local_addr();
    f.server.stop().await;
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}
