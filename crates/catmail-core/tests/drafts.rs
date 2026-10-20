//! Drafts copied to the server's Drafts folder, against the fake IMAP server: uploads
//! replacing the copy, deletion with the draft or once it is sent, lost and refused
//! APPENDs, a rebuilt folder, and uploads made in the background.

mod common;

use std::time::Duration;

use catmail_core::{Address, DraftContent, DraftId, ReplayReport, SyncSchedule};
use catmail_testkit::{FakeMailbox, ServerState, SmtpState};
use common::{Env, env, env_built, eventually, synced_env};
use mail_parser::MessageParser;

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
            .create_draft(self.account, content(subject))
            .await
            .unwrap()
            .id
    }

    async fn replay(&self) -> ReplayReport {
        self.client.replay_pending(self.account).await.unwrap()
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

    env.client.update_draft(id, content("two")).await.unwrap();
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
        env.client.update_draft(id, content("two")).await.unwrap();
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
async fn a_draft_deleted_before_its_lost_upload_is_settled_leaves_nothing() {
    let mut state = state();
    state.faults.drop_after_append = true;
    let env = synced_env(state).await;
    let id = env.draft("one").await;
    assert!(env.client.replay_pending(env.account).await.is_err());
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

    env.client.update_draft(id, content("two")).await.unwrap();
    env.replay().await;
    assert_eq!(subjects(&env.server_drafts()), ["two"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn background_sync_uploads_changes_at_once() {
    let schedule = SyncSchedule {
        full_sync: Duration::from_secs(3600),
        poll: Duration::from_millis(50),
        idle: Duration::from_secs(3600),
        retry: Duration::from_millis(50),
    };
    let env = env_built(state(), SmtpState::new("alice", "pw"), |b| {
        b.sync_in_background(Some(schedule))
    })
    .await;
    let id = env.draft("one").await;
    eventually("the draft on the server", async || {
        subjects(&env.server_drafts()) == ["one"]
    })
    .await;
    env.client.update_draft(id, content("two")).await.unwrap();
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
    env.client.update_draft(id, content("two")).await.unwrap();
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
            env.client.update_draft(id, content("two")).await.unwrap();
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
