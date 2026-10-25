//! Gmail against the fake server in its Gmail mode: All Mail, Trash and Spam are synced
//! as one, every other mailbox is a label kept with the messages (see
//! `docs/architecture/labels.md`).

use std::sync::Arc;
use std::time::Duration;

use catmail_core::{
    AccountMode, Event, Flags, FolderKind, MessageId, OpStatus, SearchQuery, SyncReport,
    SyncSchedule,
};
use catmail_testkit::{FakeMailbox, ServerState, SmtpState};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

mod common;

use common::{Env, drain, env, env_built, eventually, folder_report, synced_env};

const ALL_MAIL: &str = "[Gmail]/All Mail";
const TRASH: &str = "[Gmail]/Trash";

/// "one" in INBOX and Work, "two" archived (in All Mail alone), "old" in Trash.
fn gmail() -> ServerState {
    let mut state = ServerState::gmail("alice", "pw");
    state.mailboxes.push(FakeMailbox::new("Work"));
    let all = state.all_mail();
    let one = all.append_simple("one");
    all.set_labels(one, &["\\Inbox", "Work"]);
    all.append_simple("two");
    state.mailbox_mut(TRASH).append_simple("old");
    state
}

impl Env {
    /// Whether the message's body is stored, so that it needn't be downloaded again.
    async fn has_body(&self, id: MessageId) -> bool {
        let message = self.client.get_message(id).await.unwrap();
        message.is_some_and(|m| m.body.is_some())
    }

    fn move_on_server(&self, from: &str, uid: u32, to: &str) -> u32 {
        self.fake.state().move_message(from, uid, to)
    }

