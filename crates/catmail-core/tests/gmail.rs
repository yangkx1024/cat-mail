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
    // Still Gmail's server: only All Mail is hidden.
    assert!(account.gmail_server && account.all_mail_hidden());
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
    assert!(!account.all_mail_hidden());
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

impl Env {
    /// The location on the server of the message with this subject, and its labels,
    /// sorted.
    fn on_server(&self, subject: &str) -> (String, Vec<String>) {
        let header = format!("Subject: {subject}\r\n");
        let st = self.fake.state();
        for mb in st.mailboxes.iter().filter(|mb| mb.is_location()) {
            for msg in &mb.messages {
                if msg
                    .raw
                    .windows(header.len())
                    .any(|w| w == header.as_bytes())
                {
                    let mut labels = msg.labels.clone();
                    labels.sort();
                    return (mb.name.clone(), labels);
                }
            }
        }
        panic!("no message {subject} on the server");
    }

    /// Changes made on the server since `start`, besides what reading sends.
    fn changes_since(&self, start: usize) -> Vec<String> {
        self.commands_since(start)
            .into_iter()
            .filter(|c| {
                ["UID STORE", "UID MOVE", "UID COPY", "UID EXPUNGE"]
                    .iter()
                    .any(|p| c.starts_with(p))
            })
            .collect()
    }
}

/// [`gmail`] with a second user label, Later.
fn gmail_later() -> ServerState {
    let mut state = gmail();
    state.mailboxes.push(FakeMailbox::new("Later"));
    state
}

#[tokio::test(flavor = "multi_thread")]
async fn moving_between_labels_changes_labels_where_the_message_is() {
    let env = synced_env(gmail_later()).await;
    let one = env.id_of("INBOX", "one").await;
    let (inbox, later) = (env.folder("INBOX").await.id, env.folder("Later").await.id);

    env.client
        .move_messages(vec![one], Some(inbox), later)
        .await
        .unwrap();
    assert!(env.subjects("INBOX").await.is_empty());
    assert_eq!(env.subjects("Later").await, ["one"]);
    let start = env.fake.state().log.len();
    assert_eq!(env.replay().await.applied, 2);
    // In All Mail, where the message is; never `\Deleted` in a label's mailbox.
    assert_eq!(
        env.changes_since(start),
        [
            "UID STORE 1 -X-GM-LABELS (\\Inbox)",
            "UID STORE 1 +X-GM-LABELS (Later)"
        ]
    );
    assert!(
        env.commands_since(start)
            .iter()
            .any(|c| c.starts_with("SELECT") && c.contains(ALL_MAIL)),
    );
    assert_eq!(
        env.on_server("one"),
        (ALL_MAIL.into(), vec!["Later".into(), "Work".into()])
    );

    // The sync finds it as it is.
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.id_of("Later", "one").await, one);
    assert!(env.subjects("INBOX").await.is_empty());
    assert_eq!(env.subjects("Work").await, ["one"]);

    // Labels added and taken off directly; Starred is \Flagged.
    let work = env.folder("Work").await.id;
    env.client
        .label_messages(vec![one], vec![inbox], vec![work])
        .await
        .unwrap();
    env.replay().await;
    assert_eq!(
        env.on_server("one"),
        (ALL_MAIL.into(), vec!["Later".into(), "\\Inbox".into()])
    );
    let starred = env.folder("[Gmail]/Starred").await.id;
    let refused = env
        .client
        .label_messages(vec![one], vec![starred], vec![])
        .await;
    assert!(
        matches!(refused, Err(catmail_core::Error::InvalidInput(_))),
        "{refused:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn archiving_takes_the_message_out_of_inbox() {
    let env = synced_env(gmail()).await;
    let one = env.id_of("INBOX", "one").await;
    env.client.archive_messages(vec![one]).await.unwrap();
    assert!(env.subjects("INBOX").await.is_empty());
    assert_eq!(env.subjects(ALL_MAIL).await, ["two", "one"]);
    let start = env.fake.state().log.len();
    env.replay().await;
    assert_eq!(
        env.changes_since(start),
        ["UID STORE 1 -X-GM-LABELS (\\Inbox)"]
    );
    assert_eq!(env.on_server("one"), (ALL_MAIL.into(), vec!["Work".into()]));
}

