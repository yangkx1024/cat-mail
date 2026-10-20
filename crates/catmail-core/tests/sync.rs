//! Sync engine against the fake IMAP server, including injected faults.

use std::time::Duration;

use catmail_core::{Error, Event, Flags, FolderRole, ImapError, SyncReport};
use catmail_testkit::{FakeMailbox, ServerState};

mod common;

use common::{Env, drain, env};

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

    async fn subjects(&self, name: &str) -> Vec<String> {
        self.messages(name).await.into_iter().map(|m| m.0).collect()
    }
}

fn folder_report<'a>(report: &'a SyncReport, name: &str) -> &'a catmail_core::FolderSyncReport {
    report.folders.iter().find(|f| f.name == name).unwrap()
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
async fn uidvalidity_change_refetches_the_folder() {
    let env = env(state_with(&["one", "two", "three"])).await;
    env.sync().await;
    let old_ids: Vec<_> = {
        let folder = env.folder("INBOX").await;
        env.client
            .list_messages(folder.id, 10, 0)
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
        .list_messages(folder.id, 10, 0)
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
    assert!(matches!(err, Error::Imap(ImapError::Closed)), "{err:?}");
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
async fn check_login_logs_in_and_stores_nothing() {
    let env = env(state_with(&[])).await;
    let account = env.client.get_account(env.account).await.unwrap().unwrap();
    let new = catmail_core::NewAccount {
        email: "alice@example.org".into(),
        display_name: None,
        username: account.username,
        imap: account.imap,
        smtp: account.smtp,
        auth: account.auth,
    };

    let start = env.fake.state().log.len();
    env.client.check_login(&new, "pw").await.unwrap();
    let commands = env.commands_since(start);
    assert!(
        commands
            .iter()
            .any(|c| c.starts_with("AUTHENTICATE") || c.starts_with("LOGIN"))
    );
    assert_eq!(commands.last().map(String::as_str), Some("LOGOUT"));

    let err = env.client.check_login(&new, "wrong").await.unwrap_err();
    assert!(matches!(err, Error::AuthFailed(_)), "{err:?}");
    let err = env.client.check_login(&new, "").await.unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");

    let accounts = env.client.list_accounts().await.unwrap();
    assert_eq!(accounts.len(), 1, "only the account the test set up");
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
        matches!(err, Error::Imap(ImapError::Incomplete("LIST"))),
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
    assert!(
        inbox.error.as_deref().unwrap().contains("SEARCH"),
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
        inbox.error.as_deref().unwrap().contains("FETCH"),
        "{inbox:?}"
    );
    assert_eq!(
        inbox.removed, 1,
        "the expunge itself was applied before the flags"
    );
    let report = env.sync().await;
    let inbox = folder_report(&report, "INBOX");
    assert_eq!((inbox.error.as_deref(), inbox.updated), (None, 1));
    assert_eq!(
        env.messages("INBOX").await,
        [
            ("three".into(), Flags::empty()),
            ("two".into(), Flags::SEEN)
        ]
    );
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
    assert_eq!((inbox.added, inbox.error.as_deref()), (3, None));
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
    let list = env.client.list_messages(inbox.id, 10, 0).await.unwrap();
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
    assert!(problem.is_some_and(|p| p.auth_failed));
}
