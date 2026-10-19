//! Body and attachment downloads, search and blob GC against the fake IMAP server.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use catmail_core::{Error, Event, Message, MessageId, SearchQuery};
use catmail_testkit::ServerState;

mod common;

use common::Env;

/// Synced, unlike the plain shared one.
async fn env(state: ServerState) -> Env {
    let env = common::env(state).await;
    env.client.sync_account(env.account).await.unwrap();
    env
}

impl Env {
    /// Message ids in INBOX, by UID.
    async fn id(&self, uid_order: usize) -> MessageId {
        let inbox = self
            .client
            .find_folder(self.account, "INBOX")
            .await
            .unwrap()
            .unwrap();
        let mut list = self.client.list_messages(inbox.id, 1000, 0).await.unwrap();
        list.reverse(); // oldest (lowest UID) first; the fake gives every message one date
        list[uid_order].id
    }

    fn log_len(&self) -> usize {
        self.fake.state().log.len()
    }

    async fn search(&self, text: &str) -> Vec<String> {
        self.client
            .search(SearchQuery::new(text))
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.subject.unwrap_or_default())
            .collect()
    }
}

const NEWSLETTER: &[u8] = b"From: =?UTF-8?B?5byg5LiJ?= <zhang@example.com>\r\n\
    To: alice@example.com\r\n\
    Subject: =?UTF-8?B?56ys5LiJ5a2j5bqm5oql5ZGK?=\r\n\
    Date: Sat, 17 Oct 2026 09:30:00 +0800\r\n\
    MIME-Version: 1.0\r\n\
    Content-Type: multipart/mixed; boundary=\"outer\"\r\n\r\n\
    --outer\r\n\
    Content-Type: multipart/related; boundary=\"rel\"\r\n\r\n\
    --rel\r\n\
    Content-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n\
    --alt\r\n\
    Content-Type: text/plain; charset=gbk\r\n\
    Content-Transfer-Encoding: base64\r\n\r\n\
    x+uy6crVuL28/qGj\r\n\
    --alt\r\n\
    Content-Type: text/html; charset=utf-8\r\n\r\n\
    <table><tr><td>\xe8\xaf\xb7\xe6\x9f\xa5\xe6\x94\xb6<img src=\"cid:logo@x\"></td></tr></table>\
    <script>alert(1)</script>\r\n\
    --alt--\r\n\
    --rel\r\n\
    Content-Type: image/png\r\n\
    Content-ID: <logo@x>\r\n\
    Content-Transfer-Encoding: base64\r\n\r\n\
    iVBORw0KGgo=\r\n\
    --rel--\r\n\
    --outer\r\n\
    Content-Type: application/pdf; name=\"=?UTF-8?B?5oql5ZGKLnBkZg==?=\"\r\n\
    Content-Disposition: attachment\r\n\
    Content-Transfer-Encoding: base64\r\n\r\n\
    JVBERi0xLjQK\r\n\
    --outer--\r\n";

/// Over the whole-message limit: text plus a large base64 attachment.
fn large_message() -> Vec<u8> {
    let mut raw = b"From: Bob <bob@example.com>\r\n\
        Subject: Big file\r\n\
        Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
        --b\r\n\
        Content-Type: text/plain\r\n\r\n\
        The video is attached.\r\n\
        --b\r\n\
        Content-Type: video/mp4; name=\"clip.mp4\"\r\n\
        Content-Disposition: attachment; filename=\"clip.mp4\"\r\n\
        Content-Transfer-Encoding: base64\r\n\r\n"
        .to_vec();
    // 3 MiB of 'A' encodes to zeros.
    for _ in 0..(3 * 1024 * 1024 / 76) {
        raw.extend_from_slice(&[b'A'; 76]);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"--b--\r\n");
    raw
}

fn state(messages: &[&[u8]]) -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    for raw in messages {
        state.inbox().append(raw, &[]);
    }
    state
}

