//! Drafts, the outbox and saving to Sent, against the fake IMAP and SMTP servers.

mod common;

use std::sync::Arc;
use std::time::Duration;

use catmail_core::{Address, DraftContent, Error, Event, OutboxEntry, OutboxStatus, RecvError};
use catmail_testkit::{FakeMailbox, ServerState, SmtpState};
use common::{Env, env, env_with};
use mail_parser::{MessageParser, MimeHeaders};
use tokio::sync::Semaphore;

fn addr(email: &str) -> Address {
    Address {
        name: None,
        email: email.into(),
    }
}

fn content(subject: &str) -> DraftContent {
    DraftContent {
        subject: subject.into(),
        to: vec![Address {
            name: Some("Bob".into()),
            email: "bob@example.com".into(),
        }],
        cc: vec![addr("carol@example.com")],
        bcc: vec![addr("dave@example.com"), addr("BOB@example.com")],
        text: "你好，\n\n.see you\n".into(),
    }
}

/// An INBOX and a Sent folder.
fn state() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    state.inbox().append_simple("hello");
    state
        .mailboxes
        .push(FakeMailbox::new("Sent").with_attribute("\\Sent"));
    state
}

impl Env {
    async fn queue(&self, subject: &str) -> OutboxEntry {
        let draft = self
            .client
            .create_draft(self.account, content(subject))
            .await
            .unwrap();
        self.client.send_draft(draft.id).await.unwrap()
    }

