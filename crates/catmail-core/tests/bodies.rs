//! Body and attachment downloads, search and blob GC against the fake IMAP server.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use catmail_core::{EngineTuning, Error, Event, Message, MessageId, SearchQuery};
use catmail_testkit::{FakeHttp, Reply, ServerState, SmtpState, response};

mod common;

use common::{Env, drain, env_built, eventually, files, synced_env as env};

impl Env {
    /// Message ids in INBOX, by UID: the fixtures' dates say nothing about it.
    async fn id(&self, uid_order: usize) -> MessageId {
        let id = rusqlite::Connection::open(self.client.config().db_path())
            .unwrap()
            .query_row(
                "SELECT m.id FROM messages m JOIN folders f ON f.id = m.folder_id
                 WHERE f.remote_name = 'INBOX' ORDER BY m.uid LIMIT 1 OFFSET ?1",
                [uid_order as i64],
                |r| r.get(0),
            )
            .unwrap();
        MessageId(id)
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
    let mut events = env.client.subscribe();
    let data = env.client.attachment_content(video.id).await.unwrap();
    assert!(
        env.commands_since(start)
            .iter()
            .any(|c| c.contains("BODY.PEEK[2]"))
    );
    // Other views of the message learn that it is stored, and its exact size.
    let folder = message.summary.folder_id;
    assert_eq!(
        events.try_recv().unwrap(),
        Event::MessagesChanged {
            account: env.account,
            folder
        }
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
    assert!(drain(&mut events).contains(&Event::MessagesChanged {
        account: env.account,
        folder
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_whole_message_limit_is_tunable() {
    let env = env_built(state(&[NEWSLETTER]), SmtpState::new("alice", "pw"), |b| {
        b.tuning(EngineTuning {
            whole_message_limit: 512,
            ..EngineTuning::default()
        })
    })
    .await;
    env.client.sync_account(env.account).await.unwrap();
    let start = env.log_len();

    let message = env.client.download_message(env.id(0).await).await.unwrap();
    let commands = env.commands_since(start);
    assert!(
        commands.iter().any(|c| c.contains("BODYSTRUCTURE")),
        "{commands:?}"
    );
    assert!(
        !commands.iter().any(|c| c.contains("BODY.PEEK[]")),
        "{commands:?}"
    );
    let b = body(&message);
    assert!(b.html.as_deref().is_some_and(|h| h.contains("请查收")));
    let pdf = b
        .attachments
        .iter()
        .find(|a| a.mime_type == "application/pdf")
        .unwrap();
    assert!(!pdf.downloaded);
}

#[tokio::test(flavor = "multi_thread")]
async fn text_too_large_to_fetch_can_be_opened_as_an_attachment() {
    let mut log = Vec::new();
    for i in 0..(3 * 1024 * 1024 / 64) {
        log.extend_from_slice(format!("{i:>8} 构建日志 {}\r\n", "x".repeat(40)).as_bytes());
    }
    let header = b"From: Bob <bob@example.com>\r\n\
        Subject: Nightly log\r\n\
        Content-Type: text/plain; charset=utf-8\r\n\r\n";
    let raw = [&header[..], &log].concat();
    let env = env(state(&[&raw])).await;
    let id = env.id(0).await;
    let start = env.log_len();

    let message = env.client.download_message(id).await.unwrap();
    assert!(
        !env.commands_since(start)
            .iter()
            .any(|c| c.contains("BODY.PEEK[1]") || c.contains("BODY.PEEK[]")),
        "the text is too large to fetch with the body"
    );
    assert!(message.summary.has_attachments);
    let b = body(&message);
    assert_eq!(b.text, None);
    let [text] = &b.attachments[..] else {
        panic!("{:?}", b.attachments)
    };
    assert_eq!(
        (text.mime_type.as_str(), text.inline, text.downloaded),
        ("text/plain", false, false)
    );

    let data = env.client.attachment_content(text.id).await.unwrap();
    assert_eq!(data, log);
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
async fn malformed_bodystructure_falls_back_to_the_whole_message() {
    // The line parses, but without the BODYSTRUCTURE asked for: not a message gone.
    let raw = large_message();
    let env = env(state(&[&raw])).await;
    let id = env.id(0).await;
    env.fake.state().faults.broken_bodystructure = vec![1];
    let start = env.log_len();

    let message = env.client.download_message(id).await.unwrap();
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
    let inbox = env.folder("INBOX").await.id;
    let mut events = env.client.subscribe();

    // Gone, as the server confirms: the local copy goes, as the next sync would drop it.
    env.fake.state().inbox().expunge(1);
    let start = env.log_len();
    let err = env.client.download_message(first).await.unwrap_err();
    assert!(matches!(err, Error::OutOfSync(_)), "{err:?}");
    let commands = env.commands_since(start);
    assert!(
        commands.iter().any(|c| c == "UID SEARCH UID 1"),
        "{commands:?}"
    );
    assert!(env.client.get_message(first).await.unwrap().is_none());
    assert_eq!(
        drain(&mut events),
        [Event::MessagesChanged {
            account: env.account,
            folder: inbox
        }]
    );

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

#[tokio::test(flavor = "multi_thread")]
async fn gc_runs_in_the_background_when_asked_to() {
    let env = env_built(state(&[NEWSLETTER]), SmtpState::new("alice", "pw"), |b| {
        b.collect_garbage_every(Some(Duration::from_millis(50)))
    })
    .await;
    env.client.sync_account(env.account).await.unwrap();
    env.client.download_message(env.id(0).await).await.unwrap();
    let blobs = env.client.config().blob_dir();
    age_files(&blobs);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(count_files(&blobs), 3, "all referenced");

    env.client.remove_account(env.account).await.unwrap();
    eventually("the blobs to go", async || count_files(&blobs) == 0).await;
}

fn count_files(dir: &Path) -> usize {
    files(dir).len()
}

/// Backdates every file under `dir` past the GC grace period.
fn age_files(dir: &Path) {
    for path in files(dir) {
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7 * 24 * 3600))
            .unwrap();
    }
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

/// A web server where `/logo.png` answers, `/hang` waits forever, and the rest are
/// missing.
async fn image_server() -> FakeHttp {
    FakeHttp::start(|path| match path {
        "/logo.png" => Reply::Send(response("200 OK", &[], PNG)),
        "/hang" => Reply::Held(response("200 OK", &[], PNG)),
        _ => Reply::Send(response("404 Not Found", &[], b"")),
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn fetches_remote_images_as_they_come_until_dropped() {
    let server = image_server().await;
    let env = env(ServerState::new("alice", "pw")).await;
    let urls = vec![
        server.url("/hang"),
        server.url("/missing.gif"),
        server.url("/logo.png"),
        "file:///etc/passwd".to_owned(),
    ];
    let mut stream = env.client.fetch_remote_images(urls.clone()).await.unwrap();
    // All but the one that hangs come, in whatever order they finish.
    let mut fetched = Vec::new();
    for _ in 0..3 {
        fetched.push(stream.next().await.unwrap());
    }
    fetched.sort_by_key(|(url, _)| urls.iter().position(|u| u == url));
    let got: Vec<_> = fetched.iter().map(|(url, _)| url.clone()).collect();
    assert_eq!(got, urls[1..]);
    assert!(matches!(&fetched[0].1, Err(Error::Http(_))));
    let image = fetched[1].1.as_ref().unwrap();
    assert_eq!(
        (image.mime_type.as_str(), image.bytes.as_slice()),
        ("image/png", PNG)
    );
    assert!(matches!(&fetched[2].1, Err(e) if e.kind() == catmail_core::ErrorKind::Invalid));
    assert!(stream.next_ready().is_none());

    // Dropped, it stops the fetch still going instead of waiting out its timeout. Only
    // a request the server holds can be seen to be given up on: on a busy machine the
    // others can all finish before it is even sent.
    eventually("the hanging request to arrive", async || {
        server.heads().iter().any(|h| h.starts_with("GET /hang "))
    })
    .await;
    drop(stream);
    eventually("the hanging request to be dropped", async || {
        server.abandoned() == 1
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_images_shown_once_come_from_the_cache_until_cleared() {
    let server = image_server().await;
    let env = env(ServerState::new("alice", "pw")).await;
    let (logo, missing) = (server.url("/logo.png"), server.url("/missing.gif"));
    let urls = vec![logo.clone(), missing.clone()];
    let fetch_all = async |urls: Vec<String>| {
        let mut stream = env.client.fetch_remote_images(urls).await.unwrap();
        let mut fetched = Vec::new();
        while let Some(one) = stream.next().await {
            fetched.push(one);
        }
        fetched
    };

    assert!(
        env.client
            .cached_remote_images(urls.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(env.client.image_cache_size().await.unwrap(), 0);
    fetch_all(urls.clone()).await;
    assert_eq!(server.requests(), 2);

    // Kept: it comes from the cache, without a request; what failed isn't kept.
    let cached = env.client.cached_remote_images(urls.clone()).await.unwrap();
    assert_eq!(cached.len(), 1);
    assert_eq!(
        (cached[0].0.as_str(), cached[0].1.bytes.as_slice()),
        (logo.as_str(), PNG)
    );
    let fetched = fetch_all(urls.clone()).await;
    assert_eq!(fetched.len(), 2);
    assert!(
        fetched
            .iter()
            .any(|(url, image)| *url == logo && image.is_ok())
    );
    assert_eq!(server.requests(), 3, "only the missing one is asked again");
    let size = env.client.image_cache_size().await.unwrap();
    assert!(size > PNG.len() as u64, "{size}");

    // Cleared, it is fetched again.
    assert_eq!(env.client.clear_image_cache().await.unwrap(), size);
    assert_eq!(env.client.image_cache_size().await.unwrap(), 0);
    assert!(
        env.client
            .cached_remote_images(urls.clone())
            .await
            .unwrap()
            .is_empty()
    );
    fetch_all(vec![logo]).await;
    assert_eq!(server.requests(), 4);
}
