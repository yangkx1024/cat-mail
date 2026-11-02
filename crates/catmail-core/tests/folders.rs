//! Creating, renaming and deleting folders (Gmail's labels) against the fake server:
//! made on the server at once, then stored as a sync would.

use std::sync::Arc;
use std::time::Duration;

use catmail_core::{Error, Event, Flags, FolderKind, MailClient};
use catmail_testkit::{FakeMailbox, ServerState};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

mod common;

use common::{Env, drain, eventually, synced_env};

/// INBOX with "hello", and Projects with "plan".
fn state() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    state.inbox().append_simple("hello");
    let mut projects = FakeMailbox::new("Projects");
    projects.append_simple("plan");
    state.mailboxes.push(projects);
    state
}

impl Env {
    /// The server's mailboxes, by wire name.
    fn mailboxes(&self) -> Vec<String> {
        self.fake.state().mailbox_names()
    }

    /// The account's folders, by display name.
    async fn folder_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .client
            .list_folders(self.account)
            .await
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        names.sort();
        names
    }

    /// Starts `f` with the server holding its answer to `command`, and waits until the
    /// server has it: what follows happens while the server is at it, until
    /// [`Held::finish`].
    async fn while_held<F>(&self, command: &str, f: impl FnOnce(MailClient) -> F) -> Held<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send,
    {
        let gate = self.fake.state().hold(command);
        let task = tokio::spawn(f(self.client.clone()));
        eventually(&format!("{command} sent"), async || {
            self.fake.state().received(command) > 0
        })
        .await;
        Held { task, gate }
    }
}

/// A call the server is holding the answer to a command of, from [`Env::while_held`].
struct Held<T> {
    task: JoinHandle<T>,
    gate: Arc<Semaphore>,
}

