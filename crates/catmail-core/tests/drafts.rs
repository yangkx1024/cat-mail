//! Drafts copied to the server's Drafts folder, against the fake IMAP server: uploads
//! replacing the copy, deletion with the draft or once it is sent, lost and refused
//! APPENDs, a rebuilt folder, and uploads made in the background once a draft settles.
//! Also forwards, which copy the original's attachments into the draft.

mod common;

use std::time::Duration;

use catmail_core::{Address, DraftContent, DraftId, MailClientBuilder, ReplayReport, SyncSchedule};
use catmail_testkit::{FakeMailbox, ServerState, SmtpState};
use common::{Env, env, env_built, eventually, synced_env};
use mail_parser::{MessageParser, MimeHeaders};

fn content(subject: &str) -> DraftContent {
    DraftContent {
        subject: subject.into(),
        to: vec![Address {
            name: None,
            email: "bob@example.com".into(),
        }],
        bcc: vec![Address {
            name: None,
            email: "dave@example.com".into(),
        }],
        text: "draft text".into(),
        ..Default::default()
    }
}

/// INBOX, Drafts and Sent.
fn state() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    state.inbox().append_simple("hello");
    state
        .mailboxes
        .push(FakeMailbox::new("Drafts").with_attribute("\\Drafts"));
    state
        .mailboxes
        .push(FakeMailbox::new("Sent").with_attribute("\\Sent"));
    state
}

impl Env {
    async fn draft(&self, subject: &str) -> DraftId {
        self.client
            .create_draft(self.account, None, content(subject))
            .await
            .unwrap()
            .id
    }

    /// (subject, flags) of the messages in the server's Drafts folder.
    fn server_drafts(&self) -> Vec<(String, Vec<String>)> {
        let mut state = self.fake.state();
        state
            .mailbox_mut("Drafts")
            .messages
            .iter()
            .map(|m| {
                let parsed = MessageParser::new().parse(&m.raw).unwrap();
                (
                    parsed.subject().unwrap_or_default().to_owned(),
                    m.flags.clone(),
                )
            })
            .collect()
    }

    fn appends(&self) -> usize {
        self.fake.state().received("APPEND")
    }

    async fn on_server(&self, id: DraftId) -> bool {
        self.client.get_draft(id).await.unwrap().unwrap().on_server
    }
}

