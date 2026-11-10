//! Listing, reading and syncing.

use super::*;

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