/// To Trash and back to INBOX, the message keeps its row (and body), as it does when
/// moved on the server.
#[tokio::test(flavor = "multi_thread")]
async fn deleted_and_moved_back_to_inbox_it_keeps_its_row() {
    let env = synced_env(gmail()).await;
    let one = env.id_of("INBOX", "one").await;
    env.client.download_message(one).await.unwrap();
    env.client.delete_messages(vec![one], false).await.unwrap();
    assert!(env.subjects("INBOX").await.is_empty());
    let start = env.fake.state().log.len();
    env.replay().await;
    // INBOX is taken off as Gmail does, once there (to no effect: the change is known
    // before the server makes it).
    let changes = env.changes_since(start);
    assert_eq!(changes[0], "UID MOVE 1 \"[Gmail]/Trash\"");
    assert!(
        changes[1].ends_with("-X-GM-LABELS (\\Inbox)"),
        "{changes:?}"
    );
    assert_eq!(changes.len(), 2);
    assert_eq!(env.on_server("one"), (TRASH.into(), vec!["Work".into()]));

    // Out of Trash: into All Mail, where INBOX is a label, then the label.
    let (trash, inbox) = (env.folder(TRASH).await.id, env.folder("INBOX").await.id);
    env.client
        .move_messages(vec![one], Some(trash), inbox)
        .await
        .unwrap();
    assert_eq!(env.subjects("INBOX").await, ["one"]);
    assert_eq!(env.subjects("Work").await, ["one"]);
    let start = env.fake.state().log.len();
    let report = env.replay().await;
    assert_eq!((report.applied, report.pending), (2, 0));
    let changes = env.changes_since(start);
    assert!(changes[0].starts_with("UID MOVE "), "{changes:?}");
    assert!(
        changes[0].ends_with(&format!("\"{ALL_MAIL}\"")),
        "{changes:?}"
    );
    assert!(
        changes[1].ends_with("+X-GM-LABELS (\\Inbox)"),
        "{changes:?}"
    );
    assert_eq!(
        env.on_server("one"),
        (ALL_MAIL.into(), vec!["Work".into(), "\\Inbox".into()])
    );

    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.id_of("INBOX", "one").await, one);
    assert!(env.has_body(one).await);
    assert_eq!(env.subjects(TRASH).await, ["old"]);
}

/// INBOX added to a message in Trash: Gmail would take `+\\Inbox` there as a move out of
/// Trash, so it is the move to INBOX, made the same way.
#[tokio::test(flavor = "multi_thread")]
async fn inbox_added_in_trash_moves_the_message_out() {
    let env = synced_env(gmail()).await;
    let old = env.id_of(TRASH, "old").await;
    let (inbox, work) = (env.folder("INBOX").await.id, env.folder("Work").await.id);
    env.client
        .label_messages(vec![old], vec![inbox, work], Vec::new())
        .await
        .unwrap();
    assert!(env.subjects(TRASH).await.is_empty());
    assert_eq!(env.subjects("INBOX").await, ["old", "one"]);
    let start = env.fake.state().log.len();
    let report = env.replay().await;
    assert_eq!((report.applied, report.pending), (3, 0));
    let changes = env.changes_since(start);
    assert_eq!(changes.len(), 3, "{changes:?}");
    assert!(
        changes[0].starts_with("UID MOVE ") && changes[0].ends_with(&format!("\"{ALL_MAIL}\"")),
        "{changes:?}"
    );
    assert!(
        changes[1..].iter().all(|c| c.contains("+X-GM-LABELS")),
        "{changes:?}"
    );
    assert_eq!(
        env.on_server("old"),
        (ALL_MAIL.into(), vec!["Work".into(), "\\Inbox".into()])
    );
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.id_of("INBOX", "old").await, old);
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_label_changes_are_undone() {
    let env = synced_env(gmail_later()).await;
    let one = env.id_of("INBOX", "one").await;
    let (inbox, later) = (env.folder("INBOX").await.id, env.folder("Later").await.id);
    env.client
        .move_messages(vec![one], Some(inbox), later)
        .await
        .unwrap();
    env.fake.state().faults.refuse_changes = Some("[CANNOT] Not now".into());
    let mut events = env.client.subscribe();
    let report = env.replay().await;
    assert_eq!((report.applied, report.failed), (0, 2));
    assert_eq!(env.subjects("INBOX").await, ["one"]);
    assert!(env.subjects("Later").await.is_empty());
    assert!(drain(&mut events).contains(&Event::ChangesRefused {
        account: env.account
    }));
    let ops = env.client.list_pending(Some(env.account)).await.unwrap();
    assert!(
        ops.iter().all(|op| op.status == OpStatus::Failed),
        "{ops:?}"
    );
}

