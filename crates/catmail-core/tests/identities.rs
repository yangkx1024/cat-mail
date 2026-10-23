//! Sending as identities: the addresses of an account, each with an SMTP server and
//! login of its own (the one made with the account, and others), against the fake
//! servers.

mod common;

use catmail_core::{
    Address, DraftContent, Error, ErrorKind, Event, Identity, NewIdentity, OutboxStatus, Verify,
};
use catmail_testkit::{FakeMailbox, FakeSmtp, ServerState, SmtpState};
use common::{Env, drain, env, local_server, sparse};
use mail_parser::MessageParser;

/// An INBOX, a Sent and a Drafts folder.
fn state() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    state.inbox().append_simple("hello");
    for (name, attribute) in [("Sent", "\\Sent"), ("Drafts", "\\Drafts")] {
        state
            .mailboxes
            .push(FakeMailbox::new(name).with_attribute(attribute));
    }
    state
}

/// A sending service: logs in as `resend` with an API key.
async fn service() -> FakeSmtp {
    FakeSmtp::start(SmtpState::new("resend", "api-key")).await
}

fn new_identity(server: &FakeSmtp) -> NewIdentity {
    NewIdentity {
        email: "news@example.org".into(),
        display_name: Some("Newsletter".into()),
        username: "resend".into(),
        smtp: local_server(server.port()),
    }
}

async fn add_identity(env: &Env, server: &FakeSmtp) -> Identity {
    env.client
        .add_identity(
            env.account,
            new_identity(server),
            "api-key".into(),
            Verify::Skip,
        )
        .await
        .unwrap()
}

fn content(subject: &str, identity: Option<&Identity>) -> DraftContent {
    DraftContent {
        subject: subject.into(),
        to: vec![Address {
            name: None,
            email: "bob@example.com".into(),
        }],
        text: "hi\n".into(),
        identity: identity.map(|i| i.id),
        ..Default::default()
    }
}

impl Env {
    /// The identity made with the account, while it is the default.
    async fn own_identity(&self) -> Identity {
        let identities = self.client.list_identities(self.account).await.unwrap();
        assert!(identities[0].is_default);
        identities[0].clone()
    }

