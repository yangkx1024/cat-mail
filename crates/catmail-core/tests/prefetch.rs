//! Fetching bodies in the background against the fake IMAP server.

use std::time::Duration;

use std::sync::Arc;

use catmail_core::{BodyPrefetch, Config, Event, MailClient, MessageId, SearchQuery};
use catmail_testkit::{FakeMailbox, ServerState, SmtpState};
use tokio::runtime::Handle;

mod common;

use common::{Env, drain, env_built, eventually};

/// A message whose body says `word`, the `n`-th of the day: later ones are newer.
fn mail(word: &str, n: u32) -> Vec<u8> {
    format!(
        "From: Bob <bob@example.com>\r\nTo: alice@example.com\r\nSubject: Message {n}\r\n\
         Date: Sat, 17 Oct 2026 {n:02}:00:00 +0000\r\nMessage-ID: <{n}.{word}@example.com>\r\n\
         \r\nThe word is {word}.\r\n"
    )
    .into_bytes()
}

/// Over `whole_limit` in [`prefetching`]: text plus an attachment.
fn large_mail(word: &str, n: u32) -> Vec<u8> {
    let mut raw = format!(
        "From: Bob <bob@example.com>\r\nSubject: Message {n}\r\n\
         Date: Sat, 17 Oct 2026 {n:02}:00:00 +0000\r\n\
         Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
         --b\r\nContent-Type: text/plain\r\n\r\nThe word is {word}.\r\n\
         --b\r\nContent-Type: application/octet-stream; name=\"data.bin\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n"
    )
    .into_bytes();
    for _ in 0..20 {
        raw.extend_from_slice(&[b'A'; 76]);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"--b--\r\n");
    raw
}

/// INBOX with `inbox` and a Trash folder with one message (`trashed`, the 23rd).
fn state(inbox: &[Vec<u8>]) -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    for raw in inbox {
        state.inbox().append(raw, &[]);
    }
    let mut trash = FakeMailbox::new("Trash").with_attribute("\\Trash");
    trash.append(&mail("trashed", 23), &[]);
    state.mailboxes.push(trash);
    state
}

/// Prefetch as by default but for the whole-message limit, which `large_mail` is over,
/// and a short retry.
fn prefetching() -> BodyPrefetch {
    BodyPrefetch {
        whole_limit: 1024,
        retry: Duration::from_millis(10),
        ..BodyPrefetch::default()
    }
}

async fn env(state: ServerState, prefetch: BodyPrefetch) -> Env {
    env_built(state, SmtpState::new("alice", "pw"), |b| {
        b.prefetch_bodies(Some(prefetch))
    })
    .await
}

impl Env {
    /// [`mail`] `n` in `folder`.
    async fn id(&self, folder: &str, n: u32) -> MessageId {
        self.id_of(folder, &format!("Message {n}")).await
    }

    async fn downloaded(&self, folder: &str, n: u32) -> bool {
        let id = self.id(folder, n).await;
        let message = self.client.get_message(id).await.unwrap().unwrap();
        message.body.is_some()
    }

    fn paused_until(&self) -> Option<i64> {
        self.query("SELECT max(paused_until) FROM prefetch_usage")
    }

    fn failures(&self) -> i64 {
        self.query("SELECT count(*) FROM prefetch_failures")
    }

    async fn found(&self, text: &str) -> usize {
        self.client
            .search(SearchQuery::new(text))
            .await
            .unwrap()
            .len()
    }
}