    async fn flush(&self) -> Vec<(OutboxStatus, u32)> {
        self.client
            .flush_outbox(self.account)
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.status, e.attempts))
            .collect()
    }

    async fn outbox(&self) -> Vec<OutboxEntry> {
        self.client.list_outbox(Some(self.account)).await.unwrap()
    }

    fn delivered(&self) -> usize {
        self.smtp.state().received.len()
    }

    fn sent_folder(&self) -> Vec<Vec<u8>> {
        let mut state = self.fake.state();
        state
            .mailbox_mut("Sent")
            .messages
            .iter()
            .map(|m| m.raw.clone())
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_and_saves_to_sent() {
    let env = env(state()).await;
    env.client.sync_account(env.account).await.unwrap();
    let mut events = env.client.subscribe();

    let entry = env.queue("季度报告").await;
    assert_eq!(entry.status, OutboxStatus::Queued);
    assert_eq!(
        entry.recipients,
        ["bob@example.com", "carol@example.com", "dave@example.com"],
        "every recipient once, Bcc included"
    );
    assert!(entry.message_id.ends_with("@example.com"));
    assert!(
        env.client
            .list_drafts(env.account)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        env.delivered(),
        0,
        "nothing is sent until the outbox is flushed"
    );

    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert!(env.outbox().await.is_empty());
    let draft = entry.draft_id.unwrap();
    assert!(
        env.client.get_draft(draft).await.unwrap().is_none(),
        "deleted once sent"
    );

    let received = env.smtp.state().received[0].clone();
    assert_eq!(received.from, "alice@example.com");
    assert_eq!(received.recipients, entry.recipients);
    let m = MessageParser::new().parse(&received.data).unwrap();
    assert_eq!(m.subject(), Some("季度报告"));
    assert_eq!(m.message_id(), Some(entry.message_id.as_str()));
    assert!(m.bcc().is_none(), "Bcc stays out of the headers");
    assert_eq!(
        m.body_text(0).unwrap().replace("\r\n", "\n"),
        "你好，\n\n.see you\n"
    );

    let sent = env.sent_folder();
    assert_eq!(sent, [received.data], "the same bytes, saved once");
    assert_eq!(
        env.fake.state().mailbox_mut("Sent").messages[0].flags,
        ["\\Seen"]
    );

    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    for expected in [
        Event::DraftsChanged {
            account: env.account,
        },
        Event::OutboxChanged {
            account: env.account,
        },
        Event::MessageSent {
            account: env.account,
            outbox: entry.id,
        },
    ] {
        assert!(seen.contains(&expected), "{expected:?} not in {seen:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn lost_confirmation_is_never_resent_on_its_own() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.drop_after_data = 1;
    let env = env_with(state(), smtp, false).await;
    env.client.sync_account(env.account).await.unwrap();
    let entry = env.queue("once").await;

    assert_eq!(env.flush().await, [(OutboxStatus::Unknown, 1)]);
    assert_eq!(env.delivered(), 1, "the server did get it");
    let unknown = &env.outbox().await[0];
    assert!(unknown.last_error.is_some());

    // Flushing again doesn't touch it, and neither does a restart (see the client
    // tests): only the user decides.
    assert!(env.flush().await.is_empty());
    assert_eq!(env.delivered(), 1);
    let draft = entry.draft_id.unwrap();
    let edit = env.client.update_draft(draft, content("edited")).await;
    assert!(matches!(edit, Err(Error::Conflict(_))), "{edit:?}");

    // The user found it arrived: only the copy in Sent is still to do.
    let marked = env.client.mark_outbox_sent(entry.id).await.unwrap();
    assert_eq!(marked.status, OutboxStatus::Accepted);
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert_eq!(env.delivered(), 1, "not sent again");
    assert_eq!(env.sent_folder().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_outcome_can_be_resent_or_cancelled() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.drop_after_data = 2;
    let env = env_with(state(), smtp, false).await;
    let first = env.queue("first").await;
    let second = env.queue("second").await;
    assert_eq!(
        env.flush().await,
        [(OutboxStatus::Unknown, 1), (OutboxStatus::Unknown, 1)]
    );

    // Resending risks a duplicate, which the user accepts by asking.
    env.client.retry_outbox(first.id).await.unwrap();
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert_eq!(env.delivered(), 3);

    // Cancelling hands the draft back for editing.
    env.client.cancel_outbox(second.id).await.unwrap();
    let drafts = env.client.list_drafts(env.account).await.unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].id, second.draft_id.unwrap());
    assert_eq!(drafts[0].outbox, None);
    env.client
        .update_draft(drafts[0].id, content("second, edited"))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn decisions_wait_for_the_send_in_progress() {
    let gate = Arc::new(Semaphore::new(0));
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.end_of_data_gate = Some(gate.clone());
    let env = env_with(state(), smtp, false).await;
    env.client.sync_account(env.account).await.unwrap();
    let entry = env.queue("hi").await;

    let flush = tokio::spawn({
        let (client, account) = (env.client.clone(), env.account);
        async move { client.flush_outbox(account).await }
    });
    // The whole message is out; the server's verdict is held back.
    eventually("the end of the message", async || {
        env.outbox().await[0].status == OutboxStatus::Unknown
    })
    .await;

    // Resending now could deliver twice, and would leave the attempt unable to record
    // the acceptance that is about to arrive.
    let retry = tokio::spawn({
        let client = env.client.clone();
        async move { client.retry_outbox(entry.id).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!retry.is_finished(), "waits for the attempt to end");

    gate.add_permits(1);
    let results: Vec<_> = flush
        .await
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|e| e.status)
        .collect();
    assert_eq!(results, [OutboxStatus::Done]);
    let retried = retry.await.unwrap();
    assert!(
        matches!(retried, Err(Error::OutboxNotFound(_))),
        "{retried:?}"
    );
    assert_eq!(env.delivered(), 1);
    assert_eq!(env.sent_folder().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn temporary_failures_are_retried_with_backoff() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.busy = 1;
    let env = env_with(state(), smtp, false).await;
    env.queue("busy").await;

    assert_eq!(env.flush().await, [(OutboxStatus::Queued, 1)]);
    let entry = &env.outbox().await[0];
    assert!(entry.last_error.as_deref().unwrap().contains("451"));
    let wait = entry.next_attempt_at.unwrap() - entry.created_at;
    assert!(
        (25..=60).contains(&wait),
        "first backoff is about 30s, got {wait}"
    );

    // A manual flush doesn't wait for the backoff. (Attempts count the last step: the
    // one save to Sent.)
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert_eq!(env.delivered(), 1);
    let mails = env
        .smtp
        .state()
        .log
        .iter()
        .filter(|c| c.line.starts_with("MAIL"))
        .count();
    assert_eq!(mails, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_server_postpones_the_rest_of_the_queue() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.unavailable = 1;
    let env = env_with(state(), smtp, false).await;
    env.queue("one").await;
    env.queue("two").await;

    // The second would fail the same way: it isn't attempted, nor counted.
    assert_eq!(env.flush().await, [(OutboxStatus::Queued, 1)]);
    let outbox = env.outbox().await;
    assert_eq!(
        outbox
            .iter()
            .map(|e| (e.status, e.attempts))
            .collect::<Vec<_>>(),
        [(OutboxStatus::Queued, 1), (OutboxStatus::Queued, 0)]
    );
    assert!(outbox[1].last_error.as_deref().unwrap().contains("421"));
    assert!(outbox[1].next_attempt_at > Some(outbox[1].created_at));

    assert_eq!(
        env.flush().await,
        [(OutboxStatus::Done, 1), (OutboxStatus::Done, 1)]
    );
    assert_eq!(env.delivered(), 2);
    assert_eq!(env.smtp.state().connections, 2, "one connection per pass");
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_lost_before_the_end_is_retried() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.drop_on_mail = 1;
    let env = env_with(state(), smtp, false).await;
    env.queue("one").await;
    env.queue("two").await;

    // The server never saw the end of the first message, so it is safe to send again;
    // the second goes out on a new connection.
    assert_eq!(
        env.flush().await,
        [(OutboxStatus::Queued, 1), (OutboxStatus::Done, 1)]
    );
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert_eq!(env.delivered(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_messages_fail_and_can_be_fixed() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.reject_recipients = vec![("carol@example.com".into(), 550)];
    let env = env_with(state(), smtp, false).await;
    let entry = env.queue("typo").await;

    assert_eq!(env.flush().await, [(OutboxStatus::Failed, 1)]);
    let failed = &env.outbox().await[0];
    assert!(
        failed
            .last_error
            .as_deref()
            .unwrap()
            .contains("carol@example.com")
    );
    assert_eq!(env.delivered(), 0, "nobody gets it if someone can't");
    assert!(env.flush().await.is_empty(), "not retried on its own");

    env.client.cancel_outbox(entry.id).await.unwrap();
    let draft = entry.draft_id.unwrap();
    let mut fixed = content("typo");
    fixed.cc.clear();
    env.client.update_draft(draft, fixed).await.unwrap();
    env.client.send_draft(draft).await.unwrap();
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert_eq!(
        env.smtp.state().received[0].recipients,
        ["bob@example.com", "dave@example.com"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_fails_the_send() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.reject_login = true;
    let env = env_with(state(), smtp, false).await;
    env.queue("hi").await;
    assert_eq!(env.flush().await, [(OutboxStatus::Failed, 1)]);
    assert!(
        env.outbox().await[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("login rejected")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn saving_to_sent_is_retried_without_resending() {
    let mut state = state();
    state.faults.reject_appends = 1;
    let env = env(state).await;
    env.client.sync_account(env.account).await.unwrap();
    env.queue("hi").await;

    assert_eq!(env.flush().await, [(OutboxStatus::Accepted, 1)]);
    let accepted = &env.outbox().await[0];
    assert!(accepted.last_error.as_deref().unwrap().contains("Quota"));
    assert!(accepted.next_attempt_at.is_some());
    assert_eq!(env.delivered(), 1);
    assert!(env.sent_folder().is_empty());

    assert_eq!(env.flush().await, [(OutboxStatus::Done, 2)]);
    assert_eq!(env.delivered(), 1, "the send is never repeated");
    assert_eq!(env.sent_folder().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn append_refused_before_the_message_does_not_stall_the_connection() {
    let mut state = state();
    state.faults.reject_append_literal = true;
    let env = env(state).await;
    env.client.sync_account(env.account).await.unwrap();
    // Leaves a connection in the pool for the save to reuse.
    let inbox = env
        .client
        .find_folder(env.account, "INBOX")
        .await
        .unwrap()
        .unwrap();
    let hello = env.client.list_messages(inbox.id, 1, 0).await.unwrap()[0].id;
    env.client.download_message(hello).await.unwrap();
    env.queue("hi").await;

    // Like Dovecot when it can't write: NO instead of asking for the message. The
    // connection is left mid-command, so it must not go back to the pool.
    assert_eq!(env.flush().await, [(OutboxStatus::Accepted, 1)]);
    env.fake.state().faults.reject_append_literal = false;
    let done = tokio::time::timeout(Duration::from_secs(10), env.flush())
        .await
        .expect("no stalled connection reused");
    assert_eq!(done, [(OutboxStatus::Done, 2)]);
    assert_eq!(env.sent_folder().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn unanswered_append_is_not_saved_twice() {
    let mut state = state();
    state.faults.drop_after_append = true;
    let env = env(state).await;
    env.client.sync_account(env.account).await.unwrap();
    let entry = env.queue("hi").await;

    // Saved, but the answer was lost.
    assert_eq!(env.flush().await, [(OutboxStatus::Accepted, 1)]);
    assert_eq!(env.sent_folder().len(), 1);

    // The next attempt finds it there and doesn't APPEND again.
    let start = env.fake.state().log.len();
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 2)]);
    assert_eq!(env.sent_folder().len(), 1, "one copy");
    let commands = env.commands_since(start);
    let search = format!("UID SEARCH HEADER Message-ID <{}>", entry.message_id);
    assert!(commands.contains(&search), "{commands:?}");
    assert!(
        !commands.iter().any(|c| c.starts_with("APPEND")),
        "{commands:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sent_folder_is_found_without_a_sync_and_optional() {
    // Never synced: the Sent folder comes from LIST.
    let env = env(state()).await;
    env.queue("hi").await;
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert_eq!(env.sent_folder().len(), 1);

    // No Sent folder at all: sending is all there is to do.
    let env = common::env(ServerState::new("alice", "pw")).await;
    env.queue("hi").await;
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert!(
        !env.fake
            .state()
            .log
            .iter()
            .any(|c| c.line.starts_with("APPEND"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn replies_are_threaded_and_quoted() {
    let mut state = state();
    let raw = b"From: Zhang San <zhang@example.com>\r\n\
        To: alice@example.com, li@example.com\r\n\
        Subject: =?UTF-8?B?5a2j5bqm5oql5ZGK?=\r\n\
        Message-ID: <q3@example.com>\r\n\
        References: <root@example.com>\r\n\
        Date: Sat, 17 Oct 2026 09:30:00 +0800\r\n\r\n\
        Numbers attached.\r\n";
    state.inbox().append(raw, &[]);
    let env = env(state).await;
    env.client.sync_account(env.account).await.unwrap();
    let inbox = env
        .client
        .find_folder(env.account, "INBOX")
        .await
        .unwrap()
        .unwrap();
    let original = env.client.list_messages(inbox.id, 1, 0).await.unwrap()[0].id;

    let draft = env.client.reply(original, true).await.unwrap();
    assert_eq!(draft.content.subject, "Re: 季度报告");
    let to: Vec<_> = draft.content.to.iter().map(|a| a.email.as_str()).collect();
    let cc: Vec<_> = draft.content.cc.iter().map(|a| a.email.as_str()).collect();
    assert_eq!(
        (to, cc),
        (vec!["zhang@example.com"], vec!["li@example.com"])
    );
    assert!(
        draft
            .content
            .text
            .contains("Zhang San <zhang@example.com> wrote:\n> Numbers attached.")
    );
    assert_eq!(draft.reply_to, Some(original));

    env.client.send_draft(draft.id).await.unwrap();
    env.flush().await;
    let received = env.smtp.state().received[0].data.clone();
    let m = MessageParser::new().parse(&received).unwrap();
    assert_eq!(m.in_reply_to().as_text(), Some("q3@example.com"));
    assert_eq!(
        m.references().as_text_list().unwrap().to_vec(),
        ["root@example.com", "q3@example.com"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_are_copied_when_added() {
    let env = env(state()).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("报告.pdf");
    std::fs::write(&path, b"%PDF-1.4 first").unwrap();

    let draft = env
        .client
        .create_draft(env.account, content("with attachment"))
        .await
        .unwrap();
    let pdf = env.client.attach_file(draft.id, &path).await.unwrap();
    assert_eq!(
        (pdf.filename.as_str(), pdf.mime_type.as_str(), pdf.size),
        ("报告.pdf", "application/pdf", 14)
    );
    let extra = env.client.attach_file(draft.id, &path).await.unwrap();
    env.client.remove_draft_attachment(extra.id).await.unwrap();
    std::fs::write(&path, b"changed afterwards").unwrap();
    assert!(matches!(
        env.client.attach_file(draft.id, dir.path()).await,
        Err(Error::InvalidInput(_))
    ));

    env.client.send_draft(draft.id).await.unwrap();
    env.flush().await;
    let received = env.smtp.state().received[0].data.clone();
    let m = MessageParser::new().parse(&received).unwrap();
    assert_eq!(m.attachment_count(), 1);
    let attachment = m.attachment(0).unwrap();
    assert_eq!(attachment.attachment_name(), Some("报告.pdf"));
    assert_eq!(attachment.contents(), b"%PDF-1.4 first");
}

#[tokio::test(flavor = "multi_thread")]
async fn drafts_are_checked_before_sending() {
    let env = env(state()).await;
    let mut bad = content("x");
    bad.to.push(addr("not an address"));
    assert!(matches!(
        env.client.create_draft(env.account, bad).await,
        Err(Error::InvalidInput(_))
    ));

    let empty = env
        .client
        .create_draft(env.account, DraftContent::default())
        .await
        .unwrap();
    assert!(matches!(
        env.client.send_draft(empty.id).await,
        Err(Error::InvalidInput(_))
    ));

    let entry = env.queue("x").await;
    let draft = entry.draft_id.unwrap();
    assert!(matches!(
        env.client.send_draft(draft).await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        env.client.delete_draft(draft).await,
        Err(Error::Conflict(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn background_sending_needs_no_flush() {
    let env = env_with(state(), SmtpState::new("alice", "pw"), true).await;
    let mut events = env.client.subscribe();
    let entry = env.queue("background").await;
    wait_for_sent(&mut events, entry.id).await;
    assert_eq!(env.delivered(), 1);
    // Saved right after.
    eventually("saving to Sent", async || env.outbox().await.is_empty()).await;
    assert_eq!(env.sent_folder().len(), 1);
}

/// Polls `ready` until it holds; fails the test after 10 seconds.
async fn eventually(what: &str, mut ready: impl AsyncFnMut() -> bool) {
    let poll = async {
        while !ready().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(10), poll)
        .await
        .is_err()
    {
        panic!("timed out waiting for {what}");
    }
}

async fn wait_for_sent(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    id: catmail_core::OutboxId,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(Event::MessageSent { outbox, .. }) if outbox == id => return,
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("engine gone"),
            }
        }
    })
    .await
    .expect("sent in the background");
}