    async fn queue_as(&self, subject: &str, identity: Option<&Identity>) {
        let draft = self
            .client
            .create_draft(self.account, content(subject, identity))
            .await
            .unwrap();
        self.client.send_draft(draft.id).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_as_an_identity_through_its_own_server() {
    let env = env(state()).await;
    let resend = service().await;
    let mut events = env.client.subscribe();
    let identity = add_identity(&env, &resend).await;
    assert!(drain(&mut events).contains(&Event::IdentitiesChanged {
        account: env.account
    }));
    let own = env.own_identity().await;
    assert_eq!(own.email, "alice@example.com");
    let overview = env.client.overview().await.unwrap();
    assert_eq!(overview[0].identities, [own.clone(), identity.clone()]);

    env.queue_as("from the account", None).await;
    env.queue_as("from the newsletter", Some(&identity)).await;
    let outbox = env.client.list_outbox(Some(env.account)).await.unwrap();
    assert_eq!(outbox[0].identity, own.id, "the default");
    assert_eq!(outbox[1].identity, identity.id);
    assert!(outbox[1].message_id.ends_with("@example.org"), "its domain");
    // One pass, through both servers.
    assert_eq!(
        env.flush().await,
        [(OutboxStatus::Done, 1), (OutboxStatus::Done, 1)]
    );

    let own = env.smtp.state().received.clone();
    let theirs = resend.state().received.clone();
    assert_eq!((own.len(), theirs.len()), (1, 1));
    assert_eq!(own[0].from, "alice@example.com");
    assert_eq!(theirs[0].from, "news@example.org");
    let m = MessageParser::new().parse(&theirs[0].data).unwrap();
    assert_eq!(m.subject(), Some("from the newsletter"));
    let from = m.from().unwrap().first().unwrap();
    assert_eq!(
        (from.name(), from.address()),
        (Some("Newsletter"), Some("news@example.org"))
    );
    // Saved where the account keeps what it sent.
    let mut state = env.fake.state();
    assert_eq!(state.mailbox_mut("Sent").messages.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_copy_in_drafts_is_from_the_identity() {
    let env = env(state()).await;
    let resend = service().await;
    let identity = add_identity(&env, &resend).await;
    env.client.sync_account(env.account).await.unwrap();
    env.client
        .create_draft(env.account, content("draft", Some(&identity)))
        .await
        .unwrap();
    env.client.replay_pending(env.account).await.unwrap();

    let mut state = env.fake.state();
    let drafts = &state.mailbox_mut("Drafts").messages;
    assert_eq!(drafts.len(), 1);
    let m = MessageParser::new().parse(&drafts[0].raw).unwrap();
    let from = m.from().unwrap().first().unwrap();
    assert_eq!(from.address(), Some("news@example.org"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_identity_that_cant_log_in_holds_back_only_its_own_messages() {
    let env = env(state()).await;
    let resend = service().await;
    resend.state().faults.reject_login = true;
    let identity = add_identity(&env, &resend).await;
    env.queue_as("blocked", Some(&identity)).await;
    env.queue_as("through", None).await;

    // Another that is down for now.
    let down = service().await;
    down.state().faults.unavailable = 1;
    let other = env
        .client
        .add_identity(
            env.account,
            NewIdentity {
                email: "other@example.org".into(),
                ..new_identity(&down)
            },
            "api-key".into(),
            Verify::Skip,
        )
        .await
        .unwrap();
    env.queue_as("later", Some(&other)).await;

    let report = env.client.flush_outbox(env.account).await.unwrap();
    let statuses: Vec<_> = report.entries.iter().map(|e| e.status).collect();
    assert_eq!(
        statuses,
        [
            OutboxStatus::Queued,
            OutboxStatus::Done,
            OutboxStatus::Queued
        ]
    );
    // Each failure kept with its own identity: one to fix, one to wait out.
    let kinds: Vec<_> = report
        .errors
        .iter()
        .map(|(id, e)| (*id, e.kind, e.transient))
        .collect();
    assert_eq!(kinds.len(), 2, "{kinds:?}");
    assert_eq!(kinds[0], (identity.id, ErrorKind::Auth, false));
    assert_eq!((kinds[1].0, kinds[1].2), (other.id, true));
    let status = env.client.account_status(env.account);
    assert_eq!(status.send.len(), 2, "the account's own server is fine");
    let problem = &status.send[&identity.id];
    assert_eq!(problem.error.kind, ErrorKind::Auth);
    assert_eq!(status.send_problem(), Some((identity.id, problem)));

    resend.state().faults.reject_login = false;
    env.client.retry_outbox(report.entries[2].id).await.unwrap();
    assert_eq!(
        env.flush().await,
        [(OutboxStatus::Done, 1), (OutboxStatus::Done, 1)]
    );
    assert!(env.client.account_status(env.account).send.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_an_identity_hands_its_drafts_to_the_default() {
    let env = env(state()).await;
    let resend = service().await;
    let identity = add_identity(&env, &resend).await;
    let draft = env
        .client
        .create_draft(env.account, content("draft", Some(&identity)))
        .await
        .unwrap();
    env.queue_as("queued", Some(&identity)).await;

    let refused = env.client.remove_identity(identity.id).await;
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    let queued = env.client.list_outbox(Some(env.account)).await.unwrap();
    env.client.cancel_outbox(queued[0].id).await.unwrap();

    env.client.remove_identity(identity.id).await.unwrap();
    let own = env.own_identity().await;
    let identities = env.client.list_identities(env.account).await.unwrap();
    assert_eq!(identities, std::slice::from_ref(&own));
    let draft = env.client.get_draft(draft.id).await.unwrap().unwrap();
    assert_eq!(draft.content.identity, Some(own.id));
    // And it goes out as the account.
    env.client.send_draft(draft.id).await.unwrap();
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert_eq!(env.smtp.state().received[0].from, "alice@example.com");
}

#[tokio::test(flavor = "multi_thread")]
async fn drafts_are_written_only_as_the_accounts_identities() {
    let env = env(state()).await;
    let resend = service().await;
    let other = env
        .client
        .add_account(
            catmail_core::NewAccount {
                email: "bob@example.net".into(),
                display_name: None,
                username: "bob".into(),
                imap: new_identity(&resend).smtp,
                smtp: new_identity(&resend).smtp,
            },
            "pw".into(),
            Verify::Skip,
        )
        .await
        .unwrap();
    let theirs = env
        .client
        .add_identity(
            other.id,
            new_identity(&resend),
            "api-key".into(),
            Verify::Skip,
        )
        .await
        .unwrap();
    let err = env
        .client
        .create_draft(env.account, content("x", Some(&theirs)))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn identities_are_added_only_once_logged_in() {
    let env = env(state()).await;
    let resend = service().await;
    let new = new_identity(&resend);
    let add = async |new: &NewIdentity, password: &str| {
        env.client
            .add_identity(env.account, new.clone(), password.into(), Verify::Login)
            .await
    };
    let err = add(&new, "wrong").await.unwrap_err();
    assert!(matches!(err, Error::OutgoingServer(_)), "{err:?}");
    assert_eq!(err.kind(), ErrorKind::Auth);
    let bad = NewIdentity {
        email: "not an address".into(),
        ..new.clone()
    };
    let err = add(&bad, "api-key").await.unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
    let identities = env.client.list_identities(env.account).await.unwrap();
    assert_eq!(identities.len(), 1, "only the account's own");

    add(&new, "api-key").await.unwrap();
    let identities = env.client.list_identities(env.account).await.unwrap();
    assert_eq!(identities.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn new_drafts_are_from_the_default_which_can_be_changed() {
    let env = env(state()).await;
    let resend = service().await;
    let own = env.own_identity().await;
    let identity = add_identity(&env, &resend).await;
    let mut events = env.client.subscribe();

    env.client.set_default_identity(identity.id).await.unwrap();
    assert!(drain(&mut events).contains(&Event::IdentitiesChanged {
        account: env.account
    }));
    let identities = env.client.list_identities(env.account).await.unwrap();
    assert_eq!(
        (identities[0].id, identities[0].is_default),
        (identity.id, true)
    );
    assert!(!identities[1].is_default);
    env.queue_as("as the new default", None).await;
    assert_eq!(env.flush().await, [(OutboxStatus::Done, 1)]);
    assert_eq!(resend.state().received[0].from, "news@example.org");

    // The default stays; the one made with the account can go now.
    let refused = env.client.remove_identity(identity.id).await;
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    env.client.remove_identity(own.id).await.unwrap();
    assert!(matches!(
        env.client.set_default_identity(own.id).await,
        Err(Error::IdentityNotFound(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_fit_the_server_that_sends_the_draft() {
    const MIB: u64 = 1024 * 1024;
    let env = env(state()).await;
    let resend = service().await;
    resend.state().faults.size_limit = Some(MIB);
    // What the check learns stays with the identity.
    let identity = env
        .client
        .add_identity(
            env.account,
            new_identity(&resend),
            "api-key".into(),
            Verify::Login,
        )
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = sparse(dir.path(), "big.bin", MIB);

    let theirs = env
        .client
        .create_draft(env.account, content("theirs", Some(&identity)))
        .await
        .unwrap();
    let err = env.client.attach_file(theirs.id, &file).await.unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
    let own = env
        .client
        .create_draft(env.account, content("own", None))
        .await
        .unwrap();
    env.client.attach_file(own.id, &file).await.unwrap();
}
