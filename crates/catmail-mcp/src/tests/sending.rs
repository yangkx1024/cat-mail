//! Sending drafts: asking the user through the agent's client first, or not.

use super::*;

/// The protocol version with multi round-trip requests, which asking needs.
const ASKING_VERSION: &str = "2026-07-28";

impl Fixture {
    /// A token of `tier` for every account.
    fn token_of(&self, name: &str, tier: Tier) -> String {
        self.token(
            name,
            Access {
                tier,
                accounts: None,
            },
        )
    }

    /// A draft to Bob, from the fixture's token.
    async fn draft_to_bob(&self) -> i64 {
        let account = self.call("list_accounts", json!({})).await["accounts"][0]["id"].clone();
        let draft = self
            .call(
                "create_draft",
                json!({
                    "account": account,
                    "to": ["Bob <bob@example.com>"],
                    "subject": "Plans",
                    "text": "Shall we?",
                }),
            )
            .await;
        draft["id"].as_i64().unwrap()
    }

    /// `send_draft` of `draft` with `secret`, from a client on the protocol version that
    /// can ask its user (elicitation), with `retry` added to the call's params (the
    /// answer, and the state to echo). The JSON-RPC reply's `result`, or its `error`.
    async fn send_asking(&self, secret: &str, draft: i64, retry: Option<Value>) -> Value {
        let mut params = json!({
            "name": "send_draft",
            "arguments": {"draft": draft},
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": ASKING_VERSION,
                "io.modelcontextprotocol/clientCapabilities": {"elicitation": {}},
                "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
            },
        });
        if let Some(Value::Object(retry)) = retry {
            params.as_object_mut().unwrap().extend(retry);
        }
        let headers = [
            ("mcp-protocol-version", ASKING_VERSION),
            ("mcp-method", "tools/call"),
            ("mcp-name", "send_draft"),
        ];
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params});
        let (status, body) = self.post_with(secret, &headers, body).await;
        result_of(status, &body)
    }

    async fn outbox(&self) -> Vec<Value> {
        self.call("list_outbox", json!({})).await["entries"]
            .as_array()
            .unwrap()
            .clone()
    }
}

/// The answer to the question `asked` put, with the state to echo.
fn answer(asked: &Value, action: &str) -> Option<Value> {
    Some(json!({
        "inputResponses": {"send": {"action": action}},
        "requestState": asked["requestState"],
    }))
}

