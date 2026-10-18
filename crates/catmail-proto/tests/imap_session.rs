use std::time::Duration;

use catmail_proto::TlsConfig;
use catmail_proto::imap::{Credentials, Error, Session};
use catmail_testkit::{FakeImap, FakeMailbox, ServerState, TestCert};
use catmail_types::{Flags, FolderRole, Security, ServerConfig};

const TIMEOUT: Duration = Duration::from_secs(10);

fn server(port: u16, security: Security) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port,
        security,
    }
}

fn creds(password: &str) -> Credentials {
    Credentials::Password {
        username: "alice".into(),
        password: password.into(),
    }
}

fn no_tls() -> TlsConfig {
    TlsConfig::with_roots_only([]).unwrap()
}

async fn login(fake: &FakeImap) -> Session {
    let mut session = Session::connect(&server(fake.port(), Security::Plain), &no_tls(), TIMEOUT)
        .await
        .unwrap();
    session.login(&creds("pw")).await.unwrap();
    session
}

fn sample_state() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    let inbox = state.inbox();
    inbox.uidvalidity = 42;
    let raw = "From: =?UTF-8?B?5byg5LiJ?= <zhang@example.com>\r\n\
               Subject: =?UTF-8?B?5L2g5aW9?=\r\n\
               X-Not-Requested: nope\r\n\r\nbody";
    inbox.append(raw.as_bytes(), &["\\Seen", "$Forwarded"]);
    let uid = inbox.append_simple("second");
    inbox.message_mut(uid).attachment = true;
    state
        .mailboxes
        .push(FakeMailbox::new("&XfJT0ZAB-").with_attribute("\\Sent"));
    state
        .mailboxes
        .push(FakeMailbox::new("Parent").with_attribute("\\Noselect"));
    state
}

#[tokio::test]
async fn reads_folders_and_messages() {
    let fake = FakeImap::start(sample_state()).await;
    let mut session = login(&fake).await;
    assert!(session.capabilities().has("condstore"));

    let mailboxes = session.list().await.unwrap();
    let names: Vec<_> = mailboxes.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["INBOX", "已发送", "Parent"]);
    assert_eq!(mailboxes[1].raw_name, "&XfJT0ZAB-");
    assert_eq!(mailboxes[1].special_use, Some(FolderRole::Sent));
    assert_eq!(mailboxes[0].delimiter, Some('/'));
    assert!(mailboxes[0].selectable && !mailboxes[2].selectable);

    let status = session.examine("INBOX").await.unwrap();
    assert_eq!(status.uidvalidity, 42);
    assert_eq!(status.uidnext, Some(3));
    assert_eq!(status.exists, 2);
    assert!(status.highest_modseq.is_some());

    assert_eq!(session.uid_search_all().await.unwrap(), [1, 2]);
    assert_eq!(session.uid_search_from(2).await.unwrap(), [2]);
    // `3:*` matches UID 2, the highest; it must not be reported as new.
    assert!(session.uid_search_from(3).await.unwrap().is_empty());
    // Likewise `1:1` returning more than UID 1 would be wrong.
    let flags = session.fetch_flags(1, None).await.unwrap();
    assert_eq!(flags.iter().map(|f| f.uid).collect::<Vec<_>>(), [1]);
    let messages = session.fetch_summaries(&[1, 2]).await.unwrap();
    assert_eq!(messages.len(), 2);
    let first = &messages[0];
    assert_eq!(first.state.uid, 1);
    assert_eq!(first.state.flags, Flags::SEEN);
    assert_eq!(first.state.keywords, ["$Forwarded"]);
    assert_eq!(first.headers.subject.as_deref(), Some("你好"));
    assert_eq!(first.headers.from[0].name.as_deref(), Some("张三"));
    assert_eq!(first.internal_date, Some(1_792_200_600));
    assert!(first.state.modseq.is_some());
    assert!(!first.has_attachments);
    assert!(messages[1].has_attachments);

    let examined = session.examine("&XfJT0ZAB-").await.unwrap();
    assert_eq!(examined.exists, 0);
    assert!(session.uid_search_all().await.unwrap().is_empty());
    assert!(matches!(
        session.examine("Missing").await,
        Err(Error::Command { .. })
    ));
    session.logout().await;
}

#[tokio::test]
async fn changed_since_only_returns_changed_flags() {
    let fake = FakeImap::start(sample_state()).await;
    let mut session = login(&fake).await;
    let modseq = session.examine("INBOX").await.unwrap().highest_modseq;
    fake.state().inbox().set_flags(2, &["\\Flagged"]);

    let all = session.fetch_flags(2, None).await.unwrap();
    assert_eq!(all.len(), 2);
    let changed = session.fetch_flags(2, modseq).await.unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].uid, 2);
    assert_eq!(changed[0].flags, Flags::FLAGGED);
}

