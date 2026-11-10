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