/// Drafts is a label, which keeps no UIDVALIDITY locally: the copy a newer one replaces
/// is deleted all the same, under the generation APPENDUID gave.
#[tokio::test(flavor = "multi_thread")]
async fn replaced_draft_copies_are_deleted() {
    let env = synced_env(gmail()).await;
    let content = |subject: &str| catmail_core::DraftContent {
        subject: subject.into(),
        ..Default::default()
    };
    let draft = env
        .client
        .create_draft(env.account, None, content("draft one"), Vec::new())
        .await
        .unwrap()
        .id;
    env.replay().await;
    assert_eq!(env.on_server("draft one").1, ["\\Draft"]);

    env.edit(draft, content("draft two")).await.unwrap();
    let report = env.replay().await;
    assert_eq!((report.drafts_uploaded, report.pending), (1, 0));
    let drafts = env
        .fake
        .state()
        .mailbox_mut("[Gmail]/Drafts")
        .messages
        .len();
    assert_eq!(drafts, 1, "the old copy is gone");
    assert_eq!(env.on_server("draft two").1, ["\\Draft"]);
    let ops = env.client.list_pending(Some(env.account)).await.unwrap();
    assert!(ops.is_empty(), "{ops:?}");

    // Its deletion left pending, then Drafts rebuilt: the copy can't be told, and the op
    // is a conflict rather than pending for ever.
    env.edit(draft, content("draft three")).await.unwrap();
    {
        // On a new connection, which isn't tried again.
        let mut state = env.fake.state();
        state.drop_open_connections();
        state.faults.drop_before_change = true;
    }
    assert!(env.replay().await.error.is_some());
    env.fake.state().mailbox_mut("[Gmail]/Drafts").uidvalidity += 1;
    let report = env.replay().await;
    assert_eq!(report.pending, 0);
    let ops = env.client.list_pending(Some(env.account)).await.unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].status, OpStatus::Conflict);
}

/// Gmail saves what goes out through its SMTP server to Sent itself.
#[tokio::test(flavor = "multi_thread")]
async fn sent_mail_is_not_saved_to_sent_again() {
    let env = synced_env(gmail()).await;
    let content = catmail_core::DraftContent {
        subject: "hi".into(),
        to: vec![common::addr("bob@example.com")],
        ..Default::default()
    };
    let draft = env
        .client
        .create_draft(env.account, None, content, Vec::new())
        .await
        .unwrap();
    env.client.send_draft(draft.id).await.unwrap();
    env.flush().await;
    assert_eq!(env.smtp.state().received.len(), 1);
    assert_eq!(env.fake.state().received("APPEND"), 0);
    assert!(
        env.client
            .list_outbox(Some(env.account))
            .await
            .unwrap()
            .is_empty()
    );
}

/// Out of Trash into INBOX, then deleted for good before that was made: it is deleted in
/// Trash, without `\\Inbox` added there first, which on Gmail takes it out of Trash.
#[tokio::test(flavor = "multi_thread")]
async fn a_restore_out_of_trash_deleted_for_good_in_time_is_deleted_there() {
    let env = synced_env(gmail()).await;
    let old = env.id_of(TRASH, "old").await;
    let (trash, inbox) = (env.folder(TRASH).await.id, env.folder("INBOX").await.id);
    env.client
        .move_messages(vec![old], Some(trash), inbox)
        .await
        .unwrap();
    env.client.delete_messages(vec![old], true).await.unwrap();
    let start = env.fake.state().log.len();
    let report = env.replay().await;
    assert_eq!(report.pending, 0);
    let changes = env.changes_since(start);
    assert!(
        changes.iter().all(|c| !c.contains("X-GM-LABELS")),
        "{changes:?}"
    );
    assert!(
        changes.iter().any(|c| c.starts_with("UID EXPUNGE")),
        "{changes:?}"
    );
    assert!(env.subjects(TRASH).await.is_empty());
    assert_eq!(env.subjects("INBOX").await, ["one"]);
}

/// Out of Trash into INBOX, then deleted again before that was made: nothing is moved, and
/// `\\Inbox` is never added to the message in Trash.
#[tokio::test(flavor = "multi_thread")]
async fn a_restore_out_of_trash_cancelled_in_time_adds_no_inbox_there() {
    let env = synced_env(gmail()).await;
    let old = env.id_of(TRASH, "old").await;
    let (trash, inbox) = (env.folder(TRASH).await.id, env.folder("INBOX").await.id);
    env.client
        .move_messages(vec![old], Some(trash), inbox)
        .await
        .unwrap();
    env.client.delete_messages(vec![old], false).await.unwrap();
    assert_eq!(env.subjects(TRASH).await, ["old"]);
    let start = env.fake.state().log.len();
    let report = env.replay().await;
    assert_eq!(report.pending, 0);
    let changes = env.changes_since(start);
    assert!(
        changes
            .iter()
            .all(|c| !c.contains("+X-GM-LABELS") && !c.contains("MOVE")),
        "{changes:?}"
    );
    assert_eq!(env.on_server("old"), (TRASH.into(), vec![]));
    assert_eq!(env.subjects("INBOX").await, ["one"]);
}
