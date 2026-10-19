use std::time::Duration;

use catmail_proto::smtp::{Error, Session};
use catmail_proto::{Credentials, TlsConfig};
use catmail_testkit::{FakeSmtp, SmtpState, TestCert};
use catmail_types::{Security, ServerConfig};

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

async fn login(fake: &FakeSmtp) -> Session {
    let tls = TlsConfig::with_roots_only([]).unwrap();
    let mut session = Session::connect(&server(fake.port(), Security::Plain), &tls, TIMEOUT)
        .await
        .unwrap();
    session.login(&creds("pw")).await.unwrap();
    session
}

/// Starts with a dot and has a lone LF, which must not end the message early.
const MESSAGE: &[u8] = b"From: alice@example.com\r\nSubject: hi\r\n\r\n.leading dot\n.\nend\r\n";

async fn send(session: &mut Session, rcpts: &[&str]) -> Result<(), Error> {
    let rcpts: Vec<String> = rcpts.iter().map(|r| r.to_string()).collect();
    session.begin("alice@example.com", &rcpts, MESSAGE).await?;
    session.write_message(MESSAGE).await?;
    session.end_message().await
}

#[tokio::test]
async fn sends_messages_on_one_connection() {
    let fake = FakeSmtp::start(SmtpState::new("alice", "pw")).await;
    let mut session = login(&fake).await;
    send(&mut session, &["bob@example.com", "carol@example.com"])
        .await
        .unwrap();
    send(&mut session, &["dave@example.com"]).await.unwrap();
    session.quit().await;

    let state = fake.state();
    assert_eq!(state.connections, 1);
    assert_eq!(state.received.len(), 2);
    let first = &state.received[0];
    assert_eq!(first.from, "alice@example.com");
    assert_eq!(first.recipients, ["bob@example.com", "carol@example.com"]);
    assert_eq!(
        first.data,
        b"From: alice@example.com\r\nSubject: hi\r\n\r\n.leading dot\r\n.\r\nend\r\n"
    );
    let lines: Vec<_> = state.log.iter().map(|c| c.line.as_str()).collect();
    assert!(lines[0].starts_with("EHLO "), "{lines:?}");
    assert!(lines[1].starts_with("AUTH PLAIN "), "{lines:?}");
    assert_eq!(
        lines[2], "MAIL FROM:<alice@example.com>",
        "no SIZE unless offered"
    );
}

#[tokio::test]
async fn auth_login_and_rejection() {
    let mut state = SmtpState::new("alice", "pw");
    state.faults.auth_mechanisms = vec!["LOGIN"];
    let fake = FakeSmtp::start(state).await;
    login(&fake).await;

    let tls = TlsConfig::with_roots_only([]).unwrap();
    let mut session = Session::connect(&server(fake.port(), Security::Plain), &tls, TIMEOUT)
        .await
        .unwrap();
    let err = session.login(&creds("wrong")).await.unwrap_err();
    assert!(matches!(err, Error::AuthFailed(_)), "{err:?}");
    assert!(err.is_permanent());
}

#[tokio::test]
async fn refusals_leave_the_connection_usable() {
    let mut state = SmtpState::new("alice", "pw");
    state.faults.reject_recipients = vec![("nobody@example.com".into(), 550)];
    state.faults.busy = 1;
    state.faults.end_of_data_replies = vec![554];
    let fake = FakeSmtp::start(state).await;
    let mut session = login(&fake).await;

    let busy = send(&mut session, &["bob@example.com"]).await.unwrap_err();
    assert!(matches!(busy, Error::Reply { code: 451, .. }), "{busy:?}");
    assert!(!busy.is_permanent() && !busy.is_fatal());

    let unknown = send(&mut session, &["bob@example.com", "nobody@example.com"])
        .await
        .unwrap_err();
    assert!(
        matches!(&unknown, Error::Reply { code: 550, text, .. } if text.contains("nobody@example.com")),
        "{unknown:?}"
    );
    assert!(unknown.is_permanent());

    let spam = send(&mut session, &["bob@example.com"]).await.unwrap_err();
    assert!(matches!(spam, Error::Reply { code: 554, .. }), "{spam:?}");

    send(&mut session, &["bob@example.com"]).await.unwrap();
    let state = fake.state();
    assert_eq!(state.received.len(), 1, "only the last one was delivered");
    assert_eq!(state.received[0].recipients, ["bob@example.com"]);
    assert!(state.log.iter().any(|c| c.line == "RSET"));
}