/// Long enough for the prefetch to have gone on, if it were to.
async fn a_while() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bodies_are_fetched_once_every_folder_is_synced() {
    let env = env(
        state(&[mail("pelican", 1), large_mail("walrus", 2)]),
        prefetching(),
    )
    .await;
    // INBOX synced, the sync held at Trash: not yet.
    let gate = env.fake.state().hold_fetches();
    gate.add_permits(1);
    let syncing = tokio::spawn({
        let (client, account) = (env.client.clone(), env.account);
        async move { client.sync_account(account).await.unwrap() }
    });
    env.fake.wait_until(|st| st.header_fetches() == 2).await;
    a_while().await;
    assert_eq!(
        env.fake.state().body_fetches(),
        0,
        "not before every folder is synced"
    );

    gate.add_permits(100);
    syncing.await.unwrap();
    eventually("both bodies in INBOX", async || {
        env.downloaded("INBOX", 1).await && env.downloaded("INBOX", 2).await
    })
    .await;
    assert_eq!(env.found("pelican").await, 1);
    assert_eq!(env.found("walrus").await, 1);

    // The small one whole, so it opens offline with everything; of the large one only
    // the text, its attachment left on the server until opened.
    let start = env.fake.state().log.len();
    env.client
        .raw_message(env.id("INBOX", 1).await)
        .await
        .unwrap();
    assert_eq!(env.commands_since(start), Vec::<String>::new());
    let large = env
        .client
        .get_message(env.id("INBOX", 2).await)
        .await
        .unwrap()
        .unwrap();
    let attachments = &large.body.unwrap().attachments;
    assert_eq!(attachments.len(), 1);
    assert!(!attachments[0].downloaded);

    a_while().await;
    assert!(!env.downloaded("Trash", 23).await, "Trash is left out");
    let mut state = env.fake.state();
    assert!(state.log.iter().all(|c| !c.line.contains("BODY[")));
    assert!(state.log.iter().all(|c| !c.line.starts_with("SELECT")));
    assert!(
        state.inbox().messages.iter().all(|m| m.flags.is_empty()),
        "still unread"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn new_mail_is_fetched_as_it_arrives() {
    let env = env(state(&[mail("pelican", 1)]), prefetching()).await;
    env.client.sync_account(env.account).await.unwrap();
    eventually("the first body", async || env.downloaded("INBOX", 1).await).await;

    env.fake.state().inbox().append(&mail("heron", 2), &[]);
    env.client.sync_account(env.account).await.unwrap();
    eventually("the new body", async || env.found("heron").await == 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bodies_are_stored_and_announced_a_batch_at_a_time() {
    let inbox: Vec<_> = (1..=4).map(|n| mail("pelican", n)).collect();
    let env = env(
        state(&inbox),
        BodyPrefetch {
            batch: 2,
            ..prefetching()
        },
    )
    .await;
    let gate = env.fake.state().hold_body_fetches();
    env.client.sync_account(env.account).await.unwrap();
    let mut events = env.client.subscribe();
    gate.add_permits(100);
    eventually("every body in INBOX", async || {
        env.found("pelican").await == 4
    })
    .await;
    let inbox = env.folder("INBOX").await.id;
    let changed = drain(&mut events)
        .into_iter()
        .filter(|e| matches!(e, Event::MessagesChanged { folder, .. } if *folder == inbox))
        .count();
    assert_eq!(changed, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_newest_come_first_within_the_daily_budget() {
    let inbox: Vec<_> = (1..=3).map(|n| mail("pelican", n)).collect();
    let env = env(
        state(&inbox),
        BodyPrefetch {
            daily_budget: 1,
            gmail_daily_budget: 1,
            ..prefetching()
        },
    )
    .await;
    env.client.sync_account(env.account).await.unwrap();
    eventually("the newest body", async || env.downloaded("INBOX", 3).await).await;

    // Spent: new mail doesn't change that.
    env.fake.state().inbox().append(&mail("heron", 4), &[]);
    env.client.sync_account(env.account).await.unwrap();
    a_while().await;
    assert_eq!(env.fake.state().body_fetches(), 1);
    assert_eq!(env.found("pelican").await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_the_server_refuses_does_not_hold_up_the_rest() {
    let inbox: Vec<_> = (1..=3).map(|n| mail("pelican", n)).collect();
    let env = env(state(&inbox), prefetching()).await;
    env.fake.state().faults.refuse_body_fetch = vec![2];
    env.client.sync_account(env.account).await.unwrap();
    eventually("the others", async || {
        env.downloaded("INBOX", 3).await && env.downloaded("INBOX", 1).await
    })
    .await;
    assert_eq!(env.failures(), 1);

    // Not tried again for a while.
    a_while().await;
    assert_eq!(env.fake.state().body_fetches(), 3);

    // Opened, it is forgotten.
    env.fake.state().faults.refuse_body_fetch.clear();
    env.client
        .download_message(env.id("INBOX", 2).await)
        .await
        .unwrap();
    assert_eq!(env.failures(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn message_after_message_refused_pauses_the_prefetch() {
    let inbox: Vec<_> = (1..=4).map(|n| mail("pelican", n)).collect();
    let env = env(state(&inbox), prefetching()).await;
    env.fake.state().faults.refuse_body_fetch = vec![2, 3, 4];
    env.client.sync_account(env.account).await.unwrap();
    eventually("three refused", async || env.failures() == 3).await;
    a_while().await;
    assert!(!env.downloaded("INBOX", 1).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_connection_is_tried_again() {
    let inbox: Vec<_> = (1..=2).map(|n| mail("pelican", n)).collect();
    let env = env(state(&inbox), prefetching()).await;
    env.fake.state().faults.drop_on_body_fetch = Some(2);
    env.client.sync_account(env.account).await.unwrap();
    eventually("both bodies", async || env.found("pelican").await == 2).await;
    assert_eq!(
        env.failures(),
        0,
        "the connection's failure, not the message's"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuilt_folder_waits_for_the_next_sync() {
    let inbox: Vec<_> = (1..=2).map(|n| mail("pelican", n)).collect();
    let env = env(state(&inbox), prefetching()).await;
    let gate = env.fake.state().hold_body_fetches();
    env.client.sync_account(env.account).await.unwrap();
    // The first body is being fetched; the folder is rebuilt before the next.
    env.fake.wait_until(|st| st.body_fetches() == 1).await;
    env.fake.state().inbox().reset_uidvalidity(99);
    env.fake.state().drop_open_connections();
    gate.add_permits(100);
    a_while().await;
    assert_eq!(env.failures(), 0);
    let (fetched, examined) = {
        let state = env.fake.state();
        (state.body_fetches(), state.received("EXAMINE"))
    };
    // Not tried over and over meanwhile.
    a_while().await;
    assert_eq!(env.fake.state().received("EXAMINE"), examined);

    // The sync starts it over, with new messages; they are fetched.
    env.client.sync_account(env.account).await.unwrap();
    eventually("both bodies again", async || {
        env.found("pelican").await == 2
    })
    .await;
    assert!(env.fake.state().body_fetches() > fetched);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_login_waits_for_a_sync_that_gets_through() {
    let inbox: Vec<_> = (1..=2).map(|n| mail("pelican", n)).collect();
    let env = env(state(&inbox), prefetching()).await;
    let gate = env.fake.state().hold_body_fetches();
    env.client.sync_account(env.account).await.unwrap();
    env.fake.wait_until(|st| st.body_fetches() == 1).await;
    // The password changed on the server meanwhile.
    {
        let mut state = env.fake.state();
        state.faults.reject_login = true;
        state.drop_open_connections();
    }
    gate.add_permits(100);
    env.fake.wait_until(|st| st.received("LOGIN") >= 3).await;
    let logins = env.fake.state().received("LOGIN");
    a_while().await;
    assert_eq!(
        env.fake.state().received("LOGIN"),
        logins,
        "no more rejected logins"
    );

    env.fake.state().faults.reject_login = false;
    env.client.sync_account(env.account).await.unwrap();
    eventually("both bodies", async || env.found("pelican").await == 2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gmail_has_a_budget_of_its_own() {
    let mut state = ServerState::gmail("alice", "pw");
    let all = state.all_mail();
    for n in 1..=3 {
        let uid = all.append(&mail("pelican", n), &[]);
        all.set_labels(uid, &["\\Inbox"]);
    }
    let env = env(
        state,
        BodyPrefetch {
            daily_budget: 1,
            ..prefetching()
        },
    )
    .await;
    env.client.sync_account(env.account).await.unwrap();
    eventually("every body", async || env.found("pelican").await == 3).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn going_over_the_servers_limit_stops_it_for_a_day() {
    let inbox: Vec<_> = (1..=3).map(|n| mail("pelican", n)).collect();
    let mut env = env(state(&inbox), prefetching()).await;
    env.fake.state().faults.body_fetch_refusal = Some("Fetch volume limit exceed".into());
    env.client.sync_account(env.account).await.unwrap();
    eventually("the pause", async || env.paused_until().is_some()).await;
    a_while().await;
    assert_eq!(env.fake.state().body_fetches(), 1, "not tried again");
    assert_eq!(env.failures(), 0, "the account's, not the messages'");

    // Nor after a restart, though the server would hand them out again.
    env.fake.state().faults.body_fetch_refusal = None;
    drop(env.client);
    env.fake.wait_until(|st| st.open_connections == 0).await;
    env.client = MailClient::builder(Config::with_data_dir(env.dir.path()))
        .runtime(Handle::current())
        .secrets(Arc::clone(&env.secrets) as _)
        .prefetch_bodies(Some(prefetching()))
        .open()
        .unwrap();
    env.client.sync_account(env.account).await.unwrap();
    a_while().await;
    assert_eq!(env.fake.state().body_fetches(), 1);
}
