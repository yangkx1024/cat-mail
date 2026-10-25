//! Background sync (`MailClientBuilder::sync_in_background`) against the fake IMAP
//! server: IDLE on INBOX, polling without it, the full sync schedule, queued changes
//! made at once, reconnecting, and stopping with the account or the client.

use std::time::Duration;

use catmail_core::{Config, ErrorKind, Event, Flags, MailClient, SyncReport, SyncSchedule, Verify};
use catmail_testkit::{FakeMailbox, ServerState, SmtpState};
use tokio::runtime::Handle;

mod common;

use common::{Env, drain, env_added, env_built, eventually, gmail_trashes, labelled};

const HOUR: Duration = Duration::from_secs(3600);

/// Quick to retry and to poll; the full sync and IDLE as `full_sync` and `idle` say.
fn schedule(full_sync: Duration, idle: Duration) -> SyncSchedule {
    SyncSchedule {
        full_sync,
        poll: Duration::from_millis(50),
        idle,
        retry: Duration::from_millis(50),
    }
}

/// INBOX with "one" and "two", and an Archive.
fn state() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    state.inbox().append_simple("one");
    state.inbox().append_simple("two");
    let mut archive = FakeMailbox::new("Archive").with_attribute("\\Archive");
    archive.append_simple("old");
    state.mailboxes.push(archive);
    state
}

async fn watching(state: ServerState, schedule: SyncSchedule) -> Env {
    env_built(state, SmtpState::new("alice", "pw"), |b| {
        b.sync_in_background(Some(schedule))
    })
    .await
}

impl Env {
    /// Subjects in the folder, sorted; none if it isn't there (yet).
    async fn sorted_subjects(&self, folder: &str) -> Vec<String> {
        let Some(folder) = self.client.find_folder(self.account, folder).await.unwrap() else {
            return Vec::new();
        };
        let messages = self
            .client
            .list_messages(folder.id, 100, None)
            .await
            .unwrap();
        let mut subjects: Vec<_> = messages
            .into_iter()
            .map(|m| m.subject.unwrap_or_default())
            .collect();
        subjects.sort();
        subjects
    }

    async fn wait_for_subjects(&self, folder: &str, expected: &[&str]) {
        let what = format!("{folder} to have {expected:?}");
        eventually(&what, async || {
            self.sorted_subjects(folder).await == expected
        })
        .await;
    }