#[tokio::test(flavor = "multi_thread")]
async fn sending_is_a_tier_of_its_own() {
    let f = Fixture::new(ServeOptions::default()).await;
    let drafts = f.token_of("drafts", Tier::Draft);
    assert!(!f.tool_names(&drafts).await.contains(&"send_draft".into()));
    for (name, tier) in [("asks", Tier::Send), ("unasked", Tier::SendUnasked)] {
        let secret = f.token_of(name, tier);
        assert!(f.tool_names(&secret).await.contains(&"send_draft".into()));
    }
    let draft = f.draft_to_bob().await;
    let refused = f
        .call_result_as(&drafts, "send_draft", json!({"draft": draft}))
        .await;
    assert_eq!(refused["message"], "tool not found", "{refused}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_goes_out_once_the_user_confirms_it() {
    let f = Fixture::new(ServeOptions::default()).await;
    let secret = f.token_of("laptop", Tier::Send);
    let draft = f.draft_to_bob().await;

    let asked = f.send_asking(&secret, draft, None).await;
    assert_eq!(asked["resultType"], "input_required", "{asked}");
    let question = &asked["inputRequests"]["send"];
    assert_eq!(question["method"], "elicitation/create", "{asked}");
    let message = question["params"]["message"].as_str().unwrap();
    assert!(message.contains("\"laptop\""), "{message}");
    assert!(
        message.contains("From: Alice <alice@example.com>\n"),
        "{message}"
    );
    assert!(message.contains("To: Bob <bob@example.com>\n"), "{message}");
    assert!(message.contains("Subject: Plans\n"), "{message}");
    assert!(message.ends_with("\n\nShall we?"), "{message}");
    // Asking sends nothing.
    assert!(f.outbox().await.is_empty());

    let sent = f
        .send_asking(&secret, draft, answer(&asked, "accept"))
        .await;
    assert_eq!(sent["isError"], json!(false), "{sent}");
    assert_eq!(sent["structuredContent"]["status"], "queued", "{sent}");
    assert_eq!(
        sent["structuredContent"]["recipients"],
        json!(["bob@example.com"])
    );
    assert_eq!(f.outbox().await.len(), 1);
    assert!(
        f.call("list_drafts", json!({})).await["drafts"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // The answer is used up.
    let again = f
        .send_asking(&secret, draft, answer(&asked, "accept"))
        .await;
    assert_eq!(again["isError"], json!(true), "{again}");
    assert_eq!(f.outbox().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_the_user_declines_stays_a_draft() {
    let f = Fixture::new(ServeOptions::default()).await;
    let secret = f.token_of("laptop", Tier::Send);
    let draft = f.draft_to_bob().await;
    for action in ["decline", "cancel"] {
        let asked = f.send_asking(&secret, draft, None).await;
        let declined = f.send_asking(&secret, draft, answer(&asked, action)).await;
        assert_eq!(declined["isError"], json!(true), "{declined}");
        let text = declined["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("declined: "), "{text}");
    }
    assert!(f.outbox().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn only_what_the_user_was_shown_is_sent() {
    let f = Fixture::new(ServeOptions::default()).await;
    let secret = f.token_of("laptop", Tier::Send);
    let draft = f.draft_to_bob().await;

    // Changed between the question and the answer.
    let asked = f.send_asking(&secret, draft, None).await;
    f.call(
        "update_draft",
        json!({"draft": draft, "to": ["mallory@example.com"]}),
    )
    .await;
    let refused = f
        .send_asking(&secret, draft, answer(&asked, "accept"))
        .await;
    assert_eq!(refused["isError"], json!(true), "{refused}");
    let text = refused["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("conflict: "), "{text}");
    assert!(f.outbox().await.is_empty());

    // An answer to no question, or to another token's, is asked again.
    let made_up = answer(&json!({"requestState": "0123"}), "accept");
    let asked_again = f.send_asking(&secret, draft, made_up).await;
    assert_eq!(asked_again["resultType"], "input_required", "{asked_again}");
    let other = f.token_of("phone", Tier::Send);
    let asked = f.send_asking(&secret, draft, None).await;
    let asked_again = f.send_asking(&other, draft, answer(&asked, "accept")).await;
    assert_eq!(asked_again["resultType"], "input_required", "{asked_again}");
    assert!(f.outbox().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_cant_ask_sends_only_with_a_token_that_needs_no_asking() {
    let f = Fixture::new(ServeOptions::default()).await;
    let draft = f.draft_to_bob().await;

    let asks = f.token_of("asks", Tier::Send);
    let refused = f
        .call_result_as(&asks, "send_draft", json!({"draft": draft}))
        .await;
    assert_eq!(refused["isError"], json!(true), "{refused}");
    let text = refused["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("cannot_ask: "), "{text}");
    assert!(f.outbox().await.is_empty());

    let unasked = f.token_of("unasked", Tier::SendUnasked);
    let sent = f
        .call_as(&unasked, "send_draft", json!({"draft": draft}))
        .await;
    assert_eq!(sent["status"], "queued", "{sent}");
    assert_eq!(f.outbox().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_open_in_the_app_isnt_sent() {
    let f = Fixture::new(ServeOptions::default()).await;
    let draft = f.draft_to_bob().await;
    let (_editor, mark, _) = f
        .client
        .edit_draft(catmail_core::DraftId(draft))
        .await
        .unwrap();
    let unasked = f.token_of("unasked", Tier::SendUnasked);
    let refused = f
        .call_result_as(&unasked, "send_draft", json!({"draft": draft}))
        .await;
    let text = refused["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("conflict: draft"), "{text}");
    assert!(text.contains("open in the app"), "{text}");
    drop(mark);
    assert!(f.outbox().await.is_empty());
}
