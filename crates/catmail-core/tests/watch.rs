//! Background sync (`MailClientBuilder::sync_in_background`) against the fake IMAP
//! server: IDLE on INBOX, polling without it, the full sync schedule, queued changes
//! made at once, reconnecting, and stopping with the account or the client.

use std::time::Duration;

use catmail_core::{Config, Flags, MailClient, SyncSchedule, Verify};
use catmail_testkit::{FakeMailbox, ServerState, SmtpState};
use tokio::runtime::Handle;
use tokio::sync::Semaphore;

mod common;

use common::{Env, env_added, env_built, eventually, status_changes};

const HOUR: Duration = Duration::from_secs(3600);

/// Quick to retry and to poll; the full sync and IDLE as `full_sync` and `idle` say.
fn schedule(full_sync: Duration, idle: Duration) -> SyncSchedule {
    SyncSchedule {
        full_sync,
        poll: Duration::from_millis(50),
        idle,
        retry: Duration::from_millis(50),
        ..Default::default()
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

    fn syncing(&self) -> bool {
        self.client.account_status(self.account).syncing
    }

    async fn wait_until_synced(&self) {
        eventually("the sync to end", async || !self.syncing()).await;
    }

    async fn wait_until_login_rejected(&self) {
        eventually("the failure to be reported", async || {
            self.client
                .account_status(self.account)
                .sync
                .is_some_and(|p| p.login_rejected())
        })
        .await;
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

#[tokio::test(flavor = "multi_thread")]
async fn an_account_shows_as_syncing_until_its_first_sync_ends() {
    let mut state = state();
    let gate = state.hold_fetches();
    let env = watching(state, schedule(HOUR, HOUR)).await;
    env.fake.wait_until(|st| st.header_fetches() > 0).await;
    assert!(env.syncing());

    gate.add_permits(Semaphore::MAX_PERMITS);
    env.wait_until_synced().await;
    assert_eq!(env.sorted_subjects("Archive").await, ["old"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn syncing_now_syncs_every_folder_on_background_syncs_connection() {
    let env = watching(state(), schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;
    env.wait_until_synced().await;
    let (gate, fetches) = {
        let mut st = env.fake.state();
        st.mailbox_mut("Archive").append_simple("new");
        (st.hold_fetches(), st.header_fetches())
    };

    env.client.sync_now().await.unwrap();
    env.fake
        .wait_until(|st| st.header_fetches() > fetches)
        .await;
    assert!(env.syncing());
    // Answered by the sync under way.
    env.client.sync_now().await.unwrap();
    gate.add_permits(Semaphore::MAX_PERMITS);
    env.wait_for_subjects("Archive", &["new", "old"]).await;
    env.wait_until_synced().await;
    env.wait_for_idle(2).await;
    let st = env.fake.state();
    assert_eq!((st.connections, st.received("LIST")), (1, 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_sync_begun_before_syncing_now_is_made_again() {
    let env = watching(state(), schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;
    env.wait_until_synced().await;
    let (gate, fetches) = {
        let mut st = env.fake.state();
        st.mailbox_mut("Archive").append_simple("new");
        (st.hold_fetches(), st.header_fetches())
    };
    // One the user doesn't wait for, which may have done folders before the call.
    let earlier = tokio::spawn({
        let (client, account) = (env.client.clone(), env.account);
        async move { client.sync_account(account).await }
    });
    env.fake
        .wait_until(|st| st.header_fetches() > fetches)
        .await;

    env.client.sync_now().await.unwrap();
    gate.add_permits(Semaphore::MAX_PERMITS);
    earlier.await.unwrap().unwrap();
    env.wait_until_synced().await;
    // The first, the earlier one, and background sync's on the connection it keeps:
    // unlike after a sleep, nothing starts over.
    let st = env.fake.state();
    assert_eq!((st.connections, st.received("LIST")), (2, 3));
}

#[tokio::test(flavor = "multi_thread")]
async fn syncing_now_while_the_login_is_rejected_waits_too() {
    let mut state = state();
    state.faults.reject_login = true;
    let env = watching(state, schedule(HOUR, HOUR)).await;
    env.wait_until_login_rejected().await;
    assert!(!env.syncing());

    env.client.sync_now().await.unwrap();
    assert!(!env.syncing());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(env.fake.state().received("LOGIN"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_rejected_after_syncing_now_shows_rather_than_syncing() {
    let env = watching(state(), schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;
    env.wait_until_synced().await;
    // The password changed elsewhere: logging in again after the connection drops fails.
    let logins = {
        let mut st = env.fake.state();
        st.faults.reject_login = true;
        st.faults.slow = Some(("LOGIN".into(), Duration::from_millis(200)));
        st.drop_open_connections();
        st.received("LOGIN")
    };
    env.fake
        .wait_until(|st| st.received("LOGIN") > logins)
        .await;

    // Asked while that login, begun before, is under way.
    env.client.sync_now().await.unwrap();
    assert!(env.syncing());
    env.wait_until_login_rejected().await;
    // Nothing answers the request until the login works again.
    env.wait_until_synced().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(env.fake.state().received("LOGIN"), logins + 1);
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
async fn resuming_starts_over_on_a_new_connection() {
    let env = watching(state(), schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;
    // Dead after a sleep: DONE would never be answered.
    env.fake.state().silence_open_connections();
    let connections = env.fake.state().connections;

    env.client.resume();
    env.wait_for_idle(2).await;
    {
        let st = env.fake.state();
        assert_eq!(st.connections, connections + 1);
        assert_eq!(st.received("LIST"), 2, "every folder synced again");
    }
    env.fake.wait_until(|st| st.open_connections == 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_sync_begun_before_the_sleep_is_made_again_after_resuming() {
    let mut state = state();
    // Polling: the wait after the sync takes the signal at once.
    state.faults.idle = false;
    let gate = state.hold_fetches();
    let env = watching(state, schedule(HOUR, HOUR)).await;
    env.fake.wait_until(|st| st.header_fetches() > 0).await;
    let connections = env.fake.state().connections;

    // The connection outlived a short sleep, and the sync goes on to complete.
    env.client.resume();
    gate.add_permits(Semaphore::MAX_PERMITS);
    env.fake.wait_until(|st| st.connections > connections).await;
    // A few polls on the new connection, after whatever its first step synced.
    let examined = env.fake.state().received("EXAMINE");
    env.fake
        .wait_until(|st| st.received("EXAMINE") >= examined + 3)
        .await;
    assert_eq!(
        env.fake.state().received("LIST"),
        2,
        "every folder synced again"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_made_after_resuming_is_kept() {
    let env = watching(state(), schedule(HOUR, HOUR)).await;
    env.wait_for_idle(1).await;
    let (gate, connections, fetches) = {
        let mut st = env.fake.state();
        st.inbox().append_simple("three");
        (st.hold_fetches(), st.connections, st.header_fetches())
    };
    env.fake
        .wait_until(|st| st.header_fetches() > fetches)
        .await;

    // The step under way finds its connection dead after the sleep, and goes on on a
    // new one.
    env.client.resume();
    {
        let mut st = env.fake.state();
        st.faults.fetch_gate = None;
        st.drop_open_connections();
    }
    gate.add_permits(Semaphore::MAX_PERMITS);
    env.wait_for_idle(2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(env.fake.state().connections, connections + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wake_while_the_login_is_rejected_waits_too() {
    let mut state = state();
    state.faults.reject_login = true;
    state.faults.slow = Some(("LOGIN".into(), Duration::from_millis(200)));
    let env = watching(state, schedule(HOUR, HOUR)).await;
    env.fake.wait_until(|st| st.received("LOGIN") >= 1).await;

    // Before the rejection is known: kept for when background sync next waits.
    env.client
        .create_draft(env.account, None, Default::default(), Vec::new())
        .await
        .unwrap();
    env.client.resume();
    env.wait_until_login_rejected().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.fake.state().received("LOGIN"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn failing_to_connect_after_a_wake_tries_again_soon_until_through() {
    let schedule = SyncSchedule {
        retry: HOUR,
        retry_after_wake: Duration::from_millis(20),
        ..schedule(HOUR, HOUR)
    };
    let env = watching(state(), schedule).await;
    env.wait_for_idle(1).await;
    // Dead after the sleep, and the network not back yet.
    {
        let mut st = env.fake.state();
        st.silence_open_connections();
        st.faults.refuse_connections = 3;
    }
    let connections = env.fake.state().connections;

    env.client.resume();
    env.wait_for_idle(2).await;
    assert_eq!(env.fake.state().connections, connections + 4);

    // Through since: failing again waits as long as ever.
    {
        let mut st = env.fake.state();
        st.drop_open_connections();
        st.faults.refuse_connections = 1;
    }
    env.fake
        .wait_until(|st| st.connections == connections + 5)
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let st = env.fake.state();
    assert_eq!((st.received("IDLE"), st.connections), (2, connections + 5));
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
async fn a_rejected_login_is_reported_and_not_retried_for_each_change_or_a_wake() {
    let mut state = state();
    state.faults.reject_login = true;
    let env = watching(state, schedule(HOUR, HOUR)).await;
    let mut events = env.client.subscribe();
    env.fake.wait_until(|st| st.received("LOGIN") >= 1).await;
    env.wait_until_login_rejected().await;

    // Each change wakes background sync, which would log in again, as would waking from
    // sleep: servers lock accounts that fail too many logins.
    for i in 0..5 {
        env.client
            .create_draft(env.account, None, Default::default(), Vec::new())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20 * i)).await;
    }
    env.client.resume();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(env.fake.state().received("LOGIN"), 1);

    // The password works again, as a sync asked for finds: background sync goes on.
    env.fake.state().faults.reject_login = false;
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.client.account_status(env.account), Default::default());
    env.wait_for_idle(1).await;
    let changed = status_changes(&mut events, env.account);
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