impl<T> Held<T> {
    /// Lets the server answer, and waits for the call to finish.
    async fn finish(self) -> T {
        self.gate.add_permits(Semaphore::MAX_PERMITS);
        self.task.await.unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn folders_are_created_renamed_and_deleted_on_the_server() {
    let env = synced_env(state()).await;
    let mut events = env.client.subscribe();

    let clients = env
        .client
        .create_folder(env.account, None, " Clients ")
        .await
        .unwrap();
    assert_eq!(clients.name, "Clients");
    let acme = env
        .client
        .create_folder(env.account, Some(clients.id), "客户")
        .await
        .unwrap();
    assert_eq!(acme.name, "Clients/客户");
    assert_eq!(
        env.mailboxes(),
        ["INBOX", "Projects", "Clients", "Clients/&W6JiNw-"]
    );
    assert!(env.fake.state().subscriptions.contains("Clients/&W6JiNw-"));
    assert!(drain(&mut events).contains(&Event::FoldersChanged {
        account: env.account
    }));

    // A message in the subfolder stays the same message through the rename.
    env.fake
        .state()
        .mailbox_mut("Clients/&W6JiNw-")
        .append_simple("contract");
    env.client.sync_account(env.account).await.unwrap();
    let contract = env.id_of("Clients/客户", "contract").await;
    let fetched = env.fake.state().header_fetches();

    let customers = env
        .client
        .rename_folder(clients.id, "Customers")
        .await
        .unwrap();
    assert_eq!(
        (customers.id, customers.name.as_str()),
        (clients.id, "Customers")
    );
    assert_eq!(
        env.mailboxes(),
        ["INBOX", "Projects", "Customers", "Customers/&W6JiNw-"]
    );
    assert_eq!(
        env.folder_names().await,
        ["Customers", "Customers/客户", "INBOX", "Projects"]
    );
    assert_eq!(env.folder("Customers/客户").await.id, acme.id);
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.id_of("Customers/客户", "contract").await, contract);
    assert_eq!(
        env.fake.state().header_fetches(),
        fetched,
        "nothing fetched again"
    );

    drain(&mut events);
    env.client.delete_folder(customers.id).await.unwrap();
    assert_eq!(env.mailboxes(), ["INBOX", "Projects"]);
    assert_eq!(env.folder_names().await, ["INBOX", "Projects"]);
    let subscriptions = env.fake.state().subscriptions.clone();
    assert!(
        !subscriptions.iter().any(|s| s.starts_with("Customers")),
        "{subscriptions:?}"
    );
    assert!(drain(&mut events).contains(&Event::FoldersChanged {
        account: env.account
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn special_places_bad_names_and_taken_ones_are_refused() {
    let mut state = state();
    state
        .mailboxes
        .push(FakeMailbox::new("Leaf").with_attribute("\\Noinferiors"));
    let env = synced_env(state).await;
    let start = env.fake.state().log.len();
    let inbox = env.folder("INBOX").await;
    let projects = env.folder("Projects").await;
    let leaf = env.folder("Leaf").await;
    assert!(!leaf.inferiors);

    let refused = [
        env.client.rename_folder(inbox.id, "Mail").await.err(),
        env.client.delete_folder(inbox.id).await.err(),
        // Not in a special folder, nor where the server allows none.
        env.client
            .create_folder(env.account, Some(inbox.id), "Receipts")
            .await
            .err(),
        env.client
            .create_folder(env.account, Some(leaf.id), "Deeper")
            .await
            .err(),
        env.client
            .create_folder(env.account, None, "a/b")
            .await
            .err(),
        env.client.rename_folder(projects.id, "  ").await.err(),
    ];
    for err in refused {
        assert!(matches!(err, Some(Error::InvalidInput(_))), "{err:?}");
    }
    for taken in ["Projects", "inbox"] {
        let err = env
            .client
            .create_folder(env.account, None, taken)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::AlreadyExists(_)), "{err:?}");
    }
    assert!(
        env.commands_since(start).is_empty(),
        "nothing asked of the server"
    );
    // The server's refusal comes back as it is.
    env.fake.state().mailboxes.push(FakeMailbox::new("Later"));
    let err = env
        .client
        .create_folder(env.account, None, "Later")
        .await
        .unwrap_err();
    assert!(err.is_refusal(), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn new_folders_go_under_inbox_on_a_server_that_keeps_them_there() {
    let mut state = ServerState::new("alice", "pw");
    state
        .mailboxes
        .push(FakeMailbox::new("INBOX/Sent").with_attribute("\\Sent"));
    let env = synced_env(state).await;
    let folder = env
        .client
        .create_folder(env.account, None, "Work")
        .await
        .unwrap();
    assert_eq!(folder.name, "INBOX/Work");
    assert_eq!(env.mailboxes(), ["INBOX", "INBOX/Sent", "INBOX/Work"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn queued_changes_follow_a_renamed_folder() {
    let env = synced_env(state()).await;
    let plan = env.id_of("Projects", "plan").await;
    env.client
        .set_flags(vec![plan], Flags::FLAGGED, Flags::empty())
        .await
        .unwrap();
    let projects = env.folder("Projects").await;
    env.client.rename_folder(projects.id, "Work").await.unwrap();

    assert_eq!(env.replay().await.applied, 1);
    let flags = env.fake.state().mailbox_mut("Work").messages[0]
        .flags
        .clone();
    assert!(flags.iter().any(|f| f == "\\Flagged"), "{flags:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_moved_out_reach_their_folder_before_the_old_one_goes() {
    let env = synced_env(state()).await;
    let plan = env.id_of("Projects", "plan").await;
    let inbox = env.folder("INBOX").await;
    let projects = env.folder("Projects").await;
    env.client
        .move_messages(vec![plan], Some(projects.id), inbox.id)
        .await
        .unwrap();

    env.client.delete_folder(projects.id).await.unwrap();
    assert_eq!(env.mailboxes(), ["INBOX"]);
    let mut subjects = env.subjects("INBOX").await;
    subjects.sort();
    assert_eq!(subjects, ["hello", "plan"]);
    assert_eq!(env.fake.state().inbox().messages.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_whose_answer_was_lost_is_not_made_twice() {
    let env = synced_env(state()).await;
    // A pooled connection, for the next calls to find dead and try again on a new one.
    let clients = env
        .client
        .create_folder(env.account, None, "Clients")
        .await
        .unwrap();

    env.fake.state().faults.drop_after_mailbox_change = true;
    let renamed = env
        .client
        .rename_folder(clients.id, "Customers")
        .await
        .unwrap();
    assert_eq!(renamed.id, clients.id);
    assert_eq!(env.mailboxes(), ["INBOX", "Projects", "Customers"]);

    env.fake.state().faults.drop_after_mailbox_change = true;
    let other = env
        .client
        .create_folder(env.account, None, "Other")
        .await
        .unwrap();
    assert_eq!(other.name, "Other");

    env.fake.state().faults.drop_after_mailbox_change = true;
    env.client.delete_folder(renamed.id).await.unwrap();
    assert_eq!(env.mailboxes(), ["INBOX", "Projects", "Other"]);
    assert_eq!(env.folder_names().await, ["INBOX", "Other", "Projects"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn gmail_labels_are_created_renamed_and_deleted() {
    let mut state = ServerState::gmail("alice", "pw");
    let all = state.all_mail();
    let one = all.append_simple("one");
    all.set_labels(one, &["\\Inbox"]);
    let env = synced_env(state).await;
    let one = env.id_of("INBOX", "one").await;

    let travel = env
        .client
        .create_folder(env.account, None, "Travel")
        .await
        .unwrap();
    assert_eq!(travel.kind, FolderKind::Label);
    env.client
        .label_messages(vec![one], vec![travel.id], Vec::new())
        .await
        .unwrap();
    env.replay().await;

    let trips = env.client.rename_folder(travel.id, "Trips").await.unwrap();
    assert_eq!(trips.id, travel.id);
    let labels = env.fake.state().all_mail().messages[0].labels.clone();
    assert_eq!(labels, ["\\Inbox", "Trips"]);
    let inbox = env.folder("INBOX").await;
    env.client.sync_account(env.account).await.unwrap();
    assert_eq!(env.list("INBOX").await[0].labels, [inbox.id, trips.id]);

    let mut events = env.client.subscribe();
    env.client.delete_folder(trips.id).await.unwrap();
    assert_eq!(env.fake.state().all_mail().messages[0].labels, ["\\Inbox"]);
    assert_eq!(env.list("INBOX").await[0].labels, [inbox.id]);
    assert!(drain(&mut events).contains(&Event::MessagesChanged {
        account: env.account,
        folder: inbox.id,
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_move_out_keeps_the_folder() {
    let env = synced_env(state()).await;
    let plan = env.id_of("Projects", "plan").await;
    let (inbox, projects) = (env.folder("INBOX").await, env.folder("Projects").await);
    env.client
        .move_messages(vec![plan], Some(projects.id), inbox.id)
        .await
        .unwrap();
    env.fake.state().faults.refuse_next_changes = 1;

    let err = env.client.delete_folder(projects.id).await.unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err:?}");
    // The message is back where the server has it, and stays there.
    assert_eq!(env.subjects("Projects").await, ["plan"]);
    assert_eq!(env.fake.state().mailbox_mut("Projects").messages.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_of_a_folder_being_deleted_stay_put() {
    let env = synced_env(state()).await;
    let plan = env.id_of("Projects", "plan").await;
    let (inbox, projects) = (env.folder("INBOX").await, env.folder("Projects").await);

    let deleting = env
        .while_held("DELETE", move |client| async move {
            client.delete_folder(projects.id).await
        })
        .await;
    // Moved out while the server deletes it, it would go with it.
    let err = env
        .client
        .move_messages(vec![plan], Some(projects.id), inbox.id)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err:?}");
    deleting.finish().await.unwrap();
    assert_eq!(env.subjects("INBOX").await, ["hello"]);
    let pending = env.client.list_pending(Some(env.account)).await.unwrap();
    assert!(pending.is_empty(), "{pending:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_carrying_a_label_being_deleted_still_change() {
    let mut state = ServerState::gmail("alice", "pw");
    let all = state.all_mail();
    for subject in ["one", "two"] {
        let uid = all.append_simple(subject);
        all.set_labels(uid, &["\\Inbox", "Travel"]);
    }
    state.mailboxes.push(FakeMailbox::new("Travel"));
    let env = synced_env(state).await;
    let (one, two) = (
        env.id_of("INBOX", "one").await,
        env.id_of("INBOX", "two").await,
    );
    let travel = env.folder("Travel").await;
    let inbox = env.folder("INBOX").await;

    let deleting = env
        .while_held("DELETE", move |client| async move {
            client.delete_folder(travel.id).await
        })
        .await;
    // Starred in All Mail: the label only lists it, and stays off it.
    env.client
        .set_flags(vec![one], Flags::FLAGGED, Flags::empty())
        .await
        .unwrap();
    // Taken off, the label would be changed after it is gone.
    let err = env
        .client
        .label_messages(vec![two], Vec::new(), vec![travel.id])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err:?}");
    deleting.finish().await.unwrap();
    env.replay().await;
    let messages = env.fake.state().all_mail().messages.clone();
    assert!(messages[0].flags.iter().any(|f| f == "\\Flagged"));
    assert!(messages.iter().all(|m| m.labels == ["\\Inbox"]));
    assert_eq!(env.list("INBOX").await[0].labels, [inbox.id]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_made_while_its_parent_is_renamed_goes_into_the_renamed_one() {
    let env = synced_env(state()).await;
    let projects = env.folder("Projects").await;

    let renaming = env
        .while_held("RENAME", move |client| async move {
            client.rename_folder(projects.id, "Work").await
        })
        .await;
    let making = tokio::spawn({
        let (client, account) = (env.client.clone(), env.account);
        async move {
            client
                .create_folder(account, Some(projects.id), "2026")
                .await
        }
    });
    // It waits for the rename; given the time to start waiting, it is made after.
    tokio::time::sleep(Duration::from_millis(100)).await;
    renaming.finish().await.unwrap();
    let made = making.await.unwrap().unwrap();
    assert_eq!(made.name, "Work/2026");
    assert_eq!(env.mailboxes(), ["INBOX", "Work", "Work/2026"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn folders_another_client_made_inside_go_too() {
    let env = synced_env(state()).await;
    let projects = env.folder("Projects").await;
    env.fake
        .state()
        .mailboxes
        .push(FakeMailbox::new("Projects/Later"));

    env.client.delete_folder(projects.id).await.unwrap();
    assert_eq!(env.mailboxes(), ["INBOX"]);
    assert_eq!(env.folder_names().await, ["INBOX"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_is_checked_against_its_parents_delimiter() {
    let mut state = state();
    let mut public = FakeMailbox::new("Public");
    public.delimiter = '.';
    state.mailboxes.push(public);
    let env = synced_env(state).await;
    let public = env.folder("Public").await;

    let err = env
        .client
        .create_folder(env.account, Some(public.id), "a.b")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
    let made = env
        .client
        .create_folder(env.account, Some(public.id), "a/b")
        .await
        .unwrap();
    assert_eq!(made.name, "Public.a/b");
}