fn subjects(drafts: &[(String, Vec<String>)]) -> Vec<&str> {
    drafts.iter().map(|(s, _)| s.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_change_replaces_the_copy_on_the_server() {
    let env = synced_env(state()).await;
    let id = env.draft("one").await;
    assert!(!env.on_server(id).await);

    let report = env.replay().await;
    assert_eq!((report.drafts_uploaded, report.drafts_failed), (1, 0));
    let drafts = env.server_drafts();
    assert_eq!(subjects(&drafts), ["one"]);
    assert!(
        drafts[0].1.contains(&"\\Draft".into()) && drafts[0].1.contains(&"\\Seen".into()),
        "{drafts:?}"
    );
    let raw = env.fake.state().mailbox_mut("Drafts").messages[0]
        .raw
        .clone();
    let parsed = MessageParser::new().parse(&raw).unwrap();
    assert_eq!(
        parsed.bcc().and_then(|b| b.first()?.address()),
        Some("dave@example.com"),
        "a draft keeps its Bcc"
    );
    assert!(env.on_server(id).await);

    // Nothing changed: nothing to do, no connection needed.
    let connections = env.fake.state().connections;
    assert_eq!(env.replay().await, ReplayReport::default());
    assert_eq!(env.fake.state().connections, connections);

    env.edit(id, content("two")).await.unwrap();
    assert!(!env.on_server(id).await);
    env.replay().await;
    assert_eq!(
        subjects(&env.server_drafts()),
        ["two"],
        "the old copy is gone"
    );

    // Synced, the copy is a message of the Drafts folder like any other, and the next
    // change deletes its row too.
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.messages("Drafts").await.len(), 1);
    env.client
        .attach_file(id, env.dir.path().join("catmail.db"))
        .await
        .unwrap();
    env.replay().await;
    assert!(env.messages("Drafts").await.is_empty());
    assert_eq!(subjects(&env.server_drafts()), ["two"]);
    assert_eq!(env.appends(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_or_sending_a_draft_deletes_its_copy() {
    let env = synced_env(state()).await;
    let deleted = env.draft("deleted").await;
    let sent = env.draft("sent").await;
    env.replay().await;
    assert_eq!(env.server_drafts().len(), 2);

    env.client.delete_draft(deleted).await.unwrap();
    env.replay().await;
    assert_eq!(subjects(&env.server_drafts()), ["sent"]);

    env.client.send_draft(sent).await.unwrap();
    // Not uploaded again while it is being sent.
    env.replay().await;
    assert_eq!(subjects(&env.server_drafts()), ["sent"]);
    env.client.flush_outbox(env.account).await.unwrap();
    env.replay().await;
    assert!(env.server_drafts().is_empty());
    assert_eq!(env.smtp.state().received.len(), 1);
    let sent_folder = env.fake.state().mailbox_mut("Sent").messages.len();
    assert_eq!(sent_folder, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_append_answer_leaves_one_copy() {
    for uidplus in [true, false] {
        let mut state = state();
        state.faults.uidplus = uidplus;
        let env = synced_env(state).await;
        let id = env.draft("one").await;
        env.replay().await;
        if !uidplus {
            // Found by its Message-ID instead.
            assert!(env.on_server(id).await);
        }

        // Stored, but the answer is lost with the connection. The replay starts over on
        // a new one (the old one came from the pool), and finds the copy there.
        env.edit(id, content("two")).await.unwrap();
        env.fake.state().faults.drop_after_append = true;
        let start = env.fake.state().log.len();
        let report = env.replay().await;
        assert_eq!(report.drafts_uploaded, 0, "nothing uploaded again");
        assert!(
            env.commands_since(start)
                .iter()
                .any(|c| c.starts_with("UID SEARCH HEADER Message-ID")),
            "uidplus: {uidplus}"
        );
        assert_eq!(
            subjects(&env.server_drafts()),
            ["two"],
            "uidplus: {uidplus}"
        );
        assert!(env.on_server(id).await);
        assert_eq!(env.appends(), 2);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_edited_before_its_lost_upload_is_settled_keeps_the_latest() {
    let env = synced_env(state()).await;
    let id = env.draft("one").await;
    env.replay().await;

    // "two" is stored, the answer lost, and the replay gives up: it can't log in again.
    env.edit(id, content("two")).await.unwrap();
    {
        let mut state = env.fake.state();
        state.faults.drop_after_append = true;
        state.faults.reject_login = true;
    }
    assert!(
        env.client
            .replay_pending(env.account)
            .await
            .unwrap()
            .error
            .is_some()
    );
    assert_eq!(subjects(&env.server_drafts()), ["one", "two"]);

    env.edit(id, content("three")).await.unwrap();
    env.fake.state().faults.reject_login = false;
    let report = env.replay().await;
    assert_eq!((report.drafts_uploaded, report.drafts_failed), (1, 0));
    assert_eq!(subjects(&env.server_drafts()), ["three"]);
    assert!(env.on_server(id).await);
    assert_eq!(env.appends(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_deleted_before_its_lost_upload_is_settled_leaves_nothing() {
    let mut state = state();
    state.faults.drop_after_append = true;
    let env = synced_env(state).await;
    let id = env.draft("one").await;
    assert!(
        env.client
            .replay_pending(env.account)
            .await
            .unwrap()
            .error
            .is_some()
    );
    env.client.delete_draft(id).await.unwrap();
    env.replay().await;
    assert!(env.server_drafts().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_uploads_are_reported_and_tried_again() {
    let mut state = state();
    state.faults.reject_appends = 1;
    let env = synced_env(state).await;
    let id = env.draft("one").await;

    let report = env.replay().await;
    assert_eq!((report.drafts_uploaded, report.drafts_failed), (0, 1));
    let draft = env.client.get_draft(id).await.unwrap().unwrap();
    assert!(
        draft.upload_error.as_deref().unwrap().contains("Quota"),
        "{draft:?}"
    );
    assert!(env.server_drafts().is_empty());

    let report = env.replay().await;
    assert_eq!((report.drafts_uploaded, report.drafts_failed), (1, 0));
    let draft = env.client.get_draft(id).await.unwrap().unwrap();
    assert!(draft.on_server && draft.upload_error.is_none());
}

/// A refused upload is counted although the connection is lost after it, unless the
/// replay runs again on a new connection (the one it reused was dead) and stores it.
#[tokio::test(flavor = "multi_thread")]
async fn refused_uploads_before_a_lost_connection_are_reported() {
    for retried in [true, false] {
        let env = synced_env(state()).await;
        let hello = env.id_of("INBOX", "hello").await;
        let flag = |flags| env.client.set_flags(vec![hello], flags, Default::default());
        // Leaves its connection in the pool.
        flag(catmail_core::Flags::SEEN).await.unwrap();
        env.replay().await;
        let id = env.draft("one").await;
        flag(catmail_core::Flags::FLAGGED).await.unwrap();
        {
            let mut state = env.fake.state();
            state.faults.reject_appends = 1;
            state.faults.drop_before_change = true;
            if !retried {
                // The pooled connection fails at once, so the new one is the last try.
                state.drop_open_connections();
            }
        }

        let report = env.replay().await;
        let counts = (report.drafts_uploaded, report.drafts_failed, report.applied);
        let expected = if retried { (1, 0, 1) } else { (0, 1, 0) };
        assert_eq!(counts, expected, "retried: {retried}");
        assert_eq!(
            report.error.map(|e| e.transient),
            (!retried).then_some(true)
        );
        assert_eq!(env.on_server(id).await, retried);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_drafts_folder_drafts_stay_local() {
    let env = synced_env(ServerState::new("alice", "pw")).await;
    let id = env.draft("one").await;
    let connections = env.fake.state().connections;
    assert_eq!(env.replay().await, ReplayReport::default());
    assert_eq!(env.fake.state().connections, connections);
    assert!(!env.on_server(id).await);

    // Unsynced, the account knows of no Drafts folder yet either.
    let env = env_unsynced().await;
    let id = env.draft("one").await;
    env.replay().await;
    assert!(!env.on_server(id).await);
    env.client.sync_account(env.account).await.unwrap();
    assert!(env.on_server(id).await, "the sync uploads it");
    assert_eq!(env.server_drafts().len(), 1);
}

async fn env_unsynced() -> Env {
    env(state()).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuilt_drafts_folder_keeps_one_copy() {
    let env = synced_env(state()).await;
    let id = env.draft("one").await;
    env.replay().await;
    env.fake.state().mailbox_mut("Drafts").reset_uidvalidity(99);

    // The sync finds the copy under its new UID rather than uploading another.
    env.client.sync_account(env.account).await.unwrap();
    assert!(!env.on_server(id).await);
    env.replay().await;
    assert!(env.on_server(id).await);
    assert_eq!(subjects(&env.server_drafts()), ["one"]);
    assert_eq!(env.appends(), 1);

    env.edit(id, content("two")).await.unwrap();
    env.replay().await;
    assert_eq!(subjects(&env.server_drafts()), ["two"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn background_sync_uploads_changes() {
    let env = env_built(
        state(),
        SmtpState::new("alice", "pw"),
        background(Duration::ZERO),
    )
    .await;
    let id = env.draft("one").await;
    eventually("the draft on the server", async || {
        subjects(&env.server_drafts()) == ["one"]
    })
    .await;
    env.edit(id, content("two")).await.unwrap();
    eventually("the new version alone", async || {
        subjects(&env.server_drafts()) == ["two"]
    })
    .await;
    env.client.delete_draft(id).await.unwrap();
    eventually("no copy", async || env.server_drafts().is_empty()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_refused_before_its_message_doesnt_stop_the_sync() {
    // As Dovecot does when it can't store anything: NO instead of asking for the message,
    // which leaves that connection unusable.
    let mut state = state();
    state.faults.reject_append_literal = true;
    let env = synced_env(state).await;
    let id = env.draft("one").await;
    env.fake.state().inbox().append_simple("new mail");

    env.client.sync_account(env.account).await.unwrap();
    assert!(
        env.messages("INBOX")
            .await
            .iter()
            .any(|(subject, _)| subject == "new mail")
    );
    let draft = env.client.get_draft(id).await.unwrap().unwrap();
    assert!(
        draft.upload_error.is_some() && !draft.on_server,
        "{draft:?}"
    );
    assert!(env.server_drafts().is_empty());

    // Changes to messages still reach the server, on a new connection.
    let hello = env.id_of("INBOX", "hello").await;
    env.client
        .set_flags(
            vec![hello],
            catmail_core::Flags::FLAGGED,
            Default::default(),
        )
        .await
        .unwrap();
    let report = env.replay().await;
    assert_eq!((report.applied, report.drafts_failed), (1, 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_append_answer_not_read_in_full_is_still_settled() {
    let mut state = state();
    state.faults.garble_append = true;
    let env = synced_env(state).await;
    let id = env.draft("one").await;
    let report = env.replay().await;
    assert_eq!((report.drafts_uploaded, report.drafts_failed), (1, 0));
    assert!(env.on_server(id).await);

    // So the next change replaces that copy rather than leaving it behind.
    env.edit(id, content("two")).await.unwrap();
    env.replay().await;
    assert_eq!(subjects(&env.server_drafts()), ["two"]);
    assert_eq!(env.appends(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_replaced_or_deleted_before_a_rebuild_is_synced_goes() {
    for delete in [false, true] {
        let env = synced_env(state()).await;
        let id = env.draft("one").await;
        env.replay().await;
        // Rebuilt on the server, and changed here before any sync saw that.
        env.fake.state().mailbox_mut("Drafts").reset_uidvalidity(99);
        if delete {
            env.client.delete_draft(id).await.unwrap();
        } else {
            env.edit(id, content("two")).await.unwrap();
        }
        env.replay().await;
        env.client.sync_account(env.account).await.unwrap();
        env.replay().await;
        let expected: &[&str] = if delete { &[] } else { &["two"] };
        assert_eq!(subjects(&env.server_drafts()), expected, "delete: {delete}");
        assert!(
            env.client
                .list_pending(Some(env.account))
                .await
                .unwrap()
                .is_empty(),
            "no conflicts left"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_quoting_a_nul_doesnt_stop_the_sync() {
    // A NUL can't go into an APPEND. Quoted from a message that has one (`=00`), it
    // would fail the draft's upload, and with it every sync of the account.
    let mut state = state();
    state.inbox().append(
        b"From: carol@example.com\r\nSubject: odd\r\nMessage-ID: <odd@example.com>\r\n\
          Content-Type: text/plain\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n\
          before=00after\r\n",
        &[],
    );
    let env = synced_env(state).await;
    let odd = env.id_of("INBOX", "odd").await;
    let draft = env.client.reply(odd, false).await.unwrap();
    assert!(draft.content.text.contains("beforeafter"), "{draft:?}");

    let report = env.replay().await;
    assert_eq!((report.drafts_uploaded, report.drafts_failed), (1, 0));
    assert!(env.on_server(draft.id).await);
    env.client.sync_account(env.account).await.unwrap();

    // Nor can it be typed in.
    let mut nul = content("nul\0subject");
    nul.text = "a\0b".into();
    let draft = env
        .client
        .create_draft(env.account, None, nul)
        .await
        .unwrap();
    assert_eq!(
        (draft.content.subject.as_str(), draft.content.text.as_str()),
        ("nulsubject", "ab")
    );
}

/// Text and HTML showing an inline image, and a PDF attached.
const REPORT: &[u8] = b"From: Zhang <zhang@example.com>\r\n\
    To: alice@example.com\r\n\
    Subject: Report\r\n\
    Date: Sat, 17 Oct 2026 09:30:00 +0800\r\n\
    MIME-Version: 1.0\r\n\
    Content-Type: multipart/mixed; boundary=\"outer\"\r\n\r\n\
    --outer\r\n\
    Content-Type: multipart/related; boundary=\"rel\"\r\n\r\n\
    --rel\r\n\
    Content-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n\
    --alt\r\n\
    Content-Type: text/plain\r\n\r\n\
    See the report.\r\n\
    --alt\r\n\
    Content-Type: text/html\r\n\r\n\
    <p>See the report.<img src=\"cid:logo@x\"></p>\r\n\
    --alt--\r\n\
    --rel\r\n\
    Content-Type: image/png\r\n\
    Content-ID: <logo@x>\r\n\
    Content-Transfer-Encoding: base64\r\n\r\n\
    iVBORw0KGgo=\r\n\
    --rel--\r\n\
    --outer\r\n\
    Content-Type: application/pdf; name=\"report.pdf\"\r\n\
    Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
    Content-Transfer-Encoding: base64\r\n\r\n\
    JVBERi0xLjQK\r\n\
    --outer--\r\n";

#[tokio::test(flavor = "multi_thread")]
async fn a_forward_carries_the_attachments_but_not_the_inline_images() {
    let mut state = state();
    state.inbox().append(REPORT, &[]);
    let env = synced_env(state).await;
    let id = env.id_of("INBOX", "Report").await;
    assert!(
        env.client
            .get_message(id)
            .await
            .unwrap()
            .unwrap()
            .body
            .is_none()
    );

    let draft = env.client.forward(id).await.unwrap();
    assert_eq!(draft.content.subject, "Fwd: Report");
    assert!(draft.content.recipients().next().is_none());
    assert!(draft.in_reply_to.is_none() && draft.references.is_empty());
    let text = &draft.content.text;
    assert!(
        text.starts_with(
            "\n\n---------- Forwarded message ---------\nFrom: Zhang <zhang@example.com>\nDate: 2026-"
        ),
        "{text:?}"
    );
    assert!(
        text.ends_with("Subject: Report\nTo: alice@example.com\n\nSee the report.\n"),
        "{text:?}"
    );
    let attachments: Vec<_> = draft
        .attachments
        .iter()
        .map(|a| (a.filename.as_str(), a.mime_type.as_str(), a.size))
        .collect();
    assert_eq!(attachments, [("report.pdf", "application/pdf", 9)]);
    assert_eq!(env.client.list_drafts(env.account).await.unwrap(), [draft]);

    // As it goes out: the copy on the server is built like the message sent.
    env.replay().await;
    let raw = env.fake.state().mailbox_mut("Drafts").messages[0]
        .raw
        .clone();
    let parsed = MessageParser::new().parse(&raw).unwrap();
    assert_eq!(parsed.attachment_count(), 1);
    let pdf = parsed.attachment(0).unwrap();
    assert_eq!(pdf.attachment_name(), Some("report.pdf"));
    assert_eq!(pdf.contents(), b"%PDF-1.4\n");
}

/// Over the whole-message limit, so its attachment is fetched on its own when needed.
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

#[tokio::test(flavor = "multi_thread")]
async fn a_forward_without_its_attachment_leaves_no_draft() {
    let mut state = state();
    state.inbox().append(&large_message(), &[]);
    let env = synced_env(state).await;
    let id = env.id_of("INBOX", "Big file").await;
    env.client.download_message(id).await.unwrap();

    // The body is here; the attachment, still on the server, can't be had.
    env.fake.state().faults.nil_sections = true;
    env.client.forward(id).await.unwrap_err();
    assert!(
        env.client
            .list_drafts(env.account)
            .await
            .unwrap()
            .is_empty()
    );

    env.fake.state().faults.nil_sections = false;
    let draft = env.client.forward(id).await.unwrap();
    assert_eq!(draft.attachments.len(), 1);
    assert_eq!(draft.attachments[0].size, 3 * 1024 * 1024 / 76 * 57);
}

/// Syncing in the background, a draft uploaded once it was left alone for `delay`.
fn background(delay: Duration) -> impl FnOnce(MailClientBuilder) -> MailClientBuilder {
    let schedule = SyncSchedule {
        full_sync: Duration::from_secs(3600),
        poll: Duration::from_millis(50),
        idle: Duration::from_secs(3600),
        retry: Duration::from_millis(50),
    };
    move |b| {
        b.sync_in_background(Some(schedule))
            .draft_upload_delay(delay)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn background_sync_waits_for_a_draft_to_settle() {
    let env = env_built(
        state(),
        SmtpState::new("alice", "pw"),
        background(Duration::from_secs(1)),
    )
    .await;
    env.fake.wait_until(|st| st.received("IDLE") >= 1).await;

    // Saved as it is typed: the first save wakes background sync, which leaves the
    // draft alone; later saves only put its upload off, without a sync each.
    let id = env.draft("o").await;
    env.fake.wait_until(|st| st.received("IDLE") >= 2).await;
    let idles = env.fake.state().received("IDLE");
    for subject in ["on", "one"] {
        tokio::time::sleep(Duration::from_millis(150)).await;
        env.edit(id, content(subject)).await.unwrap();
    }
    assert_eq!(env.fake.state().received("IDLE"), idles);
    assert_eq!(env.appends(), 0);
    // The server has the copy before the upload is recorded here: without UIDPLUS the
    // engine still searches for its UID first.
    eventually("the draft on the server", async || {
        subjects(&env.server_drafts()) == ["one"] && env.on_server(id).await
    })
    .await;
    assert_eq!(env.appends(), 1, "uploaded once, when it settled");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_that_settles_during_a_sync_is_uploaded_after_it() {
    let env = env_built(
        state(),
        SmtpState::new("alice", "pw"),
        background(Duration::from_secs(1)),
    )
    .await;
    env.fake.wait_until(|st| st.received("IDLE") >= 1).await;

    // The sync the draft wakes leaves it alone, and outlasts its delay: nothing else
    // would wake background sync before the next full sync, an hour away.
    env.fake.state().faults.slow = Some(("EXAMINE".into(), Duration::from_millis(1500)));
    let id = env.draft("one").await;
    eventually("the draft on the server", async || env.on_server(id).await).await;
    assert_eq!(subjects(&env.server_drafts()), ["one"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_asked_for_dont_wait_for_a_draft_to_settle() {
    let env = env_built(
        state(),
        SmtpState::new("alice", "pw"),
        background(Duration::from_secs(3600)),
    )
    .await;
    env.client.sync_account(env.account).await.unwrap();
    let id = env.draft("one").await;
    let report = env.replay().await;
    assert_eq!(report.drafts_uploaded, 1);
    assert!(env.on_server(id).await);

    env.edit(id, content("two")).await.unwrap();
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(subjects(&env.server_drafts()), ["two"]);
}
