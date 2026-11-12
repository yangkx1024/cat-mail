//! Writing drafts, replies and forwards.

use super::*;

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
async fn files_put_to_an_upload_go_into_drafts() {
    let f = Fixture::new(ServeOptions::default()).await;
    let addr = f.server.local_addr();
    let account = f.call("list_accounts", json!({})).await["accounts"][0]["id"].clone();
    let put = |path: String, headers: &'static [(&'static str, &'static str)], body| async move {
        send_bytes(addr, http::Method::PUT, &path, headers, body).await
    };
    let failure = |result: Value| {
        assert_eq!(result["isError"], true, "{result}");
        result["content"][0]["text"].as_str().unwrap().to_owned()
    };

    let prepared = f.call("prepare_upload", json!({})).await;
    let handle = prepared["upload"].as_str().unwrap().to_owned();
    let path = format!("/mcp/uploads/{handle}");
    // On the address the request came to.
    assert_eq!(
        prepared["url"],
        format!("http://localhost:{}{path}", addr.port())
    );
    let given = |name: &str| json!([{"name": name, "upload": handle}]);
    let early = f
        .call_result(
            "create_draft",
            json!({"account": account, "attachments": given("receipt.pdf")}),
        )
        .await;
    let early = failure(early);
    assert!(early.starts_with("invalid: nothing was put"), "{early}");
    // Logged as it is, so it doesn't give away the handle, which takes a file still.
    assert!(
        early.contains("receipt.pdf") && !early.contains(&handle),
        "{early}"
    );

    // Not from a web page, and only put.
    let (status, _) = put(path.clone(), &[("origin", "https://evil.example")], vec![1])
        .await
        .unwrap();
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send_bytes(addr, http::Method::GET, &path, &[], Vec::new())
        .await
        .unwrap();
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _) = put("/mcp/uploads/made-up".into(), &[], vec![1])
        .await
        .unwrap();
    assert_eq!(status, StatusCode::NOT_FOUND);

    // No token: the URL is enough.
    let receipt: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8).collect();
    let (status, body) = put(path.clone(), &[], receipt.clone()).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(&handle), "{body}");
    let (status, _) = put(path.clone(), &[], vec![1]).await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT);

    // A call that fails leaves the file for the next.
    let refused = f
        .call_result(
            "create_draft",
            json!({"account": account, "attachments": given("../receipt.pdf")}),
        )
        .await;
    assert!(failure(refused).starts_with("invalid"));
    let both = json!([{"name": "a.pdf", "base64": "eA==", "upload": handle}]);
    let refused = f
        .call_result(
            "create_draft",
            json!({"account": account, "attachments": both}),
        )
        .await;
    assert!(failure(refused).starts_with("invalid: give a.pdf"));
    let twice = json!([
        {"name": "a.pdf", "upload": handle},
        {"name": "b.pdf", "upload": handle},
    ]);
    let refused = f
        .call_result(
            "create_draft",
            json!({"account": account, "attachments": twice}),
        )
        .await;
    let refused = failure(refused);
    assert!(
        refused.starts_with("invalid: the upload of b.pdf"),
        "{refused}"
    );
    assert!(!refused.contains(&handle), "{refused}");

    let draft = f
        .call(
            "create_draft",
            json!({"account": account, "attachments": given("receipt.pdf")}),
        )
        .await;
    assert_eq!(draft["attachments"][0]["filename"], "receipt.pdf");
    assert_eq!(draft["attachments"][0]["mime_type"], "application/pdf");
    assert_eq!(draft["attachments"][0]["size"], receipt.len());
    let id = draft["id"].clone();
    // Used once.
    let again = f
        .call_result(
            "attach",
            json!({"draft": id, "name": "receipt.pdf", "upload": handle}),
        )
        .await;
    assert!(failure(again).starts_with("not_found: no such upload"));
    let (status, _) = put(path, &[], vec![1]).await.unwrap();
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Another token's uploads aren't this one's.
    let other = f.token(
        "other",
        Access {
            tier: Tier::Draft,
            accounts: None,
        },
    );
    let theirs = f.call_as(&other, "prepare_upload", json!({})).await;
    let theirs = theirs["upload"].as_str().unwrap().to_owned();
    let (status, _) = put(format!("/mcp/uploads/{theirs}"), &[], b"notes".to_vec())
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK);
    let attach = json!({"draft": id, "name": "notes.txt", "upload": theirs});
    let refused = f.call_result("attach", attach.clone()).await;
    assert!(failure(refused).starts_with("not_found: no such upload"));
    let attached = f.call_as(&other, "attach", attach).await;
    assert_eq!(attached["attachments"][1]["filename"], "notes.txt");

    // At most 10 MB; one too large can be put again.
    let prepared = f.call("prepare_upload", json!({})).await;
    let path = format!("/mcp/uploads/{}", prepared["upload"].as_str().unwrap());
    match put(path.clone(), &[], vec![0; 10 * 1024 * 1024 + 1]).await {
        Ok((status, body)) => assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}"),
        Err(e) => assert!(e.is_incomplete_message() || is_reset(&e), "{e:?}"),
    }
    let (status, body) = put(path, &[], vec![1]).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
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
