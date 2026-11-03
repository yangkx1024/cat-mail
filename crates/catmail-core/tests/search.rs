//! Searching the servers for the text of messages whose bodies aren't downloaded, against
//! the fake IMAP server.

use catmail_core::{MessageId, SearchQuery};
use catmail_testkit::{FakeMailbox, ServerState};
use tokio::sync::Semaphore;

mod common;

use common::{Env, synced_env as env};

/// A plain message, dated by `minute`.
fn mail(subject: &str, minute: u32, body: &str) -> Vec<u8> {
    format!(
        "From: Bob <bob@example.com>\r\nTo: alice@example.com\r\nSubject: {subject}\r\n\
         Date: Sat, 17 Oct 2026 10:{minute:02}:00 +0800\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// INBOX with "Quarterly" (about pelicans), "Report CN" (in Chinese) and "Lunch";
/// Archive with "Old" (about pelicans too).
fn mailboxes() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    let inbox = state.inbox();
    inbox.append(&mail("Quarterly", 1, "The pelican numbers are final."), &[]);
    inbox.append(&mail("Report CN", 2, "请查收第三季度报告。"), &[]);
    inbox.append(&mail("Lunch", 3, "Noon at the usual place."), &[]);
    let mut archive = FakeMailbox::new("Archive");
    archive.append(&mail("Old", 0, "A pelican, archived."), &[]);
    state.mailboxes.push(archive);
    state
}

impl Env {
    /// What the server finds for `text`, then the search with it, by subject.
    async fn search_both(&self, mut query: SearchQuery) -> Vec<String> {
        query.found_on_server = self.client.search_server(query.clone()).await.unwrap();
        self.client
            .search(query)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.subject.unwrap_or_default())
            .collect()
    }

    fn log_len(&self) -> usize {
        self.fake.state().log.len()
    }

    fn connections(&self) -> usize {
        self.fake.state().connections
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_finds_what_local_search_cant() {
    let env = env(mailboxes()).await;
    let pelican = SearchQuery::new("pelican");
    assert!(env.client.search(pelican.clone()).await.unwrap().is_empty());

    let start = env.log_len();
    assert_eq!(env.search_both(pelican.clone()).await, ["Quarterly", "Old"]);
    assert_eq!(env.examined(start), ["INBOX", "Archive"]);
    // Reading never changes the server.
    assert!(
        !env.commands_since(start)
            .iter()
            .any(|c| c.starts_with("SELECT"))
    );
    assert_eq!(
        env.search_both(SearchQuery::new("季度报告")).await,
        ["Report CN"]
    );
    assert!(
        env.searches(start)
            .iter()
            .any(|s| s.contains("CHARSET UTF-8"))
    );

    // A message whose body is here is found locally, not again.
    let quarterly = env.id_of("INBOX", "Quarterly").await;
    env.client.download_message(quarterly).await.unwrap();
    let old = env.id_of("Archive", "Old").await;
    assert_eq!(
        env.client.search_server(pelican.clone()).await.unwrap(),
        [old]
    );
    assert_eq!(env.search_both(pelican).await, ["Quarterly", "Old"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_folder_searched_is_asked_about() {
    let env = env(mailboxes()).await;
    let mut query = SearchQuery::new("pelican");
    query.folder = Some(env.folder("Archive").await.id);
    let start = env.log_len();
    assert_eq!(env.search_both(query).await, ["Old"]);
    assert_eq!(env.examined(start), ["Archive"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_not_synced_yet_are_left_to_sync() {
    let env = env(mailboxes()).await;
    env.fake
        .state()
        .inbox()
        .append(&mail("New", 9, "Another pelican."), &[]);
    assert_eq!(
        env.search_both(SearchQuery::new("pelican")).await,
        ["Quarterly", "Old"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_refusing_the_charset_adds_nothing() {
    let env = env(mailboxes()).await;
    env.fake.state().faults.refuse_search_charset = true;
    let start = env.log_len();
    let found = env.client.search_server(SearchQuery::new("季度报告")).await;
    assert_eq!(found.unwrap(), []);
    // It would in every folder.
    assert_eq!(env.searches(start).len(), 1);
    // ASCII needs no charset.
    assert_eq!(
        env.search_both(SearchQuery::new("pelican")).await,
        ["Quarterly", "Old"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_out_of_reach_adds_nothing() {
    let env = env(mailboxes()).await;
    env.fake.state().faults.refuse_connections = usize::MAX;
    let found = env.client.search_server(SearchQuery::new("pelican")).await;
    assert_eq!(found.unwrap(), []);
}

#[tokio::test(flavor = "multi_thread")]
async fn searches_share_a_connection_of_their_own() {
    let env = env(mailboxes()).await;
    let before = env.connections();
    for _ in 0..2 {
        assert_eq!(
            env.search_both(SearchQuery::new("pelican")).await,
            ["Quarterly", "Old"]
        );
    }
    assert_eq!(env.connections(), before + 1);
    // Opening a message doesn't wait for it, nor take it.
    let quarterly = env.id_of("INBOX", "Quarterly").await;
    env.client.download_message(quarterly).await.unwrap();
    assert_eq!(env.connections(), before + 2);
    env.search_both(SearchQuery::new("pelican")).await;
    assert_eq!(env.connections(), before + 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_search_given_up_on_stops_and_leaves_its_connection_to_the_next() {
    // Given up on while INBOX's text is searched it goes no further; while INBOX opens,
    // it doesn't search its text either, which may scan the whole folder.
    for (held, pelican_searches) in [("UID SEARCH", 1), ("EXAMINE", 0)] {
        let env = env(mailboxes()).await;
        let (gate, before) = {
            let mut st = env.fake.state();
            (st.hold(held), st.received(held))
        };
        let (connections, start) = (env.connections(), env.log_len());
        let first = tokio::spawn({
            let client = env.client.clone();
            async move { client.search_server(SearchQuery::new("pelican")).await }
        });
        env.fake.wait_until(|st| st.received(held) > before).await;
        // As the GUI does when the text changes.
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());

        let next = tokio::spawn({
            let client = env.client.clone();
            async move { client.search_server(SearchQuery::new("lunch")).await }
        });
        gate.add_permits(Semaphore::MAX_PERMITS);
        let lunch = env.id_of("INBOX", "Lunch").await;
        assert_eq!(next.await.unwrap().unwrap(), [lunch]);
        let searches = env.searches(start);
        let pelican = searches.iter().filter(|s| s.contains("pelican")).count();
        assert_eq!(pelican, pelican_searches, "held at {held}: {searches:?}");
        // The next one went on the same connection.
        assert_eq!(env.connections(), connections + 1, "held at {held}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_refusing_the_login_isnt_asked_again_until_a_sync_gets_through() {
    for refusal in ["reject", "over limit"] {
        let env = env(mailboxes()).await;
        let set = |on: bool| {
            let mut st = env.fake.state();
            match refusal {
                "reject" => st.faults.reject_login = on,
                _ => st.faults.login_refusal = on.then(|| common::GMAIL_OVER_LIMIT.into()),
            }
        };
        set(true);
        let logins = || env.fake.state().received("LOGIN");
        let pelican = SearchQuery::new("pelican");
        assert_eq!(env.client.search_server(pelican.clone()).await.unwrap(), []);
        let tried = logins();
        // Sync hasn't seen it, and each try would count against the account.
        assert_eq!(env.client.search_server(pelican.clone()).await.unwrap(), []);
        assert_eq!(logins(), tried, "{refusal}");

        set(false);
        env.client.sync_account(env.account).await.unwrap();
        assert_eq!(
            env.search_both(pelican).await,
            ["Quarterly", "Old"],
            "{refusal}"
        );
    }
}

const ALL_MAIL: &str = "[Gmail]/All Mail";

#[tokio::test(flavor = "multi_thread")]
async fn gmail_is_searched_where_messages_are_in_its_own_syntax() {
    let mut state = ServerState::gmail("alice", "pw");
    state.mailboxes.push(FakeMailbox::new("Work"));
    let all = state.all_mail();
    let work = all.append(&mail("Quarterly", 1, "The pelican numbers."), &[]);
    all.set_labels(work, &["\\Inbox", "Work"]);
    all.append(&mail("Lunch", 2, "Noon."), &[]);
    state
        .mailbox_mut("[Gmail]/Trash")
        .append(&mail("Old", 0, "A pelican, deleted."), &[]);
    let env = env(state).await;

    let start = env.log_len();
    assert_eq!(
        env.search_both(SearchQuery::new("pelican")).await,
        ["Quarterly", "Old"]
    );
    // Not Spam, empty: nothing there for the server to find.
    assert_eq!(env.examined(start), [ALL_MAIL, "[Gmail]/Trash"]);
    let searches = env.searches(start);
    assert!(
        searches.iter().all(|s| s.contains("X-GM-RAW")),
        "{searches:?}"
    );

    // In a label: All Mail, which its messages are in.
    let mut query = SearchQuery::new("pelican");
    query.folder = Some(env.folder("Work").await.id);
    let start = env.log_len();
    assert_eq!(env.search_both(query).await, ["Quarterly"]);
    assert_eq!(env.examined(start), [ALL_MAIL]);
    let found: Vec<MessageId> = env
        .client
        .search_server(SearchQuery::new("from:bob noon"))
        .await
        .unwrap();
    assert_eq!(found, [env.id_of(ALL_MAIL, "Lunch").await]);
}
