//! Drafts, the outbox and saving to Sent, against the fake IMAP and SMTP servers.

mod common;

use std::sync::Arc;
use std::time::Duration;

use catmail_core::{
    Address, DraftContent, Error, ErrorKind, Event, IdentityId, OutboxEntry, OutboxStatus,
    RecvError, Verify,
};
use catmail_testkit::{FakeMailbox, ServerState, SmtpState};
use common::{Env, addr, drain, env, env_with, eventually, files, sparse, status_changes};
use mail_parser::{MessageParser, MimeHeaders};
use tokio::sync::Semaphore;

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
            .create_draft(self.account, None, content(subject), Vec::new())
            .await
            .unwrap();
        self.client.send_draft(draft.id).await.unwrap()
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

    let start = env.fake.state().log.len();
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
    let commands = env.commands_since(start);
    assert!(
        !commands
            .iter()
            .any(|c| c.starts_with("EXAMINE") || c.contains("SEARCH")),
        "a first APPEND can't have been made already: {commands:?}"
    );
    assert_eq!(
        env.fake.state().mailbox_mut("Sent").messages[0].flags,
        ["\\Seen"]
    );

    let seen = drain(&mut events);
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
    let edit = env.edit(draft, content("edited")).await;
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
    env.edit(drafts[0].id, content("second, edited"))
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
        .entries
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
async fn a_queued_message_is_cancelled_while_another_is_sent() {
    let gate = Arc::new(Semaphore::new(0));
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.end_of_data_gate = Some(gate.clone());
    let env = env_with(state(), smtp, false).await;
    env.queue("first").await;
    let second = env.queue("second").await;

    let flush = tokio::spawn({
        let (client, account) = (env.client.clone(), env.account);
        async move { client.flush_outbox(account).await }
    });
    eventually("the first to be out", async || {
        env.outbox().await[0].status == OutboxStatus::Unknown
    })
    .await;
    // Not after the pass: it would have sent the second by then.
    let cancelled =
        tokio::time::timeout(Duration::from_secs(5), env.client.cancel_outbox(second.id))
            .await
            .expect("doesn't wait for the pass")
            .unwrap();
    assert_eq!(cancelled.id, second.id);

    gate.add_permits(1);
    let results = flush.await.unwrap().unwrap().entries;
    assert_eq!(results.len(), 1);
    assert_eq!(env.delivered(), 1);
    let drafts = env.client.list_drafts(env.account).await.unwrap();
    assert_eq!(drafts.len(), 1, "the second is a draft again");
    assert_eq!(drafts[0].outbox, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pass_that_fails_still_announces_what_it_did() {
    let env = env_with(state(), SmtpState::new("alice", "pw"), false).await;
    env.queue("one").await;
    env.queue("two").await;
    // Starting on the second fails, as with a full disk.
    env.sql(
        "CREATE TRIGGER full_disk BEFORE UPDATE OF status ON outbox
         WHEN new.status = 'sending' AND old.subject = 'two'
         BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END;",
    );
    let mut events = env.client.subscribe();
    assert!(env.client.flush_outbox(env.account).await.is_err());
    assert_eq!(env.delivered(), 1);
    let changed = Event::OutboxChanged {
        account: env.account,
    };
    assert!(
        drain(&mut events).contains(&changed),
        "the first is gone from the outbox"
    );
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

    // The account's trouble, not the messages': neither counts an attempt.
    assert_eq!(
        env.flush().await,
        [(OutboxStatus::Queued, 0), (OutboxStatus::Queued, 0)]
    );
    let outbox = env.outbox().await;
    assert!(outbox[1].last_error.as_deref().unwrap().contains("421"));
    assert!(outbox[1].next_attempt_at > Some(outbox[1].created_at));
    let problem = env.client.account_status(env.account).send[0]
        .problem
        .clone();
    assert!(problem.error.message.contains("421"), "{problem:?}");
    assert_eq!(problem.error.kind, ErrorKind::Refused);
    assert!(env.client.account_status(env.account).sync.is_none());

    assert_eq!(
        env.flush().await,
        [(OutboxStatus::Done, 1), (OutboxStatus::Done, 1)]
    );
    assert_eq!(env.delivered(), 2);
    assert_eq!(env.smtp.state().connections, 2, "one connection per pass");
    assert!(env.client.account_status(env.account).send.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn postponed_entries_back_off_with_the_one_attempted() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.unavailable = 2;
    let env = env_with(state(), smtp, false).await;
    env.queue("one").await;
    env.queue("two").await;
    env.flush().await;
    env.flush().await;

    // Backing off as the account's failures add up, all of them alike.
    let outbox = env.outbox().await;
    let (first, second) = (&outbox[0], &outbox[1]);
    assert_eq!((first.attempts, second.attempts), (0, 0));
    let next = |e: &OutboxEntry| e.next_attempt_at.unwrap();
    assert!(next(first) - first.created_at >= 55, "{first:?}");
    assert_eq!(next(first), next(second));
    let problem = env.client.account_status(env.account).send[0]
        .problem
        .clone();
    assert_eq!(problem.failures, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_server_is_tried_again_soon_after_a_wake() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.unavailable = 2;
    let env = env_with(state(), smtp, false).await;
    env.queue("one").await;
    // The network not back yet: seconds, not half a minute.
    env.client.resume();
    let wait = async || {
        let entry = &env.outbox().await[0];
        entry.next_attempt_at.unwrap() - entry.created_at
    };
    env.flush().await;
    assert!(wait().await <= 3);
    env.flush().await;
    assert!(wait().await <= 5);
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);

    // Through since: failing again waits as long as ever.
    env.smtp.state().faults.unavailable = 1;
    env.queue("two").await;
    env.flush().await;
    assert!(wait().await >= 25);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_rejected_after_a_wake_is_not_tried_again_soon() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.reject_login = true;
    let env = env_with(state(), smtp, false).await;
    env.queue("one").await;
    // Servers lock accounts that fail too many logins.
    env.client.resume();
    env.flush().await;
    let entry = &env.outbox().await[0];
    assert!(entry.next_attempt_at.unwrap() - entry.created_at >= 25);
}

#[tokio::test(flavor = "multi_thread")]
async fn odd_reply_to_the_end_is_not_taken_as_a_refusal() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.end_of_data_replies = vec![354];
    let env = env_with(state(), smtp, false).await;
    env.queue("hi").await;

    // Neither 2xx nor a refusal: the server may have kept it.
    assert_eq!(env.flush().await, [(OutboxStatus::Unknown, 1)]);
    assert!(env.flush().await.is_empty(), "not resent on its own");
    assert_eq!(
        env.smtp
            .state()
            .log
            .iter()
            .filter(|c| c.line == "DATA")
            .count(),
        1
    );
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
    env.edit(draft, fixed).await.unwrap();
    env.client.send_draft(draft).await.unwrap();
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert_eq!(
        env.smtp.state().received[0].recipients,
        ["bob@example.com", "dave@example.com"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_flush_reports_why_the_server_was_unusable() {
    let env = env_with(state(), SmtpState::new("alice", "pw"), false).await;
    env.queue("one").await;
    let flush = async || env.client.flush_outbox(env.account).await.unwrap();

    // From this flush, not from whatever the account's status says by then.
    env.smtp.state().faults.reject_login = true;
    let rejected = flush().await;
    let error = rejected.errors.into_values().next().unwrap();
    assert_eq!((error.kind, error.transient), (ErrorKind::Auth, false));
    assert_eq!(rejected.entries[0].status, OutboxStatus::Queued);

    env.smtp.state().faults.reject_login = false;
    env.smtp.state().faults.unavailable = 1;
    let unavailable = flush().await;
    assert!(unavailable.errors.into_values().next().unwrap().transient);
    assert_eq!(unavailable.entries[0].status, OutboxStatus::Queued);

    let sent = flush().await;
    assert!(sent.errors.is_empty());
    assert_eq!(sent.entries[0].status, OutboxStatus::Done);
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_holds_the_queue_and_is_reported() {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.reject_login = true;
    let env = env_with(state(), smtp, false).await;
    let mut events = env.client.subscribe();
    env.queue("one").await;
    env.queue("two").await;
    // Not a message failed each time the queue is tried, but the account.
    for _ in 0..2 {
        assert_eq!(
            env.flush().await,
            [(OutboxStatus::Queued, 0), (OutboxStatus::Queued, 0)]
        );
    }
    let outbox = env.outbox().await;
    assert!(
        outbox[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("login rejected")
    );
    let problem = env.client.account_status(env.account).send[0]
        .problem
        .clone();
    assert_eq!(problem.error.kind, ErrorKind::Auth);
    assert!(
        problem.error.message.contains("login rejected"),
        "{problem:?}"
    );
    assert_eq!(problem.failures, 2);
    let changed = status_changes(&mut events, env.account);
    assert_eq!(changed, 1, "announced once, not for each failure");

    env.smtp.state().faults.reject_login = false;
    assert_eq!(
        env.flush().await,
        [(OutboxStatus::Done, 1), (OutboxStatus::Done, 1)]
    );
    assert!(env.client.account_status(env.account).send.is_empty());
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
    let hello = env.client.list_messages(inbox.id, 1, None).await.unwrap()[0].id;
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
    let search = format!("UID SEARCH HEADER Message-ID \"<{}>\"", entry.message_id);
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

/// The user says the identity's server saves what it sends to Sent itself: no copy is
/// saved, until they take it back.
#[tokio::test(flavor = "multi_thread")]
async fn no_copy_is_saved_where_the_server_saves_one() {
    let env = env(state()).await;
    let own = env.own_identity().await;
    assert_eq!(own.saves_sent, None);
    let mut events = env.client.subscribe();
    env.client
        .set_identity_saves_sent(own.id, Some(true))
        .await
        .unwrap();
    assert!(drain(&mut events).contains(&Event::AccountChanged {
        account: env.account
    }));
    env.queue("hi").await;
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert_eq!(env.delivered(), 1);
    assert!(env.sent_folder().is_empty());

    env.client
        .set_identity_saves_sent(own.id, None)
        .await
        .unwrap();
    env.queue("again").await;
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert_eq!(env.sent_folder().len(), 1);
    let gone = env
        .client
        .set_identity_saves_sent(IdentityId(99), Some(true));
    assert!(matches!(gone.await, Err(Error::IdentityNotFound(_))));
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
    let original = env.id_of("INBOX", "季度报告").await;

    let draft = env
        .client
        .reply(original, true, None, String::new(), Vec::new())
        .await
        .unwrap();
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
        .create_draft(env.account, None, content("with attachment"), Vec::new())
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
async fn attachments_are_limited_in_size() {
    const LIMIT: u64 = 25 * 1024 * 1024;
    let env = env(state()).await;
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str, size: u64| sparse(dir.path(), name, size);
    let draft = env
        .client
        .create_draft(env.account, None, content("with attachments"), Vec::new())
        .await
        .unwrap();
    let blobs = env.client.config().blob_dir();
    let stored = || {
        files(&blobs)
            .iter()
            .map(|p| p.metadata().unwrap().len())
            .sum::<u64>()
    };

    // Refused before it is copied.
    let before = stored();
    assert!(matches!(
        env.client
            .attach_file(draft.id, file("huge.iso", 4 * LIMIT))
            .await,
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(stored(), before);

    // The limit is on the draft's attachments together.
    let most = env
        .client
        .attach_file(draft.id, file("most.bin", LIMIT - 4))
        .await
        .unwrap();
    assert!(matches!(
        env.client.attach_file(draft.id, file("five.txt", 5)).await,
        Err(Error::InvalidInput(_))
    ));
    env.client
        .attach_file(draft.id, file("four.txt", 4))
        .await
        .unwrap();
    let attached = env.client.get_draft(draft.id).await.unwrap().unwrap();
    let names: Vec<_> = attached
        .attachments
        .iter()
        .map(|a| a.filename.as_str())
        .collect();
    assert_eq!(names, ["most.bin", "four.txt"]);

    // Removing one makes room again.
    env.client.remove_draft_attachment(most.id).await.unwrap();
    env.client
        .attach_file(draft.id, file("five.txt", 5))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn files_given_with_a_new_draft_come_with_it_or_no_draft_is_made() {
    const LIMIT: u64 = 25 * 1024 * 1024;
    let env = env(state()).await;
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str, size: u64| sparse(dir.path(), name, size);
    let create = |files| {
        env.client
            .create_draft(env.account, None, content("with files"), files)
    };

    // The limit is on the files together; the error names the one that doesn't fit.
    let err = create(vec![file("most.bin", LIMIT - 4), file("five.txt", 5)])
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Attach { path, error }
            if path.ends_with("five.txt") && matches!(**error, Error::InvalidInput(_))),
        "{err:?}"
    );
    assert_eq!(err.kind(), ErrorKind::Invalid);
    let missing = dir.path().join("missing.pdf");
    let err = create(vec![missing.clone()]).await.unwrap_err();
    assert!(
        matches!(&err, Error::Attach { path, .. } if *path == missing),
        "{err:?}"
    );
    assert!(err.to_string().contains("missing.pdf"), "{err}");
    assert!(
        env.client
            .list_drafts(env.account)
            .await
            .unwrap()
            .is_empty()
    );

    let draft = create(vec![file("most.bin", LIMIT - 4), file("four.txt", 4)])
        .await
        .unwrap();
    let names: Vec<_> = draft
        .attachments
        .iter()
        .map(|a| (a.filename.as_str(), a.size))
        .collect();
    assert_eq!(names, [("most.bin", LIMIT - 4), ("four.txt", 4)]);
    assert_eq!(env.client.list_drafts(env.account).await.unwrap(), [draft]);
}

/// What attaching `size` bytes to `draft` does: `Some` with the attachment, `None` if
/// it is refused as too large.
async fn attach(
    env: &Env,
    dir: &std::path::Path,
    draft: catmail_core::DraftId,
    size: u64,
) -> Option<catmail_core::DraftAttachment> {
    match env
        .client
        .attach_file(draft, sparse(dir, &format!("{size}.bin"), size))
        .await
    {
        Ok(attachment) => Some(attachment),
        Err(Error::InvalidInput(e)) if e.contains("add up to") => None,
        Err(e) => panic!("{e:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_follow_the_size_the_server_announces() {
    const MIB: u64 = 1024 * 1024;
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.size_limit = Some(MIB);
    let env = env_with(state(), smtp, false).await;
    let dir = tempfile::tempdir().unwrap();
    let draft = env
        .client
        .create_draft(env.account, None, content("big"), Vec::new())
        .await
        .unwrap()
        .id;
    // Not connected to yet: the usual limit.
    let early = attach(&env, dir.path(), draft, 2 * MIB).await.unwrap();

    // Sending tells: it is far less here.
    env.queue("first").await;
    env.flush().await;
    assert!(attach(&env, dir.path(), draft, 1).await.is_none());
    env.client.remove_draft_attachment(early.id).await.unwrap();
    assert!(
        attach(&env, dir.path(), draft, MIB).await.is_none(),
        "encoded, it wouldn't fit"
    );
    let fits = attach(&env, dir.path(), draft, MIB / 2).await.unwrap();

    // The server takes more now, past the usual limit.
    env.smtp.state().faults.size_limit = Some(60 * MIB);
    env.queue("second").await;
    env.flush().await;
    env.client.remove_draft_attachment(fits.id).await.unwrap();
    attach(&env, dir.path(), draft, 30 * MIB).await.unwrap();

    // And says nothing any more: the usual limit again.
    env.smtp.state().faults.size_limit = None;
    env.queue("third").await;
    env.flush().await;
    assert!(attach(&env, dir.path(), draft, 1).await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_fit_the_append_limit_learned_by_syncing() {
    const MIB: u64 = 1024 * 1024;
    let mut state = state();
    state.faults.append_limit = Some(MIB);
    let env = env(state).await;
    let dir = tempfile::tempdir().unwrap();
    env.client.sync_account(env.account).await.unwrap();
    let draft = env
        .client
        .create_draft(env.account, None, content("big"), Vec::new())
        .await
        .unwrap()
        .id;
    assert!(attach(&env, dir.path(), draft, MIB).await.is_none());
    attach(&env, dir.path(), draft, MIB / 2).await.unwrap();

    // No APPEND at all is no size: no copy is kept for attachments to fit.
    env.fake.state().faults.append_limit = Some(0);
    env.client.sync_account(env.account).await.unwrap();
    attach(&env, dir.path(), draft, MIB).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_added_checked_knows_its_limits_before_the_first_send() {
    const MIB: u64 = 1024 * 1024;
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.size_limit = Some(MIB);
    let env = env_with(state(), smtp, false).await;
    let dir = tempfile::tempdir().unwrap();
    let mut new = {
        let account = env.client.get_account(env.account).await.unwrap().unwrap();
        catmail_core::NewAccount {
            email: "alice@example.org".into(),
            display_name: None,
            username: account.username,
            imap: account.imap,
            smtp: env.smtp_config(),
        }
    };
    let checked = env
        .client
        .add_account(new.clone(), "pw".into(), Verify::Login)
        .await
        .unwrap();
    let draft = env
        .client
        .create_draft(checked.id, None, content("big"), Vec::new())
        .await;
    assert!(
        attach(&env, dir.path(), draft.unwrap().id, MIB)
            .await
            .is_none()
    );

    // Added without the check, nothing is known yet.
    new.email = "alice@example.net".into();
    let unchecked = env
        .client
        .add_account(new, "pw".into(), Verify::Skip)
        .await
        .unwrap();
    let draft = env
        .client
        .create_draft(unchecked.id, None, content("big"), Vec::new())
        .await;
    attach(&env, dir.path(), draft.unwrap().id, MIB)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn drafts_are_checked_before_sending() {
    let env = env(state()).await;
    let mut bad = content("x");
    bad.to.push(addr("not an address"));
    assert!(matches!(
        env.client
            .create_draft(env.account, None, bad, Vec::new())
            .await,
        Err(Error::InvalidInput(_))
    ));

    let empty = env
        .client
        .create_draft(env.account, None, DraftContent::default(), Vec::new())
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

/// Sending in the background, a message queued and postponed after the server was busy,
/// and the events from before it was queued.
async fn postponed_retry() -> (Env, tokio::sync::broadcast::Receiver<Event>, OutboxEntry) {
    let mut smtp = SmtpState::new("alice", "pw");
    smtp.faults.busy = 1;
    let env = env_with(state(), smtp, true).await;
    let events = env.client.subscribe();
    let busy = env.queue("busy").await;
    eventually("the first attempt", async || {
        let entry = &env.outbox().await[0];
        (entry.status, entry.attempts) == (OutboxStatus::Queued, 1)
    })
    .await;
    (env, events, busy)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clock_set_back_doesnt_hold_up_a_retry() {
    let (env, mut events, busy) = postponed_retry().await;
    // Postponed before the clock was set back a day: due in what is now the future.
    env.sql("UPDATE outbox SET next_attempt_at = next_attempt_at + 86400");

    // Wakes the worker before the retry's half a minute is up.
    env.queue("later").await;
    wait_for_sent(&mut events, busy.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_due_after_a_sleep_is_made_on_resuming() {
    let (env, mut events, busy) = postponed_retry().await;
    // The computer slept past the retry; the worker's timer didn't count the sleep.
    env.sql("UPDATE outbox SET next_attempt_at = 0");

    env.client.resume();
    wait_for_sent(&mut events, busy.id).await;
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

#[tokio::test(flavor = "multi_thread")]
async fn the_overview_counts_drafts_and_lists_the_outbox() {
    let env = env(state()).await;
    env.client.sync_account(env.account).await.unwrap();
    env.client
        .create_draft(env.account, None, content("Kept"), Vec::new())
        .await
        .unwrap();
    let queued = env.queue("Queued").await;

    let overview = env.client.overview().await.unwrap();
    assert_eq!(overview.len(), 1);
    let account = &overview[0];
    assert_eq!(account.account.id, env.account);
    let folders: Vec<_> = account.folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(folders, ["INBOX", "Sent"]);
    // The one handed to "send" is in the outbox, not among the drafts.
    assert_eq!(account.drafts, 1);
    assert_eq!(account.outbox, [queued]);
}
