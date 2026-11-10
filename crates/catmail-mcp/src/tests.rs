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

use crate::{Access, McpServer, ServeOptions, Tier, TokenLimits, Tokens};

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
async fn other_paths_and_browser_pages_are_turned_away() {
    let f = Fixture::new(ServeOptions::default()).await;
    let addr = f.server.local_addr();
    let auth = f.auth();
    let auth = ("authorization", auth.as_str());

    let (status, _) = send(addr, "/", &[auth], initialize()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Through a tunnel: addressed to the name it forwards as, with nothing to configure.
    let (status, body) = send(
        addr,
        "/mcp",
        &[auth, ("host", "mac.tailnet.ts.net")],
        initialize(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // DNS rebinding: a page whose name now resolves here. It has no token, and its
    // browser says where it is from.
    let rebound = ("host", "evil.example");
    let (status, _) = send(addr, "/mcp", &[rebound], initialize()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(
        addr,
        "/mcp",
        &[auth, rebound, ("origin", "http://evil.example")],
        initialize(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
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
    // Of the read tier, only `sync` changes anything (it makes the changes already
    // queued); every tool has a tier. Only an attachment comes as what it is (text, an
    // image, a blob) rather than as structured output.
    for tool in reply["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        let read_only = tool["annotations"]["readOnlyHint"] == json!(true);
        let tier = crate::tools::tier_of(name).unwrap_or_else(|| panic!("{name} has no tier"));
        assert_eq!(read_only, tier == Tier::Read && name != "sync", "{name}");
        assert_eq!(
            tool["outputSchema"].is_object(),
            name != "read_attachment",
            "{name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_sees_and_calls_only_its_tiers_tools() {
    let f = Fixture::new(ServeOptions::default()).await;
    let read = f.token("reader", Access::default());
    let names = f.tool_names(&read).await;
    assert!(names.contains(&"read_message".to_owned()));
    assert!(names.contains(&"read_attachment".to_owned()));
    for tool in ["mark", "move", "archive", "label", "trash"] {
        assert!(!names.contains(&tool.to_owned()), "{tool}");
        assert!(f.tool_names(&f.secret).await.contains(&tool.to_owned()));
    }
    let organize = f.token(
        "organizer",
        Access {
            tier: Tier::Organize,
            accounts: None,
        },
    );
    let names = f.tool_names(&organize).await;
    assert!(names.contains(&"trash".to_owned()));
    let drafting = [
        "create_draft",
        "reply",
        "forward",
        "update_draft",
        "attach",
        "remove_attachment",
    ];
    for tool in drafting {
        assert!(!names.contains(&tool.to_owned()), "{tool}");
        assert!(f.tool_names(&f.secret).await.contains(&tool.to_owned()));
    }
    let refused = f
        .call_result_as(&organize, "create_draft", json!({"account": 1}))
        .await;
    assert_eq!(refused["message"], "tool not found", "{refused}");
    let id = f.ids(None).await[0];
    let refused = f
        .call_result_as(&read, "mark", json!({"messages": [id], "read": true}))
        .await;
    assert_eq!(refused["message"], "tool not found", "{refused}");
    assert_eq!(
        f.call("list_messages", json!({})).await["messages"][0]["unread"],
        true
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_reaches_only_its_accounts() {
    let f = Fixture::new(ServeOptions::default()).await;
    let mut state = ServerState::new("bob", "pw");
    state.inbox().append_simple("bob's");
    let bob_server = FakeImap::start(state).await;
    let bob = add_account(&f.client, &bob_server, "bob").await;
    let bob_inbox = f.folder(bob, "INBOX").await;
    let bob_message = f.ids(Some(bob_inbox)).await[0];
    let bob_draft = f
        .client
        .create_draft(
            catmail_core::AccountId(bob),
            None,
            Default::default(),
            Vec::new(),
        )
        .await
        .unwrap()
        .id
        .0;
    let alice_only = f.token(
        "alice's",
        Access {
            tier: Tier::Draft,
            accounts: Some(vec!["ALICE@example.com".into()]),
        },
    );

    let accounts = f.call_as(&alice_only, "list_accounts", json!({})).await;
    assert_eq!(accounts["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(accounts["accounts"][0]["email"], "alice@example.com");
    // Every INBOX together: only Alice's.
    let inboxes = f.call_as(&alice_only, "list_messages", json!({})).await;
    let subjects: Vec<_> = inboxes["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, ["two", "one"]);
    let found = f
        .call_as(&alice_only, "search", json!({"query": "subject:bob's"}))
        .await;
    assert_eq!(found["messages"], json!([]));
    assert_eq!(
        f.call("search", json!({"query": "subject:bob's"})).await["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Bob's account, folders and messages don't exist for it.
    let calls = [
        ("list_folders", json!({"account": bob})),
        ("sync", json!({"account": bob})),
        ("list_messages", json!({"folder": bob_inbox})),
        ("search", json!({"query": "one", "folder": bob_inbox})),
        ("read_message", json!({"id": bob_message})),
        ("list_messages", json!({"after": bob_message})),
        ("mark", json!({"messages": [bob_message], "read": true})),
        ("trash", json!({"messages": [bob_message]})),
        (
            "read_attachment",
            json!({"message": bob_message, "attachment": 1}),
        ),
        ("create_draft", json!({"account": bob, "subject": "x"})),
        ("reply", json!({"message": bob_message})),
        ("forward", json!({"message": bob_message})),
        ("update_draft", json!({"draft": bob_draft, "text": "x"})),
        (
            "attach",
            json!({"draft": bob_draft, "name": "a.txt", "base64": "eA=="}),
        ),
        (
            "remove_attachment",
            json!({"draft": bob_draft, "attachment": 1}),
        ),
    ];
    for (tool, arguments) in calls {
        let result = f.call_result_as(&alice_only, tool, arguments).await;
        assert_eq!(result["isError"], true, "{tool}: {result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("not_found"), "{tool}: {text}");
    }
    assert!(
        bob_server.state().inbox().messages[0]
            .flags
            .iter()
            .all(|flag| flag != "\\Seen")
    );
    let drafts = f.call_as(&alice_only, "list_drafts", json!({})).await;
    assert_eq!(drafts["drafts"], json!([]));
    let bob_drafts = f.client.list_drafts(catmail_core::AccountId(bob)).await;
    assert_eq!(bob_drafts.unwrap()[0].content, Default::default());
}

#[tokio::test(flavor = "multi_thread")]
async fn marking_reaches_the_server() {
    let f = Fixture::new(ServeOptions::default()).await;
    let ids = f.ids(None).await;
    let changed = f
        .call(
            "mark",
            json!({"messages": ids, "read": true, "starred": true}),
        )
        .await;
    assert_eq!(
        changed,
        json!({"messages": 2, "not_on_server": [], "errors": []})
    );
    for message in &f.fake.state().inbox().messages {
        assert!(message.flags.iter().any(|flag| flag == "\\Seen"));
        assert!(message.flags.iter().any(|flag| flag == "\\Flagged"));
    }
    let listed = f.call("list_messages", json!({})).await;
    assert_eq!(listed["messages"][0]["unread"], false);

    let nothing = f.call_result("mark", json!({"messages": ids})).await;
    assert_eq!(nothing["isError"], true);
    let none = f
        .call_result("mark", json!({"messages": [], "read": true}))
        .await;
    assert_eq!(none["isError"], true);
    let too_many: Vec<i64> = (1..=101).collect();
    let many = f
        .call_result("mark", json!({"messages": too_many, "read": true}))
        .await;
    assert_eq!(many["isError"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn moving_archiving_and_trashing() {
    let f = Fixture::new(ServeOptions::default()).await;
    let (inbox, archive, trash) = (
        f.folder(1, "INBOX").await,
        f.folder(1, "Archive").await,
        f.folder(1, "Trash").await,
    );
    let ids = f.ids(Some(inbox)).await;
    let subject = |state: &ServerState, mailbox: &str| -> Vec<String> {
        state
            .mailboxes
            .iter()
            .find(|m| m.name == mailbox)
            .unwrap()
            .messages
            .iter()
            .map(|m| {
                String::from_utf8_lossy(&m.raw)
                    .lines()
                    .find_map(|l| l.strip_prefix("Subject: ").map(str::to_owned))
                    .unwrap()
            })
            .collect()
    };

    let archived = f.call("archive", json!({"messages": [ids[0]]})).await;
    assert_eq!(archived["not_on_server"], json!([]), "{archived}");
    assert_eq!(subject(&f.fake.state(), "Archive"), ["two"]);

    let moved = f
        .call(
            "move",
            json!({"messages": [ids[1]], "to": trash, "from": inbox}),
        )
        .await;
    assert_eq!(moved["not_on_server"], json!([]), "{moved}");
    assert_eq!(subject(&f.fake.state(), "Trash"), ["one"]);
    assert!(subject(&f.fake.state(), "INBOX").is_empty());

    // Out of Trash and back: never deleted for good.
    let in_archive = f.ids(Some(archive)).await;
    f.call("trash", json!({"messages": in_archive})).await;
    let in_trash = f.ids(Some(trash)).await;
    assert_eq!(in_trash.len(), 2);
    let refused = f.call_result("trash", json!({"messages": in_trash})).await;
    assert_eq!(refused["isError"], true);
    assert!(
        refused["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("in Trash already"),
        "{refused}"
    );
    assert_eq!(subject(&f.fake.state(), "Trash").len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_the_server_refuses_is_reported() {
    let f = Fixture::new(ServeOptions::default()).await;
    let id = f.ids(None).await[0];
    f.fake.state().faults.refuse_changes = Some("[CANNOT] Permission denied".into());
    let changed = f
        .call("mark", json!({"messages": [id], "starred": true}))
        .await;
    let refused = &changed["not_on_server"][0];
    assert_eq!(refused["message"], id, "{changed}");
    assert_eq!(refused["status"], "failed");
    // Undone here.
    let listed = f.call("list_messages", json!({"limit": 1})).await;
    assert_eq!(listed["messages"][0]["flagged"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn labelling_on_gmail() {
    let mut state = ServerState::gmail("alice", "pw");
    let uid = state.all_mail().append_simple("hello");
    state.all_mail().set_labels(uid, &["\\Inbox"]);
    state.mailboxes.push(FakeMailbox::new("Work"));
    let f = Fixture::with_state(state, ServeOptions::default()).await;
    let work = f.folder(1, "Work").await;
    let id = f.ids(None).await[0];

    let changed = f
        .call("label", json!({"messages": [id], "add": [work]}))
        .await;
    assert_eq!(changed["not_on_server"], json!([]), "{changed}");
    let labelled = f.ids(Some(work)).await;
    assert_eq!(labelled, [id]);
    let labels = f.fake.state().all_mail().messages[0].labels.clone();
    assert!(labels.iter().any(|l| l == "Work"), "{labels:?}");
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
    assert_eq!(names, ["INBOX", "Archive", "Trash"]);
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

#[tokio::test(flavor = "multi_thread")]
async fn a_move_that_turns_into_a_conflict_is_reported() {
    let f = Fixture::new(ServeOptions::default()).await;
    let id = f.ids(None).await[0];
    // INBOX is rebuilt on the server: the move's UID is of a generation gone, so the
    // replay turns it into a conflict and drops the row it moved here.
    f.fake.state().inbox().reset_uidvalidity(2);
    let changed = f.call("archive", json!({"messages": [id]})).await;
    let conflict = &changed["not_on_server"][0];
    assert_eq!(conflict["status"], "conflict", "{changed}");
    assert_eq!(conflict["change"]["kind"], "move", "{changed}");
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

#[tokio::test(flavor = "multi_thread")]
async fn writing_and_changing_drafts() {
    let f = Fixture::new(ServeOptions::default()).await;
    let account = f.call("list_accounts", json!({})).await["accounts"][0]["id"].clone();

    let draft = f
        .call(
            "create_draft",
            json!({
                "account": account,
                "to": ["Bob <bob@example.com>, carol@example.com"],
                "bcc": ["dave@example.com"],
                "subject": "Plans",
                "text": "Shall we?",
                "attachments": [{"name": "plan.txt", "base64": "c3RlcCBvbmU=\n"}],
            }),
        )
        .await;
    assert_eq!(draft["from"], "Alice <alice@example.com>");
    assert_eq!(
        draft["to"],
        json!(["Bob <bob@example.com>", "carol@example.com"])
    );
    assert_eq!(draft["bcc"], json!(["dave@example.com"]));
    assert_eq!(draft["text"], "Shall we?");
    assert_eq!(draft["attachments"][0]["filename"], "plan.txt");
    assert_eq!(draft["attachments"][0]["mime_type"], "text/plain");
    assert_eq!(draft["attachments"][0]["size"], 8);
    let id = draft["id"].clone();
    let listed = f.call("list_drafts", json!({})).await;
    assert_eq!(listed["drafts"], json!([draft]));

    // What is given replaces what it says; the rest stays.
    let updated = f
        .call(
            "update_draft",
            json!({"draft": id, "subject": "Plans, again"}),
        )
        .await;
    assert_eq!(updated["subject"], "Plans, again");
    assert_eq!(updated["text"], "Shall we?");
    assert_eq!(updated["to"], draft["to"]);
    let updated = f
        .call(
            "update_draft",
            json!({"draft": id, "cc": [], "to": ["eve@example.com"]}),
        )
        .await;
    assert_eq!(updated["to"], json!(["eve@example.com"]));

    let attached = f
        .call(
            "attach",
            json!({"draft": id, "name": "photo.png", "base64": "iVBORw=="}),
        )
        .await;
    let photo = &attached["attachments"][1];
    assert_eq!(photo["mime_type"], "image/png");
    let removed = f
        .call(
            "remove_attachment",
            json!({"draft": id, "attachment": photo["id"]}),
        )
        .await;
    assert_eq!(removed["attachments"].as_array().unwrap().len(), 1);
    let stored = f
        .client
        .get_draft(catmail_core::DraftId(id.as_i64().unwrap()));
    assert_eq!(stored.await.unwrap().unwrap().attachments.len(), 1);

    let refusals = [
        ("update_draft", json!({"draft": id}), "invalid"),
        (
            "update_draft",
            json!({"draft": id, "from": "mallory@example.com"}),
            "not_found",
        ),
        (
            "update_draft",
            json!({"draft": 999, "text": "x"}),
            "not_found",
        ),
        (
            "remove_attachment",
            json!({"draft": id, "attachment": photo["id"]}),
            "not_found",
        ),
        (
            "attach",
            json!({"draft": id, "name": "a.txt", "base64": "not base64!"}),
            "invalid",
        ),
        (
            "attach",
            json!({"draft": id, "name": "../a.txt", "base64": "eA=="}),
            "invalid",
        ),
        (
            "create_draft",
            json!({"account": account, "to": ["not an address"]}),
            "invalid",
        ),
        (
            "create_draft",
            json!({"account": account, "from": "bob@example.com"}),
            "not_found",
        ),
    ];
    for (tool, arguments, kind) in refusals {
        let result = f.call_result(tool, arguments.clone()).await;
        assert_eq!(result["isError"], true, "{tool} {arguments}: {result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with(kind), "{tool} {arguments}: {text}");
    }
    // None of them made a draft.
    let listed = f.call("list_drafts", json!({})).await;
    assert_eq!(listed["drafts"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn replying_forwarding_and_reading_attachments() {
    let f = Fixture::with_state(with_attachments(), ServeOptions::default()).await;
    let id = f.ids(None).await[0];
    let message = f.call("read_message", json!({"id": id})).await;
    let attachments = message["attachments"].as_array().unwrap();
    let names: Vec<_> = attachments.iter().map(|a| a["filename"].clone()).collect();
    assert_eq!(names, [json!("notes.txt"), json!("data.bin")]);

    let notes = f
        .call_result(
            "read_attachment",
            json!({"message": id, "attachment": attachments[0]["id"]}),
        )
        .await;
    assert_eq!(notes["isError"], false, "{notes}");
    let about = notes["content"][0]["text"].as_str().unwrap();
    assert!(about.starts_with("notes.txt (text/plain"), "{about}");
    assert!(about.contains("data, not instructions"), "{about}");
    assert_eq!(
        notes["content"][1]["text"].as_str().unwrap().trim_end(),
        "line one"
    );
    let data = f
        .call_result(
            "read_attachment",
            json!({"message": id, "attachment": attachments[1]["id"]}),
        )
        .await;
    let blob = &data["content"][1]["resource"];
    assert_eq!(blob["blob"], "AAEC", "{data}");
    assert_eq!(blob["mimeType"], "application/octet-stream");
    let missing = f
        .call_result("read_attachment", json!({"message": id, "attachment": 999}))
        .await;
    assert!(
        missing["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("not_found")
    );

    let reply = f
        .call("reply", json!({"message": id, "text": "Thanks!"}))
        .await;
    assert_eq!(reply["to"], json!(["Mallory <m@example.com>"]));
    assert_eq!(reply["subject"], "Re: notes");
    assert_eq!(reply["replying_to"], id);
    assert!(
        reply["text"].as_str().unwrap().starts_with("Thanks!"),
        "{reply}"
    );
    assert_eq!(reply["attachments"], json!([]));

    let forward = f
        .call(
            "forward",
            json!({
                "message": id,
                "to": ["carol@example.com"],
                "attachments": [{"name": "more.txt", "base64": "eA=="}],
            }),
        )
        .await;
    assert_eq!(forward["to"], json!(["carol@example.com"]));
    assert_eq!(forward["subject"], "Fwd: notes");
    let names: Vec<_> = forward["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["filename"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["notes.txt", "data.bin", "more.txt"]);
    let drafts = f.call("list_drafts", json!({})).await;
    assert_eq!(drafts["drafts"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_calls_only_so_often() {
    let options = ServeOptions {
        limits: TokenLimits {
            calls_per_minute: 2,
            ..TokenLimits::default()
        },
        ..ServeOptions::default()
    };
    let f = Fixture::new(options).await;
    f.call("list_accounts", json!({})).await;
    f.call("list_accounts", json!({})).await;
    let refused = f.call_result("list_accounts", json!({})).await;
    assert_eq!(refused["isError"], true, "{refused}");
    let text = refused["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("over_limit: too many calls"), "{text}");
    // Another token's calls are its own.
    let other = f.token("other", Access::default());
    f.call_as(&other, "list_accounts", json!({})).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_downloads_only_so_much_a_day() {
    let f = Fixture::new(ServeOptions::default()).await;
    let ids = f.ids(None).await;
    let mut sizes = Vec::new();
    for &id in &ids {
        let message = f.client.get_message(MessageId(id)).await.unwrap().unwrap();
        assert!(message.body.is_none(), "downloaded by the sync already");
        sizes.push(u64::from(message.size.unwrap()));
    }
    drop(f);
    // Room for the first message, not for both.
    let options = ServeOptions {
        limits: TokenLimits {
            download_per_day: sizes[0] + sizes[1] - 1,
            ..TokenLimits::default()
        },
        ..ServeOptions::default()
    };
    let f = Fixture::new(options).await;
    let ids = f.ids(None).await;
    f.call("read_message", json!({"id": ids[0]})).await;
    // Downloaded: it costs nothing again.
    f.call("read_message", json!({"id": ids[0]})).await;
    let refused = f.call_result("read_message", json!({"id": ids[1]})).await;
    assert_eq!(refused["isError"], true, "{refused}");
    let text = refused["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("over_limit"), "{text}");
    let message = f.client.get_message(MessageId(ids[1])).await.unwrap();
    assert!(message.unwrap().body.is_none());
    // Another token has a budget of its own.
    let other = f.token("other", Access::default());
    f.call_as(&other, "read_message", json!({"id": ids[1]}))
        .await;
}

#[test]
fn what_a_draft_says_is_left_out_of_the_log() {
    let arguments = json!({
        "draft": 1,
        "subject": "Secret plans",
        "text": "héllo",
        "to": ["bob@example.com"],
        "attachments": [{"name": "a.txt", "base64": "aGk="}],
    });
    let logged = crate::tools::logged(arguments.as_object());
    let logged: Value = serde_json::from_str(&logged).unwrap();
    assert_eq!(
        logged,
        json!({
            "draft": 1,
            "subject": "<12 chars>",
            "text": "<5 chars>",
            "to": ["bob@example.com"],
            "attachments": [{"name": "a.txt", "base64": "<4 chars>"}],
        })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_token_that_attaches_files_may_send_large_requests() {
    let f = Fixture::new(ServeOptions::default()).await;
    // 1.5 MB of arguments a tool ignores.
    let large = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "list_accounts", "arguments": {"pad": "x".repeat(1536 * 1024)}},
    });
    let (status, body) = f.post(large.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let read = f.token("reader", Access::default());
    let (status, _) = f.post_as(&read, large).await;
    assert_ne!(status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_open_in_the_app_is_left_alone() {
    let f = Fixture::new(ServeOptions::default()).await;
    let account = f.call("list_accounts", json!({})).await["accounts"][0]["id"].clone();
    let draft = f
        .call(
            "create_draft",
            json!({
                "account": account,
                "subject": "Plans",
                "attachments": [{"name": "a.txt", "base64": "eA=="}],
            }),
        )
        .await;
    let id = draft["id"].clone();
    let attachment = draft["attachments"][0]["id"].clone();
    let (_, mark, _) = f
        .client
        .edit_draft(catmail_core::DraftId(id.as_i64().unwrap()))
        .await
        .unwrap();

    let calls = [
        (
            "update_draft",
            json!({"draft": id, "subject": "Other plans"}),
        ),
        (
            "attach",
            json!({"draft": id, "name": "b.txt", "base64": "eA=="}),
        ),
        (
            "remove_attachment",
            json!({"draft": id, "attachment": attachment}),
        ),
    ];
    for (tool, arguments) in &calls {
        let result = f.call_result(tool, arguments.clone()).await;
        assert_eq!(result["isError"], true, "{tool}: {result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("conflict: draft"), "{tool}: {text}");
    }
    let listed = f.call("list_drafts", json!({})).await;
    assert_eq!(listed["drafts"], json!([draft]));

    // Once the window is gone.
    drop(mark);
    let updated = f.call(calls[0].0, calls[0].1.clone()).await;
    assert_eq!(updated["subject"], "Other plans");
}
