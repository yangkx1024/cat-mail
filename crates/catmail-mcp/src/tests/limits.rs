//! How often a token calls and how much it downloads.

use super::*;

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

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_failing_after_its_download_keeps_it_charged() {
    let f = Fixture::new(ServeOptions::default()).await;
    let ids = f.ids(None).await;
    let mut sizes = Vec::new();
    for &id in &ids {
        let message = f.client.get_message(MessageId(id)).await.unwrap().unwrap();
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
    // The file's name is checked once the message is here.
    let bad_file = json!([{"name": "../bad.txt", "base64": "aGk="}]);
    let refused = f
        .call_result(
            "reply",
            json!({"message": ids[0], "text": "hi", "attachments": bad_file}),
        )
        .await;
    assert_eq!(refused["isError"], true, "{refused}");
    let message = f.client.get_message(MessageId(ids[0])).await.unwrap();
    assert!(message.unwrap().body.is_some());
    let refused = f.call_result("read_message", json!({"id": ids[1]})).await;
    let text = refused["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("over_limit"), "{text}");
}
