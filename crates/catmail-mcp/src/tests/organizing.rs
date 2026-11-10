//! Changes to messages, here and on the server.

use super::*;

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