#[tokio::test]
async fn works_without_condstore() {
    let mut state = sample_state();
    state.faults.condstore = false;
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    let status = session.examine("INBOX").await.unwrap();
    assert_eq!(status.highest_modseq, None);
    let flags = session.fetch_flags(2, Some(1)).await.unwrap();
    assert_eq!(flags.len(), 2, "no CHANGEDSINCE without CONDSTORE");
    assert!(flags.iter().all(|f| f.modseq.is_none()));
    assert!(
        !fake
            .state()
            .log
            .iter()
            .any(|c| c.line.contains("CONDSTORE"))
    );
}

#[tokio::test]
async fn tolerates_duplicate_reordered_and_unsolicited_responses() {
    let mut state = sample_state();
    state.faults.duplicate_fetch = true;
    state.faults.reverse_fetch = true;
    state.faults.unsolicited = true;
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();

    let messages = session.fetch_summaries(&[1, 2]).await.unwrap();
    assert_eq!(
        messages.iter().map(|m| m.state.uid).collect::<Vec<_>>(),
        [1, 2]
    );
    let only = session.fetch_summaries(&[1]).await.unwrap();
    assert_eq!(only.iter().map(|m| m.state.uid).collect::<Vec<_>>(), [1]);
    let flags = session.fetch_flags(2, None).await.unwrap();
    assert_eq!(flags.iter().map(|f| f.uid).collect::<Vec<_>>(), [1, 2]);
}

#[tokio::test]
async fn dropped_connection_is_fatal() {
    let mut state = sample_state();
    state.faults.drop_on_fetch = Some(1);
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();
    let err = session.fetch_summaries(&[1, 2]).await.unwrap_err();
    assert!(matches!(err, Error::Closed), "{err:?}");
    assert!(err.is_fatal());
}

#[tokio::test]
async fn rejects_wrong_password() {
    for auth_plain in [false, true] {
        let mut state = sample_state();
        state.faults.auth_plain = auth_plain;
        let fake = FakeImap::start(state).await;
        let mut session =
            Session::connect(&server(fake.port(), Security::Plain), &no_tls(), TIMEOUT)
                .await
                .unwrap();
        let err = session.login(&creds("wrong")).await.unwrap_err();
        assert!(matches!(err, Error::AuthFailed(_)), "{err:?}");
    }
}

#[tokio::test]
async fn authenticate_plain_and_non_ascii_login() {
    let mut state = ServerState::new("alice", "密码 \"pw\"");
    state.faults.auth_plain = true;
    let fake = FakeImap::start(state).await;
    let mut session = Session::connect(&server(fake.port(), Security::Plain), &no_tls(), TIMEOUT)
        .await
        .unwrap();
    session.login(&creds("密码 \"pw\"")).await.unwrap();
    assert!(
        fake.state()
            .log
            .iter()
            .any(|c| c.line.starts_with("AUTHENTICATE PLAIN"))
    );

    // LOGIN sends a non-ASCII password as a literal.
    let fake = FakeImap::start(ServerState::new("alice", "密码 \"pw\"")).await;
    let mut session = Session::connect(&server(fake.port(), Security::Plain), &no_tls(), TIMEOUT)
        .await
        .unwrap();
    session.login(&creds("密码 \"pw\"")).await.unwrap();
}

#[tokio::test]
async fn implicit_tls() {
    let cert = TestCert::generate();
    let fake = FakeImap::start_tls(sample_state(), &cert).await;
    let tls = TlsConfig::with_roots_only([cert.cert_der()]).unwrap();
    let mut session = Session::connect(&server(fake.port(), Security::Tls), &tls, TIMEOUT)
        .await
        .unwrap();
    session.login(&creds("pw")).await.unwrap();
    assert_eq!(session.list().await.unwrap().len(), 3);
}

#[tokio::test]
async fn untrusted_certificate_is_rejected() {
    let cert = TestCert::generate();
    let fake = FakeImap::start_tls(sample_state(), &cert).await;
    let other = TlsConfig::with_roots_only([TestCert::generate().cert_der()]).unwrap();
    let err = Session::connect(&server(fake.port(), Security::Tls), &other, TIMEOUT)
        .await
        .err()
        .expect("handshake must fail");
    assert!(matches!(err, Error::Io(_)), "{err:?}");
}

#[tokio::test]
async fn starttls_upgrades_before_credentials() {
    let cert = TestCert::generate();
    let fake = FakeImap::start_starttls(sample_state(), &cert).await;
    let tls = TlsConfig::with_roots_only([cert.cert_der()]).unwrap();
    let mut session = Session::connect(&server(fake.port(), Security::StartTls), &tls, TIMEOUT)
        .await
        .unwrap();
    assert!(
        !session.capabilities().has("STARTTLS"),
        "capabilities refreshed after TLS"
    );
    session.login(&creds("pw")).await.unwrap();
    session.examine("INBOX").await.unwrap();

    let log = fake.state().log.clone();
    let login = log.iter().find(|c| c.line.starts_with("LOGIN")).unwrap();
    assert!(login.tls, "credentials only travel over TLS");
}

