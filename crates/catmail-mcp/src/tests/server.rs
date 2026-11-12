//! Tokens, tiers and accounts, what the server turns away, and stopping it.

use super::*;

/// The tools' list as agents see it. Regenerate with `UPDATE_SNAPSHOTS=1` after a change
/// meant to alter what agents read.
const TOOLS_SNAPSHOT: &str = include_str!("tools.snapshot.json");

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
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tests/tools.snapshot.json");
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
        "prepare_upload",
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
    let auth = format!("Bearer {read}");
    let headers = [("authorization", auth.as_str())];
    // Turned away once a megabyte is read: answered, or cut off while the rest is still
    // being written.
    match try_send(f.server.local_addr(), "/mcp", &headers, large).await {
        Ok((status, body)) => assert_ne!(status, StatusCode::OK, "{body}"),
        Err(e) => assert!(e.is_incomplete_message() || is_reset(&e), "{e:?}"),
    }
}