#[tokio::test]
async fn declares_the_size_when_the_server_limits_it() {
    let mut state = SmtpState::new("alice", "pw");
    state.faults.size_limit = Some(1000);
    let fake = FakeSmtp::start(state).await;
    let mut session = login(&fake).await;
    send(&mut session, &["bob@example.com"]).await.unwrap();
    let mail = format!("MAIL FROM:<alice@example.com> SIZE={}", MESSAGE.len());
    assert!(fake.state().log.iter().any(|c| c.line == mail));
}

#[tokio::test]
async fn too_large_and_invalid_addresses_fail_before_sending() {
    let mut state = SmtpState::new("alice", "pw");
    state.faults.size_limit = Some(10);
    let fake = FakeSmtp::start(state).await;
    let mut session = login(&fake).await;
    assert_eq!(session.max_size(), Some(10));
    let err = send(&mut session, &["bob@example.com"]).await.unwrap_err();
    assert!(matches!(err, Error::TooLarge { limit: 10, .. }), "{err:?}");

    let injected = "bob@example.com>\r\nRCPT TO:<evil@example.com";
    let err = send(&mut session, &[injected]).await.unwrap_err();
    assert!(matches!(err, Error::InvalidAddress(_)), "{err:?}");
    assert!(
        !fake.state().log.iter().any(|c| c.line.starts_with("MAIL")),
        "nothing was sent"
    );
}

#[tokio::test]
async fn lost_confirmation_fails_end_message_only() {
    let mut state = SmtpState::new("alice", "pw");
    state.faults.drop_after_data = 1;
    let fake = FakeSmtp::start(state).await;
    let mut session = login(&fake).await;
    session
        .begin("alice@example.com", &["bob@example.com".into()], MESSAGE)
        .await
        .unwrap();
    session.write_message(MESSAGE).await.unwrap();
    let err = session.end_message().await.unwrap_err();
    assert!(err.is_fatal() && !err.is_permanent(), "{err:?}");
    assert_eq!(fake.state().received.len(), 1, "delivered all the same");
}

#[tokio::test]
async fn implicit_tls_and_starttls() {
    let cert = TestCert::generate();
    let tls = TlsConfig::with_roots_only([cert.cert_der()]).unwrap();

    let fake = FakeSmtp::start_tls(SmtpState::new("alice", "pw"), &cert).await;
    let mut session = Session::connect(&server(fake.port(), Security::Tls), &tls, TIMEOUT)
        .await
        .unwrap();
    session.login(&creds("pw")).await.unwrap();
    send(&mut session, &["bob@example.com"]).await.unwrap();

    let mut state = SmtpState::new("alice", "pw");
    state.faults.inject_after_starttls = true;
    let fake = FakeSmtp::start_starttls(state, &cert).await;
    let mut session = Session::connect(&server(fake.port(), Security::StartTls), &tls, TIMEOUT)
        .await
        .unwrap();
    session.login(&creds("pw")).await.unwrap();
    send(&mut session, &["bob@example.com"]).await.unwrap();
    let state = fake.state();
    assert_eq!(state.received.len(), 1);
    let auth = state
        .log
        .iter()
        .find(|c| c.line.starts_with("AUTH"))
        .unwrap();
    assert!(auth.tls, "credentials only travel inside TLS");
}

#[tokio::test]
async fn starttls_is_required_when_configured() {
    let fake = FakeSmtp::start(SmtpState::new("alice", "pw")).await;
    let tls = TlsConfig::with_roots_only([]).unwrap();
    let err = Session::connect(&server(fake.port(), Security::StartTls), &tls, TIMEOUT)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, Error::Unsupported("STARTTLS")), "{err:?}");
}

#[tokio::test]
async fn busy_ehlo_is_temporary() {
    let mut state = SmtpState::new("alice", "pw");
    state.faults.ehlo_busy = 1;
    let fake = FakeSmtp::start(state).await;
    let tls = TlsConfig::with_roots_only([]).unwrap();
    let err = Session::connect(&server(fake.port(), Security::Plain), &tls, TIMEOUT)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, Error::Reply { code: 421, .. }), "{err:?}");
    assert!(!err.is_permanent());
    login(&fake).await;
}

#[tokio::test]
async fn end_of_message_verdicts() {
    let mut state = SmtpState::new("alice", "pw");
    state.faults.end_of_data_replies = vec![251, 354];
    let fake = FakeSmtp::start(state).await;
    let mut session = login(&fake).await;

    // Any positive completion is an acceptance.
    send(&mut session, &["bob@example.com"]).await.unwrap();
    // Neither acceptance nor refusal: not something to take as "not delivered".
    let err = send(&mut session, &["bob@example.com"]).await.unwrap_err();
    assert!(!matches!(err, Error::Reply { .. }), "{err:?}");
    assert!(err.is_fatal() && !err.is_permanent(), "{err:?}");
}
