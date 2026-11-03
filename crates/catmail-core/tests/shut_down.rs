//! Shutting the client down, whoever still holds it.

use std::sync::Arc;
use std::time::Duration;

use catmail_core::{Config, Error, MailClient, MemorySecrets, NewAccount, Verify};
use catmail_testkit::{FakeImap, ServerState};
use tokio::sync::broadcast::error::RecvError;

mod common;

use common::{env, local_server};

#[tokio::test(flavor = "multi_thread")]
async fn shutting_down_a_client_on_its_own_runtime_stops_the_work_under_way() {
    let mut state = ServerState::new("alice", "pw");
    state.inbox().append_simple("Lunch?");
    let fake = FakeImap::start(state).await;
    let dir = tempfile::tempdir().unwrap();
    // No runtime given: the client makes its own, as in the GUI.
    let client = MailClient::builder(Config::with_data_dir(dir.path()))
        .secrets(Arc::new(MemorySecrets::default()))
        .open()
        .unwrap();
    let new = NewAccount {
        email: "alice@example.com".into(),
        display_name: None,
        username: "alice".into(),
        imap: local_server(fake.port()),
        smtp: local_server(fake.port()),
    };
    let account = client
        .add_account(new, "pw".into(), Verify::Skip)
        .await
        .unwrap()
        .id;
    let _gate = fake.state().hold("EXAMINE INBOX");
    let syncing = tokio::spawn({
        let client = client.clone();
        async move { client.sync_account(account).await }
    });
    fake.wait_until(|st| st.received("EXAMINE INBOX") > 0).await;

    client.shut_down();
    let synced = syncing.await.unwrap();
    assert!(matches!(synced, Err(Error::Closed)), "{synced:?}");
    // The server never answered.
    tokio::time::timeout(Duration::from_secs(5), client.closed())
        .await
        .expect("closed with the sync held");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutting_down_closes_the_engine_for_every_handle_once_its_work_is_done() {
    let mut state = ServerState::new("alice", "pw");
    state.inbox().append_simple("Lunch?");
    let env = env(state).await;
    let account = env.account;
    let mut events = env.client.subscribe();
    let closed = env.client.closed();

    let held = env
        .while_held("EXAMINE INBOX", move |client| async move {
            client.sync_account(account).await
        })
        .await;
    let other = env.client.clone();
    env.client.shut_down();
    assert!(matches!(other.list_accounts().await, Err(Error::Closed)));
    // Not even what only writes to the data directory, which may be another engine's now.
    let handed_off = other.hand_off("eml", b"Lunch?".to_vec()).await;
    assert!(matches!(handed_off, Err(Error::Closed)), "{handed_off:?}");
    assert!(matches!(
        other.subscribe().recv().await,
        Err(RecvError::Closed)
    ));

    // On the borrowed runtime, the sync under way finishes, then the engine closes.
    tokio::pin!(closed);
    let open = tokio::time::timeout(Duration::from_millis(100), &mut closed).await;
    assert!(open.is_err(), "closed while a sync still held it");
    held.finish().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), closed)
        .await
        .expect("closed once the sync was done");
    while !matches!(events.recv().await, Err(RecvError::Closed)) {}

    // The data directory is free again.
    Config::with_data_dir(env.dir.path()).lock().unwrap();
}
