//! Local changes (flags, moves, deletions) and their replay against the fake IMAP server.

use catmail_core::{
    DraftContent, Error, Event, Flags, MessageId, MessageSummary, OpKind, OpStatus,
};
use catmail_testkit::{FakeMailbox, ServerState};

mod common;

use common::{Env, synced_env};

/// INBOX with "one", "two", "three" (UIDs 1..=3), plus Archive and Trash.
fn state() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    for subject in ["one", "two", "three"] {
        state.inbox().append_simple(subject);
    }
    let mut archive = FakeMailbox::new("Archive").with_attribute("\\Archive");
    archive.uidvalidity = 5;
    state.mailboxes.push(archive);
    state
        .mailboxes
        .push(FakeMailbox::new("Trash").with_attribute("\\Trash"));
    state
}

impl Env {
    async fn list(&self, name: &str) -> Vec<MessageSummary> {
        let folder = self.folder(name).await;
        self.client.list_messages(folder.id, 100, 0).await.unwrap()
    }

    async fn id(&self, name: &str, subject: &str) -> MessageId {
        self.list(name)
            .await
            .into_iter()
            .find(|m| m.subject.as_deref() == Some(subject))
            .unwrap_or_else(|| panic!("no {subject} in {name}"))
            .id
    }

    /// (subject, flags) in the local folder, by subject.
    async fn local(&self, name: &str) -> Vec<(String, Flags)> {
        let mut messages = self.messages(name).await;
        messages.sort_by(|a, b| a.0.cmp(&b.0));
        messages
    }

    /// (subject, flags) on the server, by subject.
    fn remote(&self, name: &str) -> Vec<(String, Vec<String>)> {
        let mut state = self.fake.state();
        let mut out: Vec<_> = state
            .mailbox_mut(name)
            .messages
            .iter()
            .map(|m| {
                let raw = String::from_utf8_lossy(&m.raw);
                let subject = raw
                    .lines()
                    .find_map(|l| l.strip_prefix("Subject: "))
                    .unwrap_or_default()
                    .to_owned();
                (subject, m.flags.clone())
            })
            .collect();
        out.sort();
        out
    }

    async fn pending(&self) -> Vec<(OpKind, OpStatus)> {
        self.client
            .list_pending(Some(self.account))
            .await
            .unwrap()
            .into_iter()
            .map(|op| (op.kind, op.status))
            .collect()
    }
}

fn subjects(list: &[(String, Flags)]) -> Vec<&str> {
    list.iter().map(|(s, _)| s.as_str()).collect()
}