    /// Waits until the server has received the `n`-th IDLE.
    async fn wait_for_idle(&self, n: usize) {
        self.fake.wait_until(|st| st.received("IDLE") >= n).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn new_mail_flag_changes_and_expunges_are_pushed() {
    let env = watching(state(), schedule(HOUR, HOUR)).await;
    // The account was added after the client opened; it is synced all the same.
    env.wait_for_subjects("Archive", &["old"]).await;
    env.wait_for_idle(1).await;
    assert_eq!(env.sorted_subjects("INBOX").await, ["one", "two"]);

    env.fake.state().inbox().append_simple("three");
    env.wait_for_subjects("INBOX", &["one", "three", "two"])
        .await;

    env.wait_for_idle(2).await;
    env.fake.state().inbox().set_flags(1, &["\\Seen"]);
    eventually("one to be read", async || {
        env.messages("INBOX")
            .await
            .contains(&("one".into(), Flags::SEEN))
    })
    .await;

    env.wait_for_idle(3).await;
    env.fake.state().inbox().expunge(2);
    env.wait_for_subjects("INBOX", &["one", "three"]).await;
    // Only INBOX is watched; the rest waits for the next full sync.
    assert_eq!(env.fake.state().received("LIST"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_added_once_logged_in_is_watched_on_that_connection() {
    let env = just_added(state()).await;
    env.wait_for_idle(1).await;
    assert_eq!(env.fake.state().connections, 1);
}

/// Background sync with its first sync of every folder to come, on the connection that
/// checked the password.
async fn just_added(state: ServerState) -> Env {
    env_added(state, SmtpState::new("alice", "pw"), Verify::Login, |b| {
        b.sync_in_background(Some(schedule(HOUR, HOUR)))
    })
    .await
}

fn synced_folders(report: &SyncReport) -> Vec<&str> {
    let mut names: Vec<_> = report.folders.iter().map(|f| f.name.as_str()).collect();
    names.sort_unstable();
    names
}

#[tokio::test(flavor = "multi_thread")]
async fn the_initial_sync_after_the_first_background_one_syncs_on_its_own() {
    let env = just_added(state()).await;
    env.wait_for_idle(1).await;
    let report = env.client.initial_sync(env.account).await.unwrap();
    assert_eq!(synced_folders(&report), ["Archive", "INBOX"]);
    let st = env.fake.state();
    assert_eq!((st.connections, st.received("LIST")), (2, 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_initial_sync_waiting_for_a_failed_background_one_syncs_on_its_own() {
    let mut state = state();
    state.faults.reject_login = true;
    let env = watching(state, schedule(HOUR, HOUR)).await;
    let error = env.client.initial_sync(env.account).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Auth);
    // Background sync's, then its own.
    assert_eq!(env.fake.state().received("LOGIN"), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_condstore_only_the_flags_idle_reports_are_fetched() {
    let mut state = state();
    state.faults.condstore = false;
    let env = watching(state, schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;
    let flags_fetches = || {
        let st = env.fake.state();
        st.log
            .iter()
            .filter(|c| c.line.ends_with("(UID FLAGS)"))
            .map(|c| c.line.clone())
            .collect::<Vec<_>>()
    };
    let before = flags_fetches();

    env.fake.state().inbox().append_simple("three");
    env.wait_for_subjects("INBOX", &["one", "three", "two"])
        .await;
    env.wait_for_idle(2).await;
    assert_eq!(flags_fetches(), before, "new mail fetches no flags");

    // Another client's flag change, which IDLE reports: only that message's.
    env.fake.state().inbox().set_flags(2, &["\\Seen"]);
    eventually("two to be read", async || {
        env.messages("INBOX")
            .await
            .contains(&("two".into(), Flags::SEEN))
    })
    .await;
    assert_eq!(flags_fetches()[before.len()..], ["UID FETCH 2 (UID FLAGS)"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_flags_refresh_is_tried_again() {
    let mut state = state();
    state.faults.condstore = false;
    let env = watching(state, schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;

    // The answer to the flags FETCH IDLE's report brings can't be parsed, once.
    {
        let mut st = env.fake.state();
        st.faults.garble = Some("UID 1 FLAGS (\\Seen)".into());
        st.inbox().set_flags(1, &["\\Seen"]);
    }
    eventually("one to be read", async || {
        env.messages("INBOX")
            .await
            .contains(&("one".into(), Flags::SEEN))
    })
    .await;
    assert!(env.fake.state().faults.garble.is_none(), "the fault fired");
}

#[tokio::test(flavor = "multi_thread")]
async fn queued_changes_reach_the_server_at_once() {
    let env = watching(state(), schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;
    let one = env.id_of("INBOX", "one").await;

    env.client
        .set_flags(vec![one], Flags::FLAGGED, Flags::empty())
        .await
        .unwrap();
    eventually("the server to have the flag", async || {
        env.fake.state().inbox().messages[0].flags == ["\\Flagged"]
    })
    .await;
    eventually("nothing pending", async || {
        let pending = env.client.list_pending(Some(env.account)).await.unwrap();
        pending.is_empty()
    })
    .await;
    // And back to IDLE, without a full sync.
    env.wait_for_idle(2).await;
    assert_eq!(env.fake.state().received("LIST"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_moved_out_leaves_its_other_gmail_labels_at_once() {
    let env = watching(labelled(true), schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;
    let id = env.id_of("INBOX", "labelled").await;

    gmail_trashes(&env.fake);
    env.client.delete_messages(vec![id], false).await.unwrap();
    env.wait_for_subjects("Important", &[]).await;
    env.wait_for_subjects("All Mail", &[]).await;
    assert_eq!(env.sorted_subjects("Trash").await, ["labelled"]);
    // Without a full sync.
    env.wait_for_idle(2).await;
    assert_eq!(env.fake.state().received("LIST"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn other_gmail_labels_are_looked_at_after_a_drop_past_the_move() {
    let env = watching(labelled(true), schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;
    let id = env.id_of("INBOX", "labelled").await;
    gmail_trashes(&env.fake);
    // The move is made and answered; the connection drops looking at the labels.
    env.fake.state().faults.drop_on_open = Some("All Mail".into());

    env.client.delete_messages(vec![id], false).await.unwrap();
    env.wait_for_subjects("Important", &[]).await;
    env.wait_for_subjects("All Mail", &[]).await;
    assert_eq!(env.sorted_subjects("Trash").await, ["labelled"]);
    assert!(env.fake.state().faults.drop_on_open.is_none(), "dropped");
    assert_eq!(env.fake.state().received("LIST"), 1, "without a full sync");
}

#[tokio::test(flavor = "multi_thread")]
async fn quiet_idle_is_started_over_without_a_sync() {
    let env = watching(state(), schedule(HOUR, Duration::from_millis(50))).await;
    env.wait_for_idle(3).await;
    // INBOX and Archive in the full sync, then INBOX before the first IDLE.
    assert_eq!(env.fake.state().received("EXAMINE"), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnects_when_the_connection_drops() {
    let env = watching(state(), schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;
    let connections = env.fake.state().connections;

    env.fake.state().drop_open_connections();
    env.wait_for_idle(2).await;
    assert_eq!(env.fake.state().connections, connections + 1);
    env.fake.state().inbox().append_simple("three");
    env.wait_for_subjects("INBOX", &["one", "three", "two"])
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn backs_off_when_idle_keeps_dropping() {
    let mut state = state();
    state.faults.drop_on_idle = true;
    let schedule = SyncSchedule {
        retry: HOUR,
        ..schedule(HOUR, HOUR)
    };
    let env = watching(state, schedule).await;
    // An isolated drop is answered at once; the next one waits.
    env.wait_for_idle(2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let st = env.fake.state();
    assert_eq!((st.received("IDLE"), st.connections), (2, 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn polls_inbox_without_idle() {
    let mut state = state();
    state.faults.idle = false;
    let env = watching(state, schedule(HOUR, HOUR)).await;
    env.wait_for_subjects("INBOX", &["one", "two"]).await;

    env.fake.state().inbox().append_simple("three");
    env.wait_for_subjects("INBOX", &["one", "three", "two"])
        .await;
    assert_eq!(env.fake.state().received("IDLE"), 0);
    // On the one connection.
    assert_eq!(env.fake.state().connections, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn syncs_every_folder_on_schedule() {
    let env = watching(state(), schedule(Duration::from_millis(300), HOUR)).await;
    env.wait_for_subjects("Archive", &["old"]).await;

    env.fake.state().mailbox_mut("Archive").append_simple("new");
    env.wait_for_subjects("Archive", &["new", "old"]).await;
    assert!(env.fake.state().received("LIST") >= 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn tries_again_after_a_failure() {
    let mut state = state();
    state.faults.reject_login = true;
    // A rejected login is tried again as often as every folder is synced.
    let env = watching(state, schedule(Duration::from_millis(100), HOUR)).await;
    env.fake.wait_until(|st| st.received("LOGIN") >= 2).await;

    env.fake.state().faults.reject_login = false;
    env.wait_for_subjects("INBOX", &["one", "two"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_login_is_reported_and_not_retried_for_each_change() {
    let mut state = state();
    state.faults.reject_login = true;
    let env = watching(state, schedule(HOUR, HOUR)).await;
    let mut events = env.client.subscribe();
    env.fake.wait_until(|st| st.received("LOGIN") >= 1).await;
    eventually("the failure to be reported", async || {
        env.client
            .account_status(env.account)
            .sync
            .is_some_and(|p| p.error.kind == ErrorKind::Auth)
    })
    .await;

    // Each change wakes background sync, which would log in again: servers lock
    // accounts that fail too many logins.
    for i in 0..5 {
        env.client
            .create_draft(env.account, None, Default::default(), Vec::new())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20 * i)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(env.fake.state().received("LOGIN"), 1);

    // The password works again, as a sync asked for finds: background sync goes on.
    env.fake.state().faults.reject_login = false;
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.client.account_status(env.account), Default::default());
    env.wait_for_idle(1).await;
    let changed = drain(&mut events)
        .into_iter()
        .filter(|e| {
            *e == Event::AccountStatusChanged {
                account: env.account,
            }
        })
        .count();
    assert_eq!(changed, 2, "failing, then through again");
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_the_server_cuts_short_turns_into_polling() {
    let mut state = state();
    state.faults.end_idle_early = true;
    let env = watching(state, schedule(HOUR, HOUR)).await;
    env.wait_for_idle(3).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        env.fake.state().received("IDLE"),
        3,
        "not IDLE again and again"
    );

    env.fake.state().inbox().append_simple("three");
    env.wait_for_subjects("INBOX", &["one", "three", "two"])
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_ending_at_once_every_time_slows_down() {
    // A line that can't be parsed counts as a change, so each IDLE ends at once.
    let mut state = state();
    state.faults.garble_idle = true;
    let schedule = SyncSchedule {
        poll: Duration::from_secs(1),
        ..schedule(HOUR, HOUR)
    };
    let env = watching(state, schedule).await;
    env.wait_for_idle(1).await;
    // Ten at once, then 50, 100, 200, 400, 800 ms apart, and a second.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let idles = env.fake.state().received("IDLE");
    assert!(idles <= 20, "INBOX synced in a loop: {idles} IDLEs");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_removal_keeps_the_account_in_sync() {
    let env = watching(state(), schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;

    env.sql(
        "CREATE TRIGGER full_disk BEFORE DELETE ON accounts
         BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END;",
    );
    assert!(env.client.remove_account(env.account).await.is_err());
    env.sql("DROP TRIGGER full_disk;");
    assert!(env.client.get_account(env.account).await.unwrap().is_some());

    env.fake.state().inbox().append_simple("three");
    env.wait_for_subjects("INBOX", &["one", "three", "two"])
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_with_the_account() {
    let env = watching(state(), schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;

    let removal = env.client.remove_account(env.account);
    tokio::time::timeout(Duration::from_secs(10), removal)
        .await
        .expect("removal doesn't wait for the IDLE")
        .unwrap();
    env.fake.wait_until(|st| st.open_connections == 0).await;
    let connections = env.fake.state().connections;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(env.fake.state().connections, connections, "no reconnecting");
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_with_the_client_and_starts_with_it() {
    let Env {
        fake,
        client,
        account,
        secrets,
        dir,
        ..
    } = watching(state(), schedule(HOUR, HOUR)).await;
    fake.wait_until(|st| st.received("IDLE") >= 1).await;

    drop(client);
    fake.wait_until(|st| st.open_connections == 0).await;

    // Accounts that exist when the client opens are watched too.
    fake.state().inbox().append_simple("three");
    let client = MailClient::builder(Config::with_data_dir(dir.path()))
        .runtime(Handle::current())
        .secrets(secrets)
        .sync_in_background(Some(schedule(HOUR, HOUR)))
        .open()
        .unwrap();
    let inbox = client.find_folder(account, "INBOX").await.unwrap().unwrap();
    eventually("three to arrive", async || {
        client
            .list_messages(inbox.id, 10, None)
            .await
            .unwrap()
            .len()
            == 3
    })
    .await;
}
