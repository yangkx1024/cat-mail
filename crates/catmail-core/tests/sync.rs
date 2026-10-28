//! Sync engine against the fake IMAP server, including injected faults.

use std::time::Duration;

use catmail_core::{EngineTuning, Error, ErrorKind, Event, Flags, FolderRole, SyncReport, Verify};
use catmail_testkit::{FakeMailbox, ServerState, SmtpState};

mod common;

use common::{Env, drain, env, env_added, env_built, folder_report, new_mail};

fn state_with(inbox_subjects: &[&str]) -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    for subject in inbox_subjects {
        state.inbox().append_simple(subject);
    }
    state
}

impl Env {
    async fn sync(&self) -> SyncReport {
        self.client.sync_account(self.account).await.unwrap()
    }
}

/// The fake's messages share one Date, so they sort by UID, later first.
fn newest_first(subjects: &[&str]) -> Vec<String> {
    subjects.iter().rev().map(|s| s.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn initial_sync_fetches_folders_and_headers() {
    let mut state = state_with(&["one", "two", "three"]);
    state.inbox().set_flags(1, &["\\Seen"]);
    state.mailboxes.push(FakeMailbox::new("Sent Items"));
    state
        .mailboxes
        .push(FakeMailbox::new("&V4NXPpCuTvY-").with_attribute("\\Junk"));
    state
        .mailboxes
        .push(FakeMailbox::new("Parent").with_attribute("\\Noselect"));
    let env = env(state).await;
    let mut events = env.client.subscribe();

    let report = env.sync().await;
    assert_eq!(folder_report(&report, "INBOX").added, 3);
    assert_eq!(
        report.folders.len(),
        3,
        "the \\Noselect folder is not synced"
    );
    assert!(
        report
            .folders
            .iter()
            .all(|f| f.error.is_none() && !f.uidvalidity_changed)
    );

    let folders = env.client.list_folders(env.account).await.unwrap();
    let summary: Vec<_> = folders
        .iter()
        .map(|f| {
            (
                f.name.as_str(),
                f.role,
                f.selectable,
                f.unread_count,
                f.total_count,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("INBOX", Some(FolderRole::Inbox), true, 2, 3),
            ("Parent", None, false, 0, 0),
            ("Sent Items", Some(FolderRole::Sent), true, 0, 0),
            ("垃圾邮件", Some(FolderRole::Junk), true, 0, 0),
        ]
    );
    assert!(folders[0].last_synced_at.is_some());
    assert_eq!(
        env.subjects("INBOX").await,
        newest_first(&["one", "two", "three"])
    );
    assert_eq!(env.messages("inbox").await[2].1, Flags::SEEN);

    let inbox = env.folder("INBOX").await.id;
    let seen = drain(&mut events);
    assert!(seen.contains(&Event::FoldersChanged {
        account: env.account
    }));
    assert!(seen.contains(&Event::MessagesChanged {
        account: env.account,
        folder: inbox
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn incremental_sync_applies_new_changed_and_vanished() {
    let env = env(state_with(&["one", "two", "three"])).await;
    env.sync().await;

    // Nothing changed: CONDSTORE lets the engine skip both SEARCH and FETCH.
    let start = env.fake.state().log.len();
    let report = env.sync().await;
    assert_eq!(folder_report(&report, "INBOX").added, 0);
    let commands = env.commands_since(start);
    assert!(
        !commands.iter().any(|c| c.starts_with("UID")),
        "unexpected commands: {commands:?}"
    );

    {
        let mut st = env.fake.state();
        let inbox = st.inbox();
        inbox.set_flags(2, &["\\Seen", "\\Flagged"]);
        inbox.expunge(1);
        inbox.append_simple("four");
    }
    let start = env.fake.state().log.len();
    let report = env.sync().await;
    let inbox = folder_report(&report, "INBOX");
    assert_eq!((inbox.added, inbox.updated, inbox.removed), (1, 1, 1));
    assert!(
        env.commands_since(start)
            .iter()
            .any(|c| c.contains("CHANGEDSINCE")),
        "flags are fetched incrementally"
    );
    assert_eq!(
        env.messages("INBOX").await,
        [
            ("four".into(), Flags::empty()),
            ("three".into(), Flags::empty()),
            ("two".into(), Flags::SEEN | Flags::FLAGGED),
        ]
    );
    let folder = env.folder("INBOX").await;
    assert_eq!((folder.unread_count, folder.total_count), (2, 3));
}

#[tokio::test(flavor = "multi_thread")]
async fn new_mail_is_what_arrives_in_inbox_unread() {
    let mut state = state_with(&["one"]);
    state.mailboxes.push(FakeMailbox::new("Archive"));
    let env = env(state).await;
    let mut events = env.client.subscribe();
    env.sync().await;
    assert!(
        new_mail(drain(&mut events)).is_empty(),
        "not the first sync"
    );

    {
        let mut st = env.fake.state();
        st.inbox().append_simple("two");
        let read = st.inbox().append_simple("read");
        st.inbox().set_flags(read, &["\\Seen"]);
        st.mailbox_mut("Archive").append_simple("elsewhere");
    }
    env.sync().await;
    let two = env.id_of("INBOX", "two").await;
    assert_eq!(new_mail(drain(&mut events)), [vec![two]]);
    env.sync().await;
    assert!(new_mail(drain(&mut events)).is_empty(), "announced once");

    // Still new until it is read.
    let still_new = async || -> Vec<_> {
        let found = env.client.unread_in_inbox(vec![two]).await;
        found.unwrap().into_iter().map(|m| m.id).collect()
    };
    assert_eq!(still_new().await, [two]);
    env.client
        .set_flags(vec![two], Flags::SEEN, Flags::empty())
        .await
        .unwrap();
    assert!(still_new().await.is_empty());

    // Every message of a new UIDVALIDITY is new to the engine, not to the user.
    {
        let mut st = env.fake.state();
        st.inbox().append_simple("three");
        st.inbox().reset_uidvalidity(99);
    }
    env.sync().await;
    assert_eq!(env.subjects("INBOX").await.len(), 4);
    assert!(new_mail(drain(&mut events)).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn new_mail_alone_does_not_list_every_uid() {
    let env = env(state_with(&["one", "two", "three"])).await;
    env.sync().await;

    env.fake.state().inbox().append_simple("four");
    let start = env.fake.state().log.len();
    let report = env.sync().await;
    assert_eq!(folder_report(&report, "INBOX").added, 1);
    let searches: Vec<_> = env
        .commands_since(start)
        .into_iter()
        .filter(|c| c.starts_with("UID SEARCH"))
        .collect();
    assert_eq!(searches, ["UID SEARCH UID 4:*"]);

    // An expunge as well: the counts no longer add up, so the full list is needed.
    {
        let mut st = env.fake.state();
        st.inbox().expunge(2);
        st.inbox().append_simple("five");
    }
    let report = env.sync().await;
    let inbox = folder_report(&report, "INBOX");
    assert_eq!((inbox.added, inbox.removed), (1, 1));
    assert_eq!(
        env.subjects("INBOX").await,
        ["five", "four", "three", "one"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_without_condstore_refetches_all_flags() {
    let mut state = state_with(&["one", "two"]);
    state.faults.condstore = false;
    let env = env(state).await;
    env.sync().await;

    env.fake.state().inbox().set_flags(1, &["\\Seen"]);
    let start = env.fake.state().log.len();
    let report = env.sync().await;
    assert_eq!(folder_report(&report, "INBOX").updated, 1);
    let commands = env.commands_since(start);
    assert!(
        !commands
            .iter()
            .any(|c| c.contains("CONDSTORE") || c.contains("CHANGEDSINCE"))
    );
    assert!(
        !commands.iter().any(|c| c.starts_with("UID SEARCH")),
        "UIDNEXT and the count are unchanged, so no SEARCH: {commands:?}"
    );
    assert_eq!(env.messages("INBOX").await[1], ("one".into(), Flags::SEEN));
}

#[tokio::test(flavor = "multi_thread")]
async fn new_messages_are_fetched_in_batches_of_the_tuned_size() {
    let subjects = ["a", "b", "c", "d", "e"];
    let env = env_built(state_with(&subjects), SmtpState::new("alice", "pw"), |b| {
        b.tuning(EngineTuning {
            fetch_batch: 2,
            ..EngineTuning::default()
        })
    })
    .await;
    let start = env.fake.state().log.len();
    assert_eq!(folder_report(&env.sync().await, "INBOX").added, 5);
    let fetches: Vec<_> = env
        .commands_since(start)
        .into_iter()
        .filter(|c| c.starts_with("UID FETCH"))
        .map(|c| c.split(' ').nth(2).unwrap().to_owned())
        .collect();
    // Newest first.
    assert_eq!(fetches, ["4:5", "2:3", "1"]);
    assert_eq!(env.subjects("INBOX").await, newest_first(&subjects));
}

#[tokio::test(flavor = "multi_thread")]
async fn flags_of_a_large_folder_are_fetched_in_windows_without_condstore() {
    let mut state = ServerState::new("alice", "pw");
    state.faults.condstore = false;
    for i in 0..12_000 {
        state.inbox().append_simple(&format!("m{i}"));
    }
    // Sparse UIDs: the windows count stored messages (5000), not UIDs.
    for uid in (2..=4000).step_by(2) {
        state.inbox().expunge(uid);
    }
    let env = env(state).await;
    assert_eq!(folder_report(&env.sync().await, "INBOX").added, 10_000);

    env.fake.state().inbox().set_flags(11_999, &["\\Seen"]);
    env.fake.state().inbox().set_flags(3, &["\\Flagged"]);
    let start = env.fake.state().log.len();
    assert_eq!(folder_report(&env.sync().await, "INBOX").updated, 2);
    let fetches: Vec<_> = env
        .commands_since(start)
        .into_iter()
        .filter(|c| c.starts_with("UID FETCH"))
        .collect();
    assert_eq!(
        fetches,
        [
            "UID FETCH 1:7000 (UID FLAGS)",
            "UID FETCH 7001:12000 (UID FLAGS)"
        ]
    );
    let messages = env.messages("INBOX").await;
    assert!(messages.contains(&("m11998".into(), Flags::SEEN)));
    assert!(messages.contains(&("m2".into(), Flags::FLAGGED)));
}

#[tokio::test(flavor = "multi_thread")]
async fn uidvalidity_change_refetches_the_folder() {
    let env = env(state_with(&["one", "two", "three"])).await;
    env.sync().await;
    let old_ids: Vec<_> = {
        let folder = env.folder("INBOX").await;
        env.client
            .list_messages(folder.id, 10, None)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect()
    };

    {
        let mut st = env.fake.state();
        let inbox = st.inbox();
        inbox.expunge(2);
        inbox.reset_uidvalidity(99); // "one" → UID 1, "three" → UID 2
    }
    let report = env.sync().await;
    let inbox = folder_report(&report, "INBOX");
    assert!(inbox.uidvalidity_changed);
    assert_eq!(inbox.added, 2);
    assert_eq!(env.subjects("INBOX").await, ["three", "one"]);
    let folder = env.folder("INBOX").await;
    let new_ids: Vec<_> = env
        .client
        .list_messages(folder.id, 10, None)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert!(
        new_ids.iter().all(|id| !old_ids.contains(id)),
        "rows of the old generation are gone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_sync_resumes_without_duplicates() {
    let subjects: Vec<String> = (1..=450).map(|i| format!("m{i:03}")).collect();
    let refs: Vec<&str> = subjects.iter().map(String::as_str).collect();
    let mut state = state_with(&refs);
    // The second batch of headers never arrives.
    state.faults.drop_on_fetch = Some(2);
    let env = env(state).await;

    let err = env.client.sync_account(env.account).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Network, "{err:?}");
    let partial = env.subjects("INBOX").await;
    assert_eq!(partial.len(), 200, "the first batch was committed");
    assert_eq!(partial[0], "m450", "newest first");

    let report = env.sync().await;
    assert_eq!(folder_report(&report, "INBOX").added, 250);
    assert_eq!(env.subjects("INBOX").await, newest_first(&refs));
    assert_eq!(env.folder("INBOX").await.total_count, 450);
}

#[tokio::test(flavor = "multi_thread")]
async fn tolerates_misbehaving_responses() {
    let mut state = state_with(&["one", "two", "three"]);
    state.faults.duplicate_fetch = true;
    state.faults.reverse_fetch = true;
    state.faults.unsolicited = true;
    let env = env(state).await;
    let report = env.sync().await;
    assert_eq!(folder_report(&report, "INBOX").added, 3);
    env.fake.state().inbox().set_flags(3, &["\\Seen"]);
    let report = env.sync().await;
    assert_eq!(folder_report(&report, "INBOX").updated, 1);
    assert_eq!(
        env.subjects("INBOX").await,
        newest_first(&["one", "two", "three"])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn folders_removed_on_the_server_are_removed_locally() {
    let mut state = state_with(&["one"]);
    let mut archive = FakeMailbox::new("Archive");
    archive.append_simple("old");
    state.mailboxes.push(archive);
    let env = env(state).await;
    env.sync().await;
    assert_eq!(env.subjects("Archive").await, ["old"]);

    env.fake.state().mailboxes.retain(|m| m.name != "Archive");
    let mut events = env.client.subscribe();
    env.sync().await;
    assert!(
        env.client
            .find_folder(env.account, "Archive")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        events.try_recv().unwrap(),
        Event::FoldersChanged {
            account: env.account
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_is_an_auth_error() {
    let mut state = state_with(&[]);
    state.faults.reject_login = true;
    let env = env(state).await;
    let err = env.client.sync_account(env.account).await.unwrap_err();
    assert!(matches!(err, Error::AuthFailed(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_is_added_only_once_logged_in() {
    let env = env(state_with(&[])).await;
    let account = env.client.get_account(env.account).await.unwrap().unwrap();
    let new = catmail_core::NewAccount {
        email: "alice@example.org".into(),
        display_name: None,
        username: account.username,
        imap: account.imap,
        smtp: env.smtp_config(),
    };
    let add = async |new: &catmail_core::NewAccount, password: &str| {
        env.client
            .add_account(new.clone(), password.into(), Verify::Login)
            .await
    };

    let err = add(&new, "wrong").await.unwrap_err();
    assert!(matches!(err, Error::AuthFailed(_)), "{err:?}");
    let err = add(&new, "").await.unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
    // The SMTP server is tried too, and its failure told apart; the IMAP login that
    // worked is logged out of.
    env.smtp.state().faults.reject_login = true;
    let start = env.fake.state().log.len();
    let err = add(&new, "pw").await.unwrap_err();
    assert!(matches!(err, Error::OutgoingServer(_)), "{err:?}");
    assert_eq!(err.kind(), catmail_core::ErrorKind::Auth);
    let commands = env.commands_since(start);
    assert!(
        commands
            .iter()
            .any(|c| c.starts_with("AUTHENTICATE") || c.starts_with("LOGIN"))
    );
    assert_eq!(commands.last().map(String::as_str), Some("LOGOUT"));
    // So is one whose account can't be stored.
    env.smtp.state().faults.reject_login = false;
    let start = env.fake.state().log.len();
    let taken = catmail_core::NewAccount {
        email: "alice@example.com".into(),
        ..new
    };
    let err = add(&taken, "pw").await.unwrap_err();
    assert!(matches!(err, Error::AlreadyExists(_)), "{err:?}");
    let commands = env.commands_since(start);
    assert_eq!(commands.last().map(String::as_str), Some("LOGOUT"));

    let accounts = env.client.list_accounts().await.unwrap();
    assert_eq!(accounts.len(), 1, "only the account the test set up");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_added_once_logged_in_keeps_that_connection() {
    let env = env_added(
        state_with(&[]),
        SmtpState::new("alice", "pw"),
        Verify::Login,
        |b| b,
    )
    .await;
    assert_eq!(env.fake.state().connections, 1);
    // A whole sync logs in on a connection of its own; the next request takes the one
    // the check left.
    env.client.sync_account(env.account).await.unwrap();
    let inbox = env.folder("INBOX").await;
    env.client
        .sync_folder(inbox.id, Duration::ZERO)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(env.fake.state().connections, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_syncs_of_one_account_are_serialized() {
    let env = env(state_with(&["one", "two", "three"])).await;
    let (a, b) = tokio::join!(
        env.client.sync_account(env.account),
        env.client.sync_account(env.account)
    );
    let added =
        folder_report(&a.unwrap(), "INBOX").added + folder_report(&b.unwrap(), "INBOX").added;
    assert_eq!(added, 3);
    assert_eq!(env.subjects("INBOX").await.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_the_account_cancels_its_sync() {
    let mut state = state_with(&["one"]);
    state.faults.hang_on_fetch = true;
    let env = env(state).await;
    let sync = tokio::spawn({
        let client = env.client.clone();
        let account = env.account;
        async move { client.sync_account(account).await }
    });
    // Wait until the sync is stuck on the header FETCH.
    while !env
        .fake
        .state()
        .log
        .iter()
        .any(|c| c.line.contains("HEADER.FIELDS"))
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    tokio::time::timeout(
        Duration::from_secs(5),
        env.client.remove_account(env.account),
    )
    .await
    .expect("removal must not wait for the network timeout")
    .unwrap();
    assert!(matches!(sync.await.unwrap(), Err(Error::Cancelled)));
    assert!(matches!(
        env.client.sync_account(env.account).await,
        Err(Error::AccountNotFound(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn unparsable_answers_never_delete_local_state() {
    let mut state = state_with(&["one", "two"]);
    state.mailboxes.push(FakeMailbox::new("Archive"));
    let env = env(state).await;
    env.sync().await;

    // A lost LIST line must not look like a deleted folder.
    env.fake.state().faults.garble = Some("\"Archive\"".into());
    let err = env.client.sync_account(env.account).await.unwrap_err();
    assert!(
        err.kind() == ErrorKind::Refused && err.to_string().contains("LIST"),
        "{err:?}"
    );
    assert!(
        env.client
            .find_folder(env.account, "Archive")
            .await
            .unwrap()
            .is_some()
    );

    // A lost SEARCH line must not look like everything was expunged. New mail and an
    // expunge force the full SEARCH.
    {
        let mut st = env.fake.state();
        st.inbox().expunge(1);
        st.inbox().append_simple("three");
        st.faults.garble = Some("* SEARCH".into());
    }
    let report = env.sync().await;
    let inbox = folder_report(&report, "INBOX");
    let error = inbox.error.as_ref().unwrap();
    assert!(
        error.kind == ErrorKind::Refused && error.message.contains("SEARCH"),
        "{inbox:?}"
    );
    assert_eq!(
        env.subjects("INBOX").await,
        ["two", "one"],
        "nothing removed"
    );

    // A lost flag change must not be skipped over by the advancing HIGHESTMODSEQ.
    env.fake.state().inbox().set_flags(2, &["\\Seen"]);
    env.fake.state().faults.garble = Some("FETCH (UID 2 FLAGS".into());
    let report = env.sync().await;
    let inbox = folder_report(&report, "INBOX");
    assert!(
        inbox.error.as_ref().unwrap().message.contains("FETCH"),
        "{inbox:?}"
    );
    assert_eq!(
        inbox.removed, 1,
        "the expunge itself was applied before the flags"
    );
    let report = env.sync().await;
    let inbox = folder_report(&report, "INBOX");
    assert_eq!((inbox.error.as_ref(), inbox.updated), (None, 1));
    assert_eq!(
        env.messages("INBOX").await,
        [
            ("three".into(), Flags::empty()),
            ("two".into(), Flags::SEEN)
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn headers_in_raw_bytes_are_read_in_the_charset_of_the_text() {
    // 报告 and 张三 in raw GBK bytes, as some mail programs write them, beside GBK text.
    let mut state = ServerState::new("alice", "pw");
    state.inbox().append(
        b"Subject: \xb1\xa8\xb8\xe6\r\n\
        From: \xd5\xc5\xc8\xfd <zhang@example.com>\r\n\
        Content-Type: text/plain; charset=gb2312\r\n\r\n\
        \xc7\xeb\xb2\xe9\xca\xd5\r\n",
        &[],
    );
    let env = env(state).await;
    env.sync().await;
    let inbox = env.folder("INBOX").await;
    let list = env.client.list_messages(inbox.id, 10, None).await.unwrap();
    assert_eq!(list[0].subject.as_deref(), Some("报告"));
    assert_eq!(list[0].from[0].name.as_deref(), Some("张三"));
}

#[tokio::test(flavor = "multi_thread")]
async fn unparsable_bodystructure_does_not_hide_the_message() {
    let mut state = state_with(&["one", "two", "three"]);
    state.inbox().message_mut(2).raw = b"Subject: two\r\nContent-Type: multipart/mixed; \
        boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nHello\r\n--b\r\n\
        Content-Type: application/pdf; name=a.pdf\r\n\r\n%PDF\r\n--b--\r\n"
        .to_vec();
    state.faults.broken_bodystructure = vec![2];
    let env = env(state).await;
    let report = env.sync().await;
    let inbox = folder_report(&report, "INBOX");
    assert_eq!((inbox.added, inbox.error.as_ref()), (3, None));
    assert_eq!(
        env.subjects("INBOX").await,
        newest_first(&["one", "two", "three"])
    );

    // Complete, so the cursor moved: nothing new means no SEARCH and no FETCH.
    let start = env.fake.state().log.len();
    env.sync().await;
    let commands = env.commands_since(start);
    assert!(
        !commands
            .iter()
            .any(|c| c.starts_with("UID SEARCH") || c.starts_with("UID FETCH")),
        "{commands:?}"
    );

    // Unknown until the body is downloaded, which doesn't need the BODYSTRUCTURE.
    let inbox = env.folder("INBOX").await;
    let list = env.client.list_messages(inbox.id, 10, None).await.unwrap();
    let two = list
        .iter()
        .find(|m| m.subject.as_deref() == Some("two"))
        .unwrap();
    assert!(!two.has_attachments);
    let message = env.client.download_message(two.id).await.unwrap();
    assert!(message.summary.has_attachments);
}

#[tokio::test(flavor = "multi_thread")]
async fn split_fetch_responses_keep_their_flags() {
    let mut state = state_with(&["one"]);
    state.inbox().set_flags(1, &["\\Seen", "\\Flagged"]);
    state.faults.split_fetch = true;
    let env = env(state).await;
    env.sync().await;
    assert_eq!(
        env.messages("INBOX").await,
        [("one".into(), Flags::SEEN | Flags::FLAGGED)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn preauth_server_syncs_without_login() {
    let mut state = state_with(&["one"]);
    state.faults.preauth = true;
    let env = env(state).await;
    env.sync().await;
    assert_eq!(env.subjects("INBOX").await, ["one"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn mailbox_deleted_under_its_children_is_emptied() {
    let mut state = state_with(&[]);
    let mut parent = FakeMailbox::new("Parent");
    parent.append_simple("old");
    state.mailboxes.push(parent);
    state.mailboxes.push(FakeMailbox::new("Parent/Child"));
    let env = env(state).await;
    env.sync().await;
    assert_eq!(env.subjects("Parent").await, ["old"]);

    {
        let mut st = env.fake.state();
        let parent = st.mailbox_mut("Parent");
        parent.messages.clear();
        parent.attributes.push("\\Noselect".into());
    }
    env.sync().await;
    let parent = env.folder("Parent").await;
    assert!(!parent.selectable);
    assert_eq!(parent.total_count, 0);
    assert!(env.subjects("Parent").await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn listing_without_inbox_deletes_nothing() {
    let mut state = state_with(&["one"]);
    state.mailboxes.push(FakeMailbox::new("Archive"));
    let env = env(state).await;
    env.sync().await;

    // A LIST answer missing its INBOX line, from a server that has one.
    env.fake.state().faults.omit = Some("INBOX".into());
    let failed = env.client.sync_account(env.account).await;
    assert!(failed.is_err(), "{failed:?}");
    assert_eq!(env.subjects("INBOX").await, ["one"]);
    env.folder("Archive").await;

    env.sync().await;
    assert_eq!(env.subjects("INBOX").await, ["one"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn account_without_inbox_syncs() {
    // RFC 3501 §6.3.8: a user the server keeps no INBOX for gets none listed.
    let mut state = ServerState::new("alice", "pw");
    state.mailboxes = vec![FakeMailbox::new("Archive")];
    state.mailbox_mut("Archive").append_simple("kept");
    let env = env(state).await;
    let report = env.sync().await;
    assert_eq!(folder_report(&report, "Archive").added, 1);
    assert_eq!(env.subjects("Archive").await, ["kept"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn incomplete_examine_never_deletes_local_messages() {
    // EXISTS drives both the no-change fast path and the empty-mailbox shortcut; a
    // lost or missing EXISTS line read as 0 would empty the folder.
    let exists = || Some(" EXISTS".to_string());
    for (fault, garble, omit) in [("garble", exists(), None), ("omit", None, exists())] {
        let env = env(state_with(&["one", "two"])).await;
        env.sync().await;
        {
            let mut st = env.fake.state();
            st.inbox().append_simple("three"); // so the fast path doesn't hide it
            (st.faults.garble, st.faults.omit) = (garble, omit);
        }
        let report = env.sync().await;
        let inbox = folder_report(&report, "INBOX");
        assert!(inbox.error.is_some(), "{fault}: {inbox:?}");
        assert_eq!(inbox.removed, 0, "{fault}");
        assert_eq!(env.subjects("INBOX").await, ["two", "one"], "{fault}");

        let report = env.sync().await;
        assert_eq!(
            folder_report(&report, "INBOX").added,
            1,
            "{fault}: recovers"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_folder_brings_one_folder_up_to_date() {
    let mut state = state_with(&["one"]);
    state.mailboxes.push(FakeMailbox::new("Archive"));
    state.mailbox_mut("Archive").append_simple("old");
    let env = env(state).await;
    env.sync().await;
    let archive = env.folder("Archive").await.id;
    {
        let mut st = env.fake.state();
        st.inbox().append_simple("two");
        st.mailbox_mut("Archive").append_simple("new");
    }
    // A change made here goes to the server first, as in a full sync.
    let old = env.id_of("Archive", "old").await;
    env.client
        .set_flags(vec![old], Flags::SEEN, Flags::empty())
        .await
        .unwrap();

    let report = env
        .client
        .sync_folder(archive, Duration::ZERO)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (report.added, report.ops_applied, report.error),
        (1, 1, None)
    );
    assert_eq!(env.subjects("Archive").await, ["new", "old"]);
    assert_eq!(
        env.fake.state().mailbox_mut("Archive").messages[0].flags,
        ["\\Seen"]
    );
    assert_eq!(
        env.subjects("INBOX").await,
        ["one"],
        "only the folder asked for"
    );

    // Synced just now: nothing to do, not even a connection's worth.
    let start = env.fake.state().log.len();
    let skipped = env
        .client
        .sync_folder(archive, Duration::from_secs(60))
        .await;
    assert_eq!(skipped.unwrap(), None);
    assert!(env.commands_since(start).is_empty());

    let missing = catmail_core::FolderId(archive.0 + 100);
    assert!(matches!(
        env.client.sync_folder(missing, Duration::ZERO).await,
        Err(Error::FolderNotFound(id)) if id == missing
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_folder_reports_a_folder_the_server_refuses() {
    let mut state = state_with(&["one"]);
    state.mailboxes.push(FakeMailbox::new("Archive"));
    let env = env(state).await;
    env.sync().await;
    let archive = env.folder("Archive").await.id;
    // Deleted on the server since the last LIST: EXAMINE fails, the call doesn't.
    env.fake.state().mailboxes.retain(|m| m.name != "Archive");

    let report = env
        .client
        .sync_folder(archive, Duration::ZERO)
        .await
        .unwrap()
        .unwrap();
    assert!(report.error.is_some(), "{report:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_skipped_folder_sync_leaves_the_account_status_alone() {
    let mut state = state_with(&["one"]);
    state.mailboxes.push(FakeMailbox::new("Archive"));
    let env = env(state).await;
    env.sync().await;
    let archive = env.folder("Archive").await.id;
    env.fake.state().faults.reject_login = true;
    assert!(env.client.sync_account(env.account).await.is_err());
    assert!(env.client.account_status(env.account).sync.is_some());

    // Synced just now, so not looked at: that says nothing about the server.
    let skipped = env
        .client
        .sync_folder(archive, Duration::from_secs(60))
        .await;
    assert_eq!(skipped.unwrap(), None);
    let problem = env.client.account_status(env.account).sync;
    assert!(problem.is_some_and(|p| p.login_rejected()));
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_folder_goes_in_between_the_folders_of_a_full_sync() {
    let mut state = state_with(&["one"]);
    state.mailboxes.push(FakeMailbox::new("Archive"));
    state.mailboxes.push(FakeMailbox::new("Later"));
    let env = env(state).await;
    env.sync().await;
    let later = env.folder("Later").await.id;
    {
        let mut st = env.fake.state();
        st.mailbox_mut("Later").append_simple("new");
        // Holds the full sync in its first folder until the folder sync is waiting.
        st.faults.slow = Some(("EXAMINE INBOX".into(), Duration::from_millis(500)));
    }
    let start = env.fake.state().log.len();

    let (client, account) = (env.client.clone(), env.account);
    let full = tokio::spawn(async move { client.sync_account(account).await });
    env.fake
        .wait_until(|st| st.received("EXAMINE INBOX") >= 2)
        .await;
    let report = env
        .client
        .sync_folder(later, Duration::ZERO)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.added, 1);

    let full = full.await.unwrap().unwrap();
    assert_eq!(
        env.examined(start),
        ["INBOX", "Later", "Archive"],
        "the opened folder went next, and the full sync left it alone after"
    );
    assert_eq!(full.folders.len(), 3, "{full:?}");
    assert_eq!(folder_report(&full, "Later").added, 0);
    assert_eq!(env.subjects("Later").await, ["new"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_sync_doesnt_skip_a_folder_whose_sync_began_before_it() {
    let mut state = state_with(&[]);
    // Without CONDSTORE every sync fetches the flags, which is where it is held up.
    state.faults.condstore = false;
    state.mailboxes.push(FakeMailbox::new("Archive"));
    state.mailbox_mut("Archive").append_simple("old");
    let env = env(state).await;
    env.sync().await;
    let archive = env.folder("Archive").await.id;
    let fetches = {
        let mut st = env.fake.state();
        st.faults.slow = Some(("UID FETCH".into(), Duration::from_millis(500)));
        st.received("UID FETCH")
    };

    let folder_sync = tokio::spawn({
        let client = env.client.clone();
        async move { client.sync_folder(archive, Duration::ZERO).await }
    });
    // Past its EXAMINE: what arrives now is news to it.
    env.fake
        .wait_until(|st| st.received("UID FETCH") > fetches)
        .await;
    {
        let mut st = env.fake.state();
        // The FETCH already held stays held; the full sync's needn't be.
        st.faults.slow = None;
        st.mailbox_mut("Archive").append_simple("new");
    }

    // Asked for while the folder sync runs, so it has to see the new message.
    let full = env.sync().await;
    folder_sync.await.unwrap().unwrap();
    assert_eq!(folder_report(&full, "Archive").added, 1, "{full:?}");
    assert_eq!(env.subjects("Archive").await, ["new", "old"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clock_set_back_doesnt_make_a_full_sync_skip_folders() {
    let mut state = state_with(&["one"]);
    state.mailboxes.push(FakeMailbox::new("Archive"));
    let env = env(state).await;
    env.sync().await;
    // Synced before the clock was set back a day: stamped in what is now the future.
    env.sql(
        "UPDATE folders SET last_synced_at = last_synced_at + 86400
         WHERE remote_name = 'Archive'",
    );
    env.fake.state().mailbox_mut("Archive").append_simple("new");

    let report = env.sync().await;
    assert_eq!(folder_report(&report, "Archive").added, 1, "{report:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clock_set_back_doesnt_make_an_opened_folder_look_fresh() {
    let mut state = state_with(&["one"]);
    state.mailboxes.push(FakeMailbox::new("Archive"));
    let env = env(state).await;
    env.sync().await;
    let archive = env.folder("Archive").await.id;
    // Synced before the clock was set back a day: stamped in what is now the future.
    env.sql(
        "UPDATE folders SET last_synced_at = last_synced_at + 86400
         WHERE remote_name = 'Archive'",
    );
    env.fake.state().mailbox_mut("Archive").append_simple("new");

    let report = env
        .client
        .sync_folder(archive, Duration::ZERO)
        .await
        .unwrap()
        .expect("synced");
    assert_eq!(report.added, 1, "{report:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn after_resuming_an_opened_folder_is_synced_again() {
    let mut state = state_with(&["one"]);
    state.mailboxes.push(FakeMailbox::new("Archive"));
    let env = env(state).await;
    env.sync().await;
    let archive = env.folder("Archive").await.id;
    let fresh = Duration::from_secs(3600);
    assert_eq!(env.client.sync_folder(archive, fresh).await.unwrap(), None);

    // The time since it was synced, by the clock that didn't count the sleep, says
    // nothing of how old it is.
    env.client.resume();
    let report = env.client.sync_folder(archive, fresh).await.unwrap();
    assert!(report.is_some(), "synced");
}