fn names(list: &[(String, Vec<String>)]) -> Vec<&str> {
    list.iter().map(|(s, _)| s.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn flags_change_locally_then_on_the_server() {
    let env = synced_env(state()).await;
    let (one, two) = (env.id("INBOX", "one").await, env.id("INBOX", "two").await);
    let mut events = env.client.subscribe();
    let inbox = env.folder("INBOX").await;

    env.client
        .set_flags(vec![one, two], Flags::SEEN | Flags::FLAGGED, Flags::empty())
        .await
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        Event::MessagesChanged {
            account: env.account,
            folder: inbox.id
        }
    );
    assert_eq!(env.folder("INBOX").await.unread_count, 1);
    // Only locally so far.
    assert!(
        env.remote("INBOX")
            .iter()
            .all(|(_, flags)| flags.is_empty())
    );
    assert_eq!(env.pending().await.len(), 2);

    let start = env.fake.state().log.len();
    let report = env.client.replay_pending(env.account).await.unwrap();
    assert_eq!((report.applied, report.failed, report.pending), (2, 0, 0));
    assert!(
        env.commands_since(start).ends_with(&[
            "SELECT INBOX (CONDSTORE)".into(),
            "UID STORE 1:2 +FLAGS.SILENT (\\Seen \\Flagged)".into()
        ]),
        "{:?}",
        env.commands_since(start)
    );
    assert_eq!(env.remote("INBOX")[0].1, ["\\Seen", "\\Flagged"]);
    assert!(env.pending().await.is_empty());

    // Clearing a flag that isn't set changes nothing and queues nothing.
    let three = env.id("INBOX", "three").await;
    env.client
        .set_flags(vec![three], Flags::empty(), Flags::SEEN)
        .await
        .unwrap();
    assert!(env.pending().await.is_empty());
    let err = env
        .client
        .set_flags(vec![three], Flags::DELETED, Flags::empty())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err}");
    let err = env
        .client
        .set_flags(vec![three, MessageId(999)], Flags::SEEN, Flags::empty())
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::MessageNotFound(MessageId(999))),
        "{err}"
    );
    assert_eq!(
        env.local("INBOX").await[1],
        ("three".into(), Flags::empty()),
        "all or nothing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_replays_first_and_keeps_what_others_changed() {
    let env = synced_env(state()).await;
    let one = env.id("INBOX", "one").await;
    env.client
        .set_flags(vec![one], Flags::SEEN, Flags::empty())
        .await
        .unwrap();
    // Meanwhile another client flags it.
    env.fake.state().inbox().set_flags(1, &["\\Flagged"]);

    let report = env.client.sync_account(env.account).await.unwrap();
    let inbox = report.folders.iter().find(|f| f.name == "INBOX").unwrap();
    assert_eq!((inbox.ops_applied, inbox.ops_failed), (1, 0));
    assert_eq!(env.remote("INBOX")[0].1, ["\\Flagged", "\\Seen"]);
    assert_eq!(env.local("INBOX").await[0].1, Flags::SEEN | Flags::FLAGGED);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_moved_message_keeps_its_body_and_id() {
    let env = synced_env(state()).await;
    let two = env.id("INBOX", "two").await;
    env.client.download_message(two).await.unwrap();
    let archive = env.folder("Archive").await;

    env.client
        .move_messages(vec![two], archive.id)
        .await
        .unwrap();
    assert_eq!(subjects(&env.local("INBOX").await), ["one", "three"]);
    assert_eq!(subjects(&env.local("Archive").await), ["two"]);
    assert_eq!(env.folder("Archive").await.total_count, 1);
    // Still readable offline, from where it is now.
    let message = env.client.get_message(two).await.unwrap().unwrap();
    assert_eq!(message.summary.folder_id, archive.id);
    assert!(message.body.is_some());

    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(names(&env.remote("INBOX")), ["one", "three"]);
    assert_eq!(names(&env.remote("Archive")), ["two"]);
    // Same row: the server said where it went (COPYUID), so sync doesn't fetch it again.
    let list = env.list("Archive").await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, two);
    assert!(
        env.client
            .get_message(two)
            .await
            .unwrap()
            .unwrap()
            .body
            .is_some()
    );
    assert!(env.pending().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_uidplus_moved_messages_are_fetched_again() {
    let mut state = state();
    state.faults.uidplus = false;
    state.faults.move_command = false;
    let env = synced_env(state).await;
    let two = env.id("INBOX", "two").await;
    let archive = env.folder("Archive").await;
    env.client
        .move_messages(vec![two], archive.id)
        .await
        .unwrap();

    let start = env.fake.state().log.len();
    env.client.sync_account(env.account).await.unwrap();
    let commands = env.commands_since(start);
    assert!(
        commands.contains(&"UID COPY 2 Archive".to_owned())
            && commands.contains(&"EXPUNGE".to_owned()),
        "{commands:?}"
    );
    assert_eq!(names(&env.remote("Archive")), ["two"]);
    assert_eq!(subjects(&env.local("Archive").await), ["two"]);
    assert_ne!(env.list("Archive").await[0].id, two);
    assert_eq!(subjects(&env.local("INBOX").await), ["one", "three"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_moved_offline_downloads_from_where_it_was() {
    let env = synced_env(state()).await;
    let two = env.id("INBOX", "two").await;
    let archive = env.folder("Archive").await;
    env.client
        .move_messages(vec![two], archive.id)
        .await
        .unwrap();
    let message = env.client.download_message(two).await.unwrap();
    assert_eq!(message.body.unwrap().text.as_deref(), Some("Hello\r\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_goes_through_trash() {
    let env = synced_env(state()).await;
    let one = env.id("INBOX", "one").await;
    env.client.delete_messages(vec![one], false).await.unwrap();
    assert_eq!(subjects(&env.local("Trash").await), ["one"]);
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(names(&env.remote("Trash")), ["one"]);

    let one = env.id("Trash", "one").await;
    env.client.delete_messages(vec![one], false).await.unwrap();
    let three = env.id("INBOX", "three").await;
    env.client.delete_messages(vec![three], true).await.unwrap();
    assert!(env.local("Trash").await.is_empty());
    env.client.sync_account(env.account).await.unwrap();
    assert!(env.remote("Trash").is_empty());
    assert_eq!(names(&env.remote("INBOX")), ["two"]);
    assert!(env.local("Trash").await.is_empty());
    assert_eq!(subjects(&env.local("INBOX").await), ["two"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_changes_are_undone() {
    let env = synced_env(state()).await;
    let (one, two) = (env.id("INBOX", "one").await, env.id("INBOX", "two").await);
    let archive = env.folder("Archive").await;
    env.client
        .set_flags(vec![one], Flags::FLAGGED, Flags::empty())
        .await
        .unwrap();
    env.client
        .move_messages(vec![two], archive.id)
        .await
        .unwrap();
    env.fake.state().faults.refuse_changes = Some("[CANNOT] Permission denied".into());

    let report = env.client.replay_pending(env.account).await.unwrap();
    assert_eq!((report.applied, report.failed, report.pending), (0, 2, 0));
    assert_eq!(
        env.local("INBOX").await,
        [
            ("one".into(), Flags::empty()),
            ("three".into(), Flags::empty()),
            ("two".into(), Flags::empty())
        ]
    );
    assert_eq!(env.id("INBOX", "two").await, two, "back where it was");
    let ops = env.client.list_pending(None).await.unwrap();
    assert!(ops.iter().all(|op| {
        op.status == OpStatus::Failed
            && op
                .last_error
                .as_deref()
                .unwrap()
                .contains("Permission denied")
    }));

    for op in ops {
        env.client.dismiss_pending(op.id).await.unwrap();
    }
    assert!(env.pending().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_whose_answer_is_lost_are_made_once() {
    let env = synced_env(state()).await;
    let (one, two) = (env.id("INBOX", "one").await, env.id("INBOX", "two").await);
    let archive = env.folder("Archive").await;

    // The flag change is made, but the connection drops before the answer.
    env.client
        .set_flags(vec![one], Flags::SEEN, Flags::empty())
        .await
        .unwrap();
    env.fake.state().faults.drop_after_change = true;
    let err = env.client.replay_pending(env.account).await.unwrap_err();
    assert!(matches!(err, Error::Imap(_)), "{err}");
    assert_eq!(env.remote("INBOX")[0].1, ["\\Seen"]);
    let ops = env.client.list_pending(None).await.unwrap();
    assert_eq!((ops.len(), ops[0].attempts), (1, 1));
    let report = env.client.replay_pending(env.account).await.unwrap();
    assert_eq!((report.applied, report.pending), (1, 0));
    assert_eq!(env.remote("INBOX")[0].1, ["\\Seen"]);

    // Same for a move. This time the connection was reused, which is retried at once on
    // a new one: INBOX no longer has the message, so there is no COPYUID, and the sync
    // fetches it in Archive.
    env.client
        .move_messages(vec![two], archive.id)
        .await
        .unwrap();
    env.fake.state().faults.drop_after_change = true;
    let report = env.client.replay_pending(env.account).await.unwrap();
    assert_eq!((report.applied, report.pending), (1, 0));
    assert_eq!(names(&env.remote("Archive")), ["two"]);
    assert!(env.local("Archive").await.is_empty());
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(names(&env.remote("Archive")), ["two"]);
    assert_eq!(subjects(&env.local("Archive").await), ["two"]);
    assert_eq!(subjects(&env.local("INBOX").await), ["one", "three"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuilt_folder_turns_changes_into_conflicts() {
    let env = synced_env(state()).await;
    let (one, two) = (env.id("INBOX", "one").await, env.id("INBOX", "two").await);
    let archive = env.folder("Archive").await;
    env.client
        .set_flags(vec![one], Flags::SEEN, Flags::empty())
        .await
        .unwrap();
    env.client
        .move_messages(vec![two], archive.id)
        .await
        .unwrap();
    env.fake.state().inbox().reset_uidvalidity(99);
    let mut events = env.client.subscribe();

    // Not under the UIDVALIDITY they were made for: left alone.
    let report = env.client.replay_pending(env.account).await.unwrap();
    assert_eq!((report.applied, report.pending), (0, 2));
    let report = env.client.sync_account(env.account).await.unwrap();
    assert!(report.folders[0].uidvalidity_changed);
    assert_eq!(
        env.pending()
            .await
            .iter()
            .map(|op| op.1)
            .collect::<Vec<_>>(),
        [OpStatus::Conflict, OpStatus::Conflict]
    );
    assert!(
        env.remote("INBOX")
            .iter()
            .all(|(_, flags)| flags.is_empty())
    );
    assert_eq!(subjects(&env.local("INBOX").await), ["one", "three", "two"]);
    assert!(env.local("Archive").await.is_empty());
    // The message moved to Archive is gone from there too, and views are told so.
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    assert!(
        seen.contains(&Event::MessagesChanged {
            account: env.account,
            folder: archive.id
        }) && seen.contains(&Event::PendingOpsChanged {
            account: env.account
        }),
        "{seen:?}"
    );
}

/// Without MOVE, with "two" moved locally to Archive. The message "two" has no
/// Message-ID unless `message_id`.
async fn copy_move_env(message_id: bool) -> (Env, MessageId) {
    let mut state = ServerState::new("alice", "pw");
    state.faults.move_command = false;
    for subject in ["one", "two"] {
        let id = if message_id || subject != "two" {
            format!("Message-ID: <{subject}@example.com>\r\n")
        } else {
            String::new()
        };
        let raw = format!("From: a@example.com\r\nSubject: {subject}\r\n{id}\r\nHello\r\n");
        state.inbox().append(raw.as_bytes(), &[]);
    }
    state.mailboxes.push(FakeMailbox::new("Archive"));
    let env = synced_env(state).await;
    let two = env.id("INBOX", "two").await;
    let archive = env.folder("Archive").await;
    env.client
        .move_messages(vec![two], archive.id)
        .await
        .unwrap();
    (env, two)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_whose_answer_was_lost_is_not_made_again() {
    // Without a Message-ID to look for, too.
    let (env, two) = copy_move_env(false).await;
    env.fake.state().faults.drop_after_change = true;
    assert!(env.client.replay_pending(env.account).await.is_err());
    assert_eq!(names(&env.remote("Archive")), ["two"]);
    assert_eq!(names(&env.remote("INBOX")), ["one", "two"]);

    let report = env.client.replay_pending(env.account).await.unwrap();
    assert_eq!((report.applied, report.pending), (1, 0));
    assert_eq!(names(&env.remote("Archive")), ["two"], "copied once");
    assert_eq!(names(&env.remote("INBOX")), ["one"]);
    // The copy was found, so the message keeps its row and id.
    assert_eq!(env.list("Archive").await[0].id, two);
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(subjects(&env.local("Archive").await), ["two"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_copy_is_undone_when_the_move_is() {
    // Moved back, or deleted, after a COPY whose answer was lost: the copy goes too.
    for delete in [false, true] {
        let (env, two) = copy_move_env(false).await;
        env.fake.state().faults.drop_after_change = true;
        assert!(env.client.replay_pending(env.account).await.is_err());
        assert_eq!(names(&env.remote("Archive")), ["two"]);

        if delete {
            env.client.delete_messages(vec![two], true).await.unwrap();
        } else {
            let inbox = env.folder("INBOX").await;
            env.client.move_messages(vec![two], inbox.id).await.unwrap();
        }
        let report = env.client.replay_pending(env.account).await.unwrap();
        assert_eq!(report.pending, 0, "delete: {delete}");
        assert!(env.remote("Archive").is_empty(), "delete: {delete}");
        let inbox = if delete {
            vec!["one"]
        } else {
            vec!["one", "two"]
        };
        assert_eq!(names(&env.remote("INBOX")), inbox, "delete: {delete}");
        assert!(env.client.list_pending(None).await.unwrap().is_empty());

        env.client.sync_account(env.account).await.unwrap();
        assert_eq!(subjects(&env.local("INBOX").await), inbox);
        assert!(env.local("Archive").await.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_that_never_happened_is_made() {
    let (env, _) = copy_move_env(true).await;
    // A different message with the same Message-ID is in Archive already.
    env.fake.state().mailbox_mut("Archive").append(
        b"From: b@example.com\r\nSubject: other\r\nMessage-ID: <two@example.com>\r\n\r\nHi\r\n",
        &[],
    );
    // The connection drops before the COPY is made.
    env.fake.state().faults.drop_before_change = true;
    assert!(env.client.replay_pending(env.account).await.is_err());
    assert_eq!(names(&env.remote("Archive")), ["other"]);

    let report = env.client.replay_pending(env.account).await.unwrap();
    assert_eq!((report.applied, report.pending), (1, 0));
    assert_eq!(names(&env.remote("Archive")), ["other", "two"], "not lost");
    assert_eq!(names(&env.remote("INBOX")), ["one"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknowable_copy_leaves_the_original() {
    let (env, two) = copy_move_env(true).await;
    env.fake.state().faults.drop_after_change = true;
    assert!(env.client.replay_pending(env.account).await.is_err());
    // Archive is rebuilt: the copy, if any, can't be told apart from other mail.
    env.fake
        .state()
        .mailbox_mut("Archive")
        .reset_uidvalidity(77);

    let report = env.client.replay_pending(env.account).await.unwrap();
    assert_eq!((report.applied, report.failed, report.pending), (0, 1, 0));
    assert_eq!(names(&env.remote("INBOX")), ["one", "two"], "original kept");
    assert_eq!(env.id("INBOX", "two").await, two);
    let ops = env.client.list_pending(None).await.unwrap();
    assert_eq!(ops[0].status, OpStatus::Failed);
    assert!(
        ops[0].last_error.as_deref().unwrap().contains("can't tell"),
        "{ops:?}"
    );
    // Not tried again on its own.
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(names(&env.remote("INBOX")), ["one", "two"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn sent_replies_mark_the_original_answered() {
    let env = synced_env(state()).await;
    let one = env.id("INBOX", "one").await;
    let draft = env.client.reply(one, false).await.unwrap();
    env.client
        .update_draft(
            draft.id,
            DraftContent {
                text: "Thanks".into(),
                ..draft.content
            },
        )
        .await
        .unwrap();
    env.client.send_draft(draft.id).await.unwrap();
    env.client.flush_outbox(env.account).await.unwrap();
    assert_eq!(env.local("INBOX").await[0].1, Flags::ANSWERED);
    env.client.replay_pending(env.account).await.unwrap();
    assert_eq!(env.remote("INBOX")[0].1, ["\\Answered"]);
}
