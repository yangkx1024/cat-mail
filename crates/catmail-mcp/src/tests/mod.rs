//! The server end to end: HTTP requests as an agent sends them, to a client synced with
//! the fake IMAP server.

mod drafts;
mod limits;
mod organizing;
mod reading;
mod server;

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

use crate::{Access, McpServer, ServeOptions, Tier, TokenLimits, Tokens};

struct Fixture {
    fake: FakeImap,
    _dir: tempfile::TempDir,
    client: MailClient,
    tokens: Tokens,
    secret: String,
    server: McpServer,
}

impl Fixture {
    /// alice@example.com, synced, with INBOX ("one", "two"), Archive and Trash; a server
    /// on a free port and a token for it, of the draft tier: every tool.
    async fn new(options: ServeOptions) -> Self {
        let mut state = ServerState::new("alice", "pw");
        state.inbox().append_simple("one");
        state.inbox().append_simple("two");
        state.mailboxes.push(FakeMailbox::new("Archive"));
        state
            .mailboxes
            .push(FakeMailbox::new("Trash").with_attribute("\\Trash"));
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
        add_account(&client, &fake, "alice").await;
        let tokens = Tokens::new(dir.path().join("config"));
        let every_tool = Access {
            tier: Tier::Draft,
            accounts: None,
        };
        let secret = tokens.add("test", every_tool).unwrap().secret;
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
        self.post_as(&self.secret, body).await
    }

    async fn post_as(&self, secret: &str, body: Value) -> (StatusCode, String) {
        let auth = format!("Bearer {secret}");
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
        self.call_as(&self.secret, tool, arguments).await
    }

    async fn call_as(&self, secret: &str, tool: &str, arguments: Value) -> Value {
        let result = self.call_result_as(secret, tool, arguments).await;
        assert_eq!(result["isError"], json!(false), "{tool}: {result}");
        result["structuredContent"].clone()
    }

    async fn call_result(&self, tool: &str, arguments: Value) -> Value {
        self.call_result_as(&self.secret, tool, arguments).await
    }

    /// The JSON-RPC reply's `result`, or its `error`.
    async fn call_result_as(&self, secret: &str, tool: &str, arguments: Value) -> Value {
        let (status, body) = self
            .post_as(
                secret,
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": tool, "arguments": arguments},
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let reply: Value = serde_json::from_str(&body).unwrap();
        if reply.get("error").is_some() {
            return reply["error"].clone();
        }
        reply["result"].clone()
    }

    /// The tools `secret`'s token is shown.
    async fn tool_names(&self, secret: &str) -> Vec<String> {
        let (status, body) = self
            .post_as(
                secret,
                json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let reply: Value = serde_json::from_str(&body).unwrap();
        reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    }

    /// A token named `name` with `access`.
    fn token(&self, name: &str, access: Access) -> String {
        self.tokens.add(name, access).unwrap().secret
    }

    /// The ids of the messages `list_messages` lists in folder `folder` (every INBOX
    /// with `None`), newest first.
    async fn ids(&self, folder: Option<i64>) -> Vec<i64> {
        let listed = self.call("list_messages", json!({"folder": folder})).await;
        listed["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_i64().unwrap())
            .collect()
    }

    /// The id of the account's folder named `name`.
    async fn folder(&self, account: i64, name: &str) -> i64 {
        let folders = self.call("list_folders", json!({"account": account})).await;
        folders["folders"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap_or_else(|| panic!("no folder {name}: {folders}"))["id"]
            .as_i64()
            .unwrap()
    }
}

/// Adds `user`@example.com, on `fake` with password "pw", and syncs it.
async fn add_account(client: &MailClient, fake: &FakeImap, user: &str) -> i64 {
    let server = |port| ServerConfig {
        host: "127.0.0.1".into(),
        port,
        security: Security::Plain,
    };
    let account = client
        .add_account(
            NewAccount {
                email: format!("{user}@example.com"),
                display_name: (user == "alice").then(|| "Alice".into()),
                username: user.into(),
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
    account.0
}

/// One HTTP/1.1 POST to the server at `addr`, as `localhost` unless `headers` says.
async fn send(
    addr: SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, String) {
    try_send(addr, path, headers, body).await.unwrap()
}

/// [`send`], failing if the connection does: a server turning a request away before
/// reading all of it may close it while the body is still being written.
async fn try_send(
    addr: SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> Result<(StatusCode, String), hyper::Error> {
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
    let response = sender.send_request(request).await?;
    let status = response.status();
    let body = response.into_body().collect().await?.to_bytes();
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
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

/// alice@example.com's INBOX with one message from Mallory, with a text file and a
/// binary one attached.
fn with_attachments() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    let raw = "From: Mallory <m@example.com>\r\nTo: Alice <alice@example.com>\r\n\
               Subject: notes\r\nDate: Sat, 17 Oct 2026 10:00:00 +0800\r\n\
               Message-ID: <notes@example.com>\r\nMIME-Version: 1.0\r\n\
               Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
               --b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSee the notes.\r\n\
               --b\r\nContent-Type: text/plain; name=\"notes.txt\"\r\n\
               Content-Disposition: attachment; filename=\"notes.txt\"\r\n\r\nline one\r\n\
               --b\r\nContent-Type: application/octet-stream; name=\"data.bin\"\r\n\
               Content-Disposition: attachment; filename=\"data.bin\"\r\n\
               Content-Transfer-Encoding: base64\r\n\r\nAAEC\r\n--b--\r\n";
    state.inbox().append(raw.as_bytes(), &[]);
    state
        .mailboxes
        .push(FakeMailbox::new("Drafts").with_attribute("\\Drafts"));
    state
}