#[tokio::test]
async fn starttls_ignores_plaintext_injection() {
    let cert = TestCert::generate();
    let mut state = sample_state();
    state.faults.inject_after_starttls = true;
    let fake = FakeImap::start_starttls(state, &cert).await;
    let tls = TlsConfig::with_roots_only([cert.cert_der()]).unwrap();
    // Failing the handshake is as acceptable as ignoring the bytes; trusting them is not.
    if let Ok(session) =
        Session::connect(&server(fake.port(), Security::StartTls), &tls, TIMEOUT).await
    {
        assert!(!session.capabilities().has("X-INJECTED"));
    }
}

#[tokio::test]
async fn starttls_is_required_when_configured() {
    let fake = FakeImap::start(sample_state()).await;
    let err = Session::connect(&server(fake.port(), Security::StartTls), &no_tls(), TIMEOUT)
        .await
        .err()
        .expect("must not fall back to plaintext");
    assert!(matches!(err, Error::Unsupported("STARTTLS")), "{err:?}");
    assert!(!fake.state().log.iter().any(|c| c.line.starts_with("LOGIN")));
}

#[tokio::test]
async fn unparsable_lines_make_authoritative_answers_fail() {
    // LIST, SEARCH and the flags FETCH describe the complete state; a missing line
    // would read as a deleted folder, expunged messages or lost flag changes.
    let garbled = |pattern: &'static str| async move {
        let mut state = sample_state();
        state.faults.garble = Some(pattern.into());
        let fake = FakeImap::start(state).await;
        let session = login(&fake).await;
        (fake, session)
    };
    let check = |err: Error, command: &str| {
        assert!(
            matches!(err, Error::Incomplete(c) if c == command),
            "{err:?}"
        );
        assert!(!err.is_fatal(), "the connection is still usable");
    };

    let (_fake, mut session) = garbled("* LIST").await;
    check(session.list().await.unwrap_err(), "LIST");

    let (_fake, mut session) = garbled("* SEARCH").await;
    session.examine("INBOX").await.unwrap();
    check(session.uid_search_all().await.unwrap_err(), "SEARCH");

    let (_fake, mut session) = garbled("FETCH (UID 2 FLAGS").await;
    session.examine("INBOX").await.unwrap();
    check(session.fetch_flags(2, None).await.unwrap_err(), "FETCH");

    // For summaries a missing message is merely absent; the caller notices the gap.
    let (_fake, mut session) = garbled("FETCH (UID 1 ").await;
    session.examine("INBOX").await.unwrap();
    let messages = session.fetch_summaries(&[1, 2]).await.unwrap();
    assert_eq!(
        messages.iter().map(|m| m.state.uid).collect::<Vec<_>>(),
        [2]
    );
}

#[tokio::test]
async fn examine_requires_a_complete_answer() {
    for (garble, omit, expect_incomplete) in [
        (Some(" EXISTS"), None, true),
        (None, Some(" EXISTS"), false),
        (None, Some("UIDVALIDITY"), false),
    ] {
        let mut state = sample_state();
        state.faults.garble = garble.map(String::from);
        state.faults.omit = omit.map(String::from);
        let fake = FakeImap::start(state).await;
        let mut session = login(&fake).await;
        let err = session.examine("INBOX").await.unwrap_err();
        if expect_incomplete {
            assert!(matches!(err, Error::Incomplete("EXAMINE")), "{err:?}");
        } else {
            assert!(matches!(err, Error::Command { .. }), "{err:?}");
        }
        assert!(!err.is_fatal(), "only this folder fails");
        // And nothing that depends on the selected mailbox runs on a stale one.
        assert!(session.uid_search_all().await.is_err());
    }
}

#[tokio::test]
async fn merges_fetch_responses_split_across_lines() {
    let mut state = sample_state();
    state.faults.split_fetch = true;
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();
    let messages = session.fetch_summaries(&[1, 2]).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].state.flags, Flags::SEEN);
    assert_eq!(messages[0].state.keywords, ["$Forwarded"]);
    assert!(messages[0].state.modseq.is_some());
    assert_eq!(messages[0].headers.subject.as_deref(), Some("你好"));
}

#[tokio::test]
async fn preauth_skips_login() {
    let mut state = sample_state();
    state.faults.preauth = true;
    let fake = FakeImap::start(state).await;
    let mut session = Session::connect(&server(fake.port(), Security::Plain), &no_tls(), TIMEOUT)
        .await
        .unwrap();
    session.login(&creds("pw")).await.unwrap();
    assert_eq!(session.list().await.unwrap().len(), 3);
    assert!(!fake.state().log.iter().any(|c| c.line.starts_with("LOGIN")));
}