    /// Makes `change` on the server, then starts a sync and waits until its first header
    /// FETCH arrives, which it holds until the gate returned gets permits.
    async fn held_sync(
        &self,
        change: impl FnOnce(&mut ServerState),
    ) -> (Arc<Semaphore>, JoinHandle<catmail_core::Result<SyncReport>>) {
        let gate = Arc::new(Semaphore::new(0));
        let fetched = {
            let mut st = self.fake.state();
            change(&mut st);
            st.faults.fetch_gate = Some(gate.clone());
            st.header_fetches()
        };
        let sync = tokio::spawn({
            let (client, account) = (self.client.clone(), self.account);
            async move { client.sync_account(account).await }
        });
        self.fake
            .wait_until(|st| st.header_fetches() > fetched)
            .await;
        (gate, sync)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn labels_are_kept_with_the_messages_of_three_locations() {
    let env = env(gmail()).await;
    env.client.sync_account(env.account).await.unwrap();

    // Only the locations are opened, and their messages come with their labels.
    assert_eq!(env.examined(0), [ALL_MAIL, "[Gmail]/Spam", TRASH]);
    let commands = env.commands_since(0);
    assert!(
        commands
            .iter()
            .any(|c| c.contains("FETCH") && c.contains("X-GM-LABELS")),
        "{commands:?}"
    );
    let account = env.client.get_account(env.account).await.unwrap().unwrap();
    assert_eq!(account.mode, AccountMode::Gmail);
    for (name, kind) in [
        ("INBOX", FolderKind::Label),
        ("Work", FolderKind::Label),
        ("[Gmail]/Starred", FolderKind::Label),
        (ALL_MAIL, FolderKind::Location),
        (TRASH, FolderKind::Location),
    ] {
        assert_eq!(env.folder(name).await.kind, kind, "{name}");
    }
    assert_eq!(env.subjects("INBOX").await, ["one"]);
    assert_eq!(env.subjects("Work").await, ["one"]);
    assert_eq!(env.subjects(ALL_MAIL).await, ["two", "one"]);
    assert_eq!(env.subjects(TRASH).await, ["old"]);
    assert_eq!(env.folder("INBOX").await.total_count, 1);

    // A message is one row, however many labels it has.
    let found = env.client.search(SearchQuery::new("alice")).await.unwrap();
    assert_eq!(found.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_moved_to_trash_and_back_keeps_its_row() {
    let env = synced_env(gmail()).await;
    let one = env.id_of("INBOX", "one").await;
    env.client.download_message(one).await.unwrap();
    let (inbox, work) = (env.folder("INBOX").await.id, env.folder("Work").await.id);
    let mut events = env.client.subscribe();

    // Another client trashes it: Gmail takes \Inbox off and keeps Work.
    let uid = env.move_on_server(ALL_MAIL, 1, TRASH);
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.id_of(TRASH, "one").await, one);
    assert!(env.has_body(one).await);
    assert!(env.subjects("INBOX").await.is_empty());
    assert!(env.subjects("Work").await.is_empty(), "Trash isn't shown");
    assert_eq!(env.subjects(ALL_MAIL).await, ["two"]);
    assert_eq!(env.folder("INBOX").await.total_count, 0);
    let seen = drain(&mut events);
    for folder in [inbox, work] {
        let changed = Event::MessagesChanged {
            account: env.account,
            folder,
        };
        assert!(seen.contains(&changed), "{folder}: {seen:?}");
    }

    // And back, under yet another UID.
    env.move_on_server(TRASH, uid, ALL_MAIL);
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.id_of(ALL_MAIL, "one").await, one);
    assert!(env.has_body(one).await);
    assert_eq!(env.subjects("Work").await, ["one"]);
    assert_eq!(env.subjects(TRASH).await, ["old"]);
}

/// A message gone from All Mail isn't deleted before Trash and Spam are synced: the
/// connection lost in between, it is still there, to be found in Trash next time.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_missed_in_one_location_waits_for_the_others() {
    let env = synced_env(gmail()).await;
    let one = env.id_of("INBOX", "one").await;
    env.client.download_message(one).await.unwrap();
    env.move_on_server(ALL_MAIL, 1, TRASH);
    {
        let mut st = env.fake.state();
        // Trash's FETCH of the message's headers; All Mail has nothing new.
        st.faults.drop_on_fetch = Some(st.header_fetches() + 1);
    }
    assert!(env.client.sync_account(env.account).await.is_err());
    assert!(env.has_body(one).await);

    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.id_of(TRASH, "one").await, one);
    assert!(env.has_body(one).await);
    assert_eq!(env.subjects(ALL_MAIL).await, ["two"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn labels_changed_alone_come_with_the_flags() {
    let env = synced_env(gmail()).await;
    let inbox = env.folder("INBOX").await.id;
    let mut events = env.client.subscribe();
    env.fake.state().all_mail().set_labels(2, &["\\Inbox"]);

    let start = env.fake.state().log.len();
    let report = env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.subjects("INBOX").await, ["two", "one"]);
    let all_mail = folder_report(&report, ALL_MAIL);
    assert_eq!((all_mail.added, all_mail.updated), (0, 1));
    let commands = env.commands_since(start);
    assert!(
        commands
            .iter()
            .any(|c| c.contains("X-GM-LABELS") && c.contains("CHANGEDSINCE")),
        "{commands:?}"
    );
    assert!(drain(&mut events).contains(&Event::MessagesChanged {
        account: env.account,
        folder: inbox,
    }));
}

/// All Mail hidden from IMAP in Gmail's settings: the labels are synced as folders, until
/// it is shown again. Each time every location starts over, and changes not yet made on
/// the server no longer apply.
#[tokio::test(flavor = "multi_thread")]
async fn hiding_all_mail_syncs_the_labels_as_folders() {
    let env = synced_env(gmail()).await;
    let old = env.id_of(TRASH, "old").await;
    env.client
        .set_flags(
            vec![old],
            catmail_core::Flags::SEEN,
            catmail_core::Flags::empty(),
        )
        .await
        .unwrap();
    let hidden = {
        let mut st = env.fake.state();
        let at = st
            .mailboxes
            .iter()
            .position(|m| m.name == ALL_MAIL)
            .unwrap();
        (at, st.mailboxes.remove(at))
    };

    env.client.sync_account(env.account).await.unwrap();
    let account = env.client.get_account(env.account).await.unwrap().unwrap();
    assert_eq!(account.mode, AccountMode::Standard);
    assert_eq!(env.folder("INBOX").await.kind, FolderKind::Location);
    assert_eq!(env.subjects("INBOX").await, ["one"]);
    assert_eq!(env.subjects(TRASH).await, ["old"]);
    let ops = env.client.list_pending(Some(env.account)).await.unwrap();
    let statuses: Vec<_> = ops.iter().map(|op| op.status).collect();
    assert_eq!(statuses, [OpStatus::Conflict]);
    let found = env.client.search(SearchQuery::new("one")).await.unwrap();
    assert_eq!(found.len(), 2, "INBOX and Work are folders now");

    env.fake.state().mailboxes.insert(hidden.0, hidden.1);
    env.client.sync_account(env.account).await.unwrap();
    let account = env.client.get_account(env.account).await.unwrap().unwrap();
    assert_eq!(account.mode, AccountMode::Gmail);
    assert_eq!(env.folder("INBOX").await.kind, FolderKind::Label);
    assert_eq!(env.subjects("INBOX").await, ["one"]);
    assert_eq!(env.subjects(ALL_MAIL).await, ["two", "one"]);
    let found = env.client.search(SearchQuery::new("one")).await.unwrap();
    assert_eq!(found.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn background_sync_idles_on_inbox_and_syncs_the_locations() {
    const HOUR: Duration = Duration::from_secs(3600);
    let schedule = SyncSchedule {
        full_sync: HOUR,
        poll: Duration::from_millis(50),
        idle: HOUR,
        retry: Duration::from_millis(50),
    };
    let env = env_built(gmail(), SmtpState::new("alice", "pw"), |b| {
        b.sync_in_background(Some(schedule))
    })
    .await;
    let inbox = async || {
        let found = env.client.find_folder(env.account, "INBOX").await.unwrap();
        match found {
            Some(folder) => {
                let list = env.client.list_messages(folder.id, 10, None).await;
                list.unwrap()
                    .into_iter()
                    .filter_map(|m| m.subject)
                    .collect()
            }
            None => Vec::new(),
        }
    };
    eventually("INBOX to show one", async || inbox().await == ["one"]).await;
    env.fake.wait_until(|st| st.received("IDLE") >= 1).await;
    let start = env.fake.state().log.len();

    {
        let mut st = env.fake.state();
        let all = st.all_mail();
        let three = all.append_simple("three");
        all.set_labels(three, &["\\Inbox"]);
    }
    eventually("INBOX to show three", async || {
        inbox().await == ["three", "one"]
    })
    .await;
    // Woken by IDLE on INBOX, it synced the locations, and opened INBOX to IDLE again.
    env.fake.wait_until(|st| st.received("IDLE") >= 2).await;
    let examined = env.examined(start);
    assert_eq!(examined, [ALL_MAIL, "[Gmail]/Spam", TRASH, "INBOX"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_a_label_syncs_the_locations() {
    let env = synced_env(gmail()).await;
    let work = env.folder("Work").await;
    {
        let mut st = env.fake.state();
        let all = st.all_mail();
        let three = all.append_simple("three");
        all.set_labels(three, &["Work"]);
    }
    // Just synced with the account.
    let hour = Duration::from_secs(3600);
    assert_eq!(env.client.sync_folder(work.id, hour).await.unwrap(), None);

    let start = env.fake.state().log.len();
    let report = env
        .client
        .sync_folder(work.id, Duration::ZERO)
        .await
        .unwrap();
    let report = report.unwrap();
    assert_eq!((report.folder, report.added), (work.id, 1));
    assert_eq!(env.examined(start), [ALL_MAIL, "[Gmail]/Spam", TRASH]);
    assert_eq!(env.subjects("Work").await, ["three", "one"]);
    assert_eq!(env.client.sync_folder(work.id, hour).await.unwrap(), None);
}

/// Moved back to All Mail while the sync is past it and still on its way to Trash: Trash
/// misses it, and All Mail is looked at again before that counts as gone.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_moved_back_during_the_sync_keeps_its_row() {
    let env = synced_env(gmail()).await;
    let one = env.id_of("INBOX", "one").await;
    env.client.download_message(one).await.unwrap();
    let trashed = env.move_on_server(ALL_MAIL, 1, TRASH);
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.id_of(TRASH, "one").await, one);

    // Spam, between All Mail and Trash, has something to fetch: hold it there.
    let (gate, sync) = env
        .held_sync(|st| {
            st.mailbox_mut("[Gmail]/Spam").append_simple("spam");
        })
        .await;
    env.move_on_server(TRASH, trashed, ALL_MAIL);
    gate.add_permits(10);
    sync.await.unwrap().unwrap();

    assert_eq!(env.id_of(ALL_MAIL, "one").await, one);
    assert!(env.has_body(one).await);
    assert_eq!(env.subjects(TRASH).await, ["old"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn local_changes_tell_the_labels_showing_the_message() {
    let env = synced_env(gmail()).await;
    let one = env.id_of("INBOX", "one").await;
    let folder = async |name| env.folder(name).await.id;
    let (inbox, work, starred) = (
        folder("INBOX").await,
        folder("Work").await,
        folder("[Gmail]/Starred").await,
    );
    let mut events = env.client.subscribe();
    let mut all_told = || {
        let seen = drain(&mut events);
        for folder in [inbox, work, starred] {
            let changed = Event::MessagesChanged {
                account: env.account,
                folder,
            };
            assert!(seen.contains(&changed), "{folder}: {seen:?}");
        }
    };

    env.client
        .set_flags(vec![one], Flags::SEEN | Flags::FLAGGED, Flags::empty())
        .await
        .unwrap();
    all_told();
    env.client.delete_messages(vec![one], false).await.unwrap();
    all_told();
    assert!(env.subjects("INBOX").await.is_empty());
}

/// Important hidden from IMAP when the messages were fetched: X-GM-LABELS named it, but
/// there was no mailbox to tie it to. Shown again, it lists them.
#[tokio::test(flavor = "multi_thread")]
async fn a_system_label_shown_again_gets_its_messages() {
    let mut state = gmail();
    state.all_mail().set_labels(2, &["\\Important"]);
    let at = state
        .mailboxes
        .iter()
        .position(|m| m.name == "[Gmail]/Important")
        .unwrap();
    let important = state.mailboxes.remove(at);
    let env = synced_env(state).await;
    assert!(
        env.client
            .find_folder(env.account, "[Gmail]/Important")
            .await
            .unwrap()
            .is_none()
    );

    env.fake.state().mailboxes.insert(at, important);
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.subjects("[Gmail]/Important").await, ["two"]);
}

/// A message moved while the sync looks again at the locations (from Spam to All Mail,
/// once All Mail was looked at): it vanished during that look, so it is looked for once
/// more rather than deleted.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_moved_while_the_locations_are_looked_at_again_keeps_its_row() {
    const SPAM: &str = "[Gmail]/Spam";
    let mut state = gmail();
    let spam_uid = state.mailbox_mut(SPAM).append_simple("keep me");
    let env = synced_env(state).await;
    let kept = env.id_of(SPAM, "keep me").await;
    env.client.download_message(kept).await.unwrap();

    let (gate, sync) = env
        .held_sync(|st| {
            st.mailbox_mut(SPAM).append_simple("first pass blocker");
        })
        .await;
    // Held in Spam's first pass: something vanishes from Trash, which has the sync look
    // again, and All Mail gets something new to hold that look at.
    let fetched = {
        let mut st = env.fake.state();
        st.mailbox_mut(TRASH).expunge(1);
        st.all_mail().append_simple("recheck blocker");
        st.header_fetches()
    };
    gate.add_permits(1);
    env.fake
        .wait_until(|st| st.header_fetches() > fetched)
        .await;
    env.move_on_server(SPAM, spam_uid, ALL_MAIL);
    gate.add_permits(10);
    sync.await.unwrap().unwrap();

    assert!(env.has_body(kept).await);
    assert_eq!(env.id_of(ALL_MAIL, "keep me").await, kept);
    assert!(
        env.subjects(TRASH).await.is_empty(),
        "gone for good is deleted"
    );
}
