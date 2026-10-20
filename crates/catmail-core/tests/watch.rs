//! Background sync (`MailClientBuilder::sync_in_background`) against the fake IMAP
//! server: IDLE on INBOX, polling without it, the full sync schedule, queued changes
//! made at once, reconnecting, and stopping with the account or the client.

use std::time::Duration;

use catmail_core::{Config, Flags, MailClient, SyncSchedule};
use catmail_testkit::{FakeMailbox, ServerState, SmtpState};
use tokio::runtime::Handle;

mod common;

use common::{Env, env_built, eventually};

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
    async fn subjects(&self, folder: &str) -> Vec<String> {
        let Some(folder) = self.client.find_folder(self.account, folder).await.unwrap() else {
            return Vec::new();
        };
        let messages = self.client.list_messages(folder.id, 100, 0).await.unwrap();
        let mut subjects: Vec<_> = messages
            .into_iter()
            .map(|m| m.subject.unwrap_or_default())
            .collect();
        subjects.sort();
        subjects
    }

    async fn wait_for_subjects(&self, folder: &str, expected: &[&str]) {
        let what = format!("{folder} to have {expected:?}");
        eventually(&what, async || self.subjects(folder).await == expected).await;
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
    assert_eq!(env.subjects("INBOX").await, ["one", "two"]);

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
    let env = watching(state, schedule(HOUR, HOUR)).await;
    env.fake.wait_until(|st| st.received("LOGIN") >= 2).await;

    env.fake.state().faults.reject_login = false;
    env.wait_for_subjects("INBOX", &["one", "two"]).await;
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
        client.list_messages(inbox.id, 10, 0).await.unwrap().len() == 3
    })
    .await;
}