fn body(message: &Message) -> &catmail_core::MessageBody {
    message.body.as_ref().expect("downloaded")
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_a_small_message_whole() {
    let env = env(state(&[NEWSLETTER])).await;
    let id = env.id(0).await;
    assert!(
        env.client
            .get_message(id)
            .await
            .unwrap()
            .unwrap()
            .body
            .is_none()
    );
    assert!(
        env.search("请查收").await.is_empty(),
        "body not downloaded yet"
    );
    let mut events = env.client.subscribe();
    let start = env.log_len();

    let message = env.client.download_message(id).await.unwrap();
    let commands = env.commands_since(start);
    assert!(
        commands.iter().any(|c| c.starts_with("EXAMINE")),
        "{commands:?}"
    );
    assert!(!commands.iter().any(|c| c.starts_with("SELECT")));
    assert!(commands.iter().any(|c| c.contains("BODY.PEEK[]")));
    assert!(!commands.iter().any(|c| c.contains("BODY[")));
    assert!(matches!(
        events.recv().await.unwrap(),
        Event::MessagesChanged { .. }
    ));

    let b = body(&message);
    assert_eq!(b.text.as_deref(), Some("请查收附件。"));
    let html = b.html.as_deref().unwrap();
    assert!(
        html.contains("<table>") && html.contains("cid:logo@x"),
        "{html}"
    );
    assert!(!html.contains("script"), "{html}");
    let parts: Vec<_> = b
        .attachments
        .iter()
        .map(|a| {
            (
                a.filename.as_deref(),
                a.mime_type.as_str(),
                a.inline,
                a.downloaded,
            )
        })
        .collect();
    assert_eq!(
        parts,
        [
            (None, "image/png", true, true),
            (Some("报告.pdf"), "application/pdf", false, true),
        ]
    );
    assert!(message.summary.has_attachments);
    assert_eq!(message.summary.snippet.as_deref(), Some("请查收附件。"));
    assert_eq!(message.to[0].email, "alice@example.com");

    // Everything is local now.
    let start = env.log_len();
    let again = env.client.download_message(id).await.unwrap();
    assert_eq!(again, message);
    let pdf = env
        .client
        .attachment_content(b.attachments[1].id)
        .await
        .unwrap();
    assert_eq!(pdf, b"%PDF-1.4\n");
    assert_eq!(env.client.raw_message(id).await.unwrap(), NEWSLETTER);
    assert_eq!(env.commands_since(start), Vec::<String>::new());

    assert_eq!(env.search("请查收").await, ["第三季度报告"]);
    assert_eq!(env.search("季度报告 from:张三").await, ["第三季度报告"]);
    assert_eq!(env.client.get_message(id).await.unwrap().unwrap(), message);
    assert!(
        env.fake.state().inbox().messages[0].flags.is_empty(),
        "still unread"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn large_messages_leave_attachments_on_the_server_until_opened() {
    let raw = large_message();
    let env = env(state(&[&raw])).await;
    let id = env.id(0).await;
    let start = env.log_len();

    let message = env.client.download_message(id).await.unwrap();
    let commands = env.commands_since(start);
    assert!(
        commands.iter().any(|c| c.contains("BODYSTRUCTURE")),
        "{commands:?}"
    );
    assert!(commands.iter().any(|c| c.contains("BODY.PEEK[1]")));
    assert!(
        !commands
            .iter()
            .any(|c| c.contains("BODY.PEEK[]") || c.contains("BODY.PEEK[2]")),
        "{commands:?}"
    );
    let b = body(&message);
    assert_eq!(
        b.text.as_deref().map(str::trim),
        Some("The video is attached.")
    );
    let video = &b.attachments[0];
    assert_eq!(video.filename.as_deref(), Some("clip.mp4"));
    assert!(!video.downloaded);
    let decoded = 3 * 1024 * 1024 / 76 * 57;
    let estimate = video.size.unwrap();
    assert!(estimate.abs_diff(decoded) < decoded / 100, "{estimate}");

    let start = env.log_len();
    let data = env.client.attachment_content(video.id).await.unwrap();
    assert!(
        env.commands_since(start)
            .iter()
            .any(|c| c.contains("BODY.PEEK[2]"))
    );
    assert_eq!(data.len() as u32, decoded);
    assert!(data.iter().all(|&b| b == 0));
    let stored = env.client.get_message(id).await.unwrap().unwrap();
    let video = &body(&stored).attachments[0];
    assert!(video.downloaded);
    assert_eq!(video.size, Some(data.len() as u32), "exact once downloaded");

    let start = env.log_len();
    assert_eq!(env.client.attachment_content(video.id).await.unwrap(), data);
    assert!(
        env.commands_since(start).is_empty(),
        "read from the blob store"
    );
    assert_eq!(env.client.raw_message(id).await.unwrap(), raw);
    assert!(
        env.commands_since(start)
            .iter()
            .any(|c| c.contains("BODY.PEEK[]"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_message_of_a_large_message_fetches_it_whole_once() {
    let raw = large_message();
    let env = env(state(&[&raw])).await;
    let id = env.id(0).await;
    let start = env.log_len();

    assert_eq!(env.client.raw_message(id).await.unwrap(), raw);
    let fetches: Vec<_> = env
        .commands_since(start)
        .into_iter()
        .filter(|c| c.contains("FETCH"))
        .collect();
    assert_eq!(fetches.len(), 1, "{fetches:?}");
    assert!(fetches[0].contains("BODY.PEEK[]"));
    let message = env.client.get_message(id).await.unwrap().unwrap();
    assert!(
        body(&message).attachments[0].downloaded,
        "stored from the whole message"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unparsable_bodystructure_falls_back_to_the_whole_message() {
    let raw = large_message();
    let env = env(state(&[&raw])).await;
    let id = env.id(0).await;
    env.fake.state().faults.garble = Some("BODYSTRUCTURE".into());

    let message = env.client.download_message(id).await.unwrap();
    let b = body(&message);
    assert_eq!(
        b.text.as_deref().map(str::trim),
        Some("The video is attached.")
    );
    assert!(b.attachments[0].downloaded);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_text_sections_fall_back_to_the_whole_message() {
    // A valid answer that leaves out the text (NIL) must not be stored as the body.
    let raw = large_message();
    let env = env(state(&[&raw])).await;
    env.fake.state().faults.nil_sections = true;
    let start = env.log_len();

    let message = env.client.download_message(env.id(0).await).await.unwrap();
    assert!(
        env.commands_since(start)
            .iter()
            .any(|c| c.contains("BODY.PEEK[]"))
    );
    let b = body(&message);
    assert_eq!(
        b.text.as_deref().map(str::trim),
        Some("The video is attached.")
    );
    assert!(b.attachments[0].downloaded);
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_on_a_new_connection_when_every_idle_one_died() {
    let env = env(state(&[NEWSLETTER, NEWSLETTER, NEWSLETTER])).await;
    let (first, second, third) = (env.id(0).await, env.id(1).await, env.id(2).await);

    // Two downloads at once leave two idle connections behind.
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    env.fake.state().faults.body_fetch_gate = Some(gate.clone());
    let connections = env.fake.state().connections;
    let downloads = tokio::spawn({
        let client = env.client.clone();
        async move {
            let (a, b) = tokio::join!(
                client.download_message(first),
                client.download_message(second)
            );
            a.and(b)
        }
    });
    while env
        .commands_since(0)
        .iter()
        .filter(|c| c.contains("BODY.PEEK[]"))
        .count()
        < 2
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    gate.add_permits(2);
    downloads.await.unwrap().unwrap();
    assert_eq!(env.fake.state().connections, connections + 2);

    {
        let mut st = env.fake.state();
        st.faults.body_fetch_gate = None;
        st.drop_open_connections();
    }
    env.client.download_message(third).await.unwrap();
    assert_eq!(env.fake.state().connections, connections + 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnects_when_an_idle_connection_died() {
    let env = env(state(&[NEWSLETTER, NEWSLETTER])).await;
    env.client.download_message(env.id(0).await).await.unwrap();
    let connections = env.fake.state().connections;

    // The kept connection dies with the next body fetch; one retry on a new connection.
    env.fake.state().faults.drop_on_body_fetch = Some(2);
    env.client.download_message(env.id(1).await).await.unwrap();
    assert_eq!(env.fake.state().connections, connections + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_download_stores_nothing_and_can_be_retried() {
    let env = env(state(&[NEWSLETTER])).await;
    let id = env.id(0).await;
    env.fake.state().faults.drop_on_body_fetch = Some(1);

    let err = env.client.download_message(id).await.unwrap_err();
    assert!(matches!(err, Error::Imap(_)), "{err:?}");
    assert!(
        env.client
            .get_message(id)
            .await
            .unwrap()
            .unwrap()
            .body
            .is_none()
    );

    let message = env.client.download_message(id).await.unwrap();
    assert_eq!(body(&message).attachments.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_uids_from_an_old_uidvalidity_or_expunged_messages() {
    let env = env(state(&[NEWSLETTER, NEWSLETTER])).await;
    let (first, second) = (env.id(0).await, env.id(1).await);

    env.fake.state().inbox().expunge(1);
    let err = env.client.download_message(first).await.unwrap_err();
    assert!(matches!(err, Error::OutOfSync(_)), "{err:?}");

    // UID 2 now names a different message (or none); it must not be fetched.
    env.fake.state().inbox().reset_uidvalidity(99);
    let start = env.log_len();
    let err = env.client.download_message(second).await.unwrap_err();
    assert!(matches!(err, Error::OutOfSync(_)), "{err:?}");
    assert!(
        !env.commands_since(start)
            .iter()
            .any(|c| c.contains("FETCH"))
    );
    assert!(
        env.client
            .get_message(second)
            .await
            .unwrap()
            .unwrap()
            .body
            .is_none()
    );

    // Sync drops the old generation; the message is downloadable under its new id.
    env.client.sync_account(env.account).await.unwrap();
    assert!(env.client.get_message(second).await.unwrap().is_none());
    let message = env.client.download_message(env.id(0).await).await.unwrap();
    assert!(message.body.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_downloads_store_one_body() {
    let env = env(state(&[NEWSLETTER])).await;
    let id = env.id(0).await;
    let (a, b) = tokio::join!(
        env.client.download_message(id),
        env.client.download_message(id)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let message = env.client.get_message(id).await.unwrap().unwrap();
    assert_eq!(body(&message).attachments.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_the_account_cancels_a_download() {
    let env = env(state(&[NEWSLETTER])).await;
    let id = env.id(0).await;
    env.fake.state().faults.hang_on_body_fetch = true;

    let download = tokio::spawn({
        let client = env.client.clone();
        async move { client.download_message(id).await }
    });
    // Let it reach the hanging FETCH.
    while !env
        .commands_since(0)
        .iter()
        .any(|c| c.contains("BODY.PEEK[]"))
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::timeout(
        Duration::from_secs(5),
        env.client.remove_account(env.account),
    )
    .await
    .expect("removal waited for the network")
    .unwrap();
    let result = download.await.unwrap();
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert!(matches!(
        env.client.download_message(id).await,
        Err(Error::MessageNotFound(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn gc_frees_the_blobs_of_removed_accounts() {
    let env = env(state(&[NEWSLETTER])).await;
    env.client.download_message(env.id(0).await).await.unwrap();
    let blobs = env.client.config().blob_dir();
    age_files(&blobs);
    assert_eq!(
        env.client.collect_garbage().await.unwrap().removed,
        0,
        "all referenced"
    );

    env.client.remove_account(env.account).await.unwrap();
    let report = env.client.collect_garbage().await.unwrap();
    // The raw message, the image and the PDF.
    assert_eq!(report.removed, 3);
    assert_eq!(env.client.collect_garbage().await.unwrap().removed, 0);
}

/// Backdates every file under `dir` past the GC grace period.
fn age_files(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            age_files(&path);
        } else {
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(7 * 24 * 3600))
                .unwrap();
        }
    }
}
