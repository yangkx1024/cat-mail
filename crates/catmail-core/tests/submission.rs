//! Sending through a real SMTP submission server: Dovecot's, started by
//! `testenv/dovecot/run.sh` (see `testenv`), relaying to a mailpit sink whose HTTP API
//! shows what arrived. Saving to Sent goes to the same Dovecot over IMAP.
//!
//! Messages are found again by Message-ID, or by a subject with the run's tag in Sent, so
//! the tests can be rerun against the same servers.

mod testenv;

use catmail_core::{
    AccountId, Address, DraftContent, ErrorKind, MailClient, OutboxEntry, OutboxStatus,
    ServerConfig, TlsConfig,
};
use mail_parser::{MessageParser, MimeHeaders};
use serde_json::Value;
use testenv::{TestEnv, add_account, open_client, run_tag, sent_copies};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A client trusting the servers' certificate, with an account of the `sender` user
/// sending through `smtp`.
async fn client(
    env: &TestEnv,
    smtp: &ServerConfig,
    password: &str,
) -> (MailClient, AccountId, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let tls = TlsConfig::with_roots_only([env.cert.as_slice()]).unwrap();
    let client = open_client(dir.path(), Some(tls));
    let account = add_account(
        &client,
        "sender",
        password,
        None,
        Some("猫邮件"),
        env.imap.clone(),
        smtp.clone(),
    )
    .await;
    (client, account, dir)
}

fn addr(name: Option<&str>, email: &str) -> Address {
    Address {
        name: name.map(Into::into),
        email: email.into(),
    }
}

/// A short message to zhang@example.com.
fn note(subject: &str) -> DraftContent {
    DraftContent {
        to: vec![addr(None, "zhang@example.com")],
        subject: subject.into(),
        text: "hi\n".into(),
        ..Default::default()
    }
}

/// Sends `content` (with `attachment`, if any) and works through the outbox once.
async fn send(
    client: &MailClient,
    account: AccountId,
    content: DraftContent,
    attachment: Option<&[u8]>,
) -> OutboxEntry {
    let draft = client.create_draft(account, content).await.unwrap();
    if let Some(data) = attachment {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("数据.bin");
        std::fs::write(&path, data).unwrap();
        client.attach_file(draft.id, &path).await.unwrap();
    }
    let queued = client.send_draft(draft.id).await.unwrap();
    let flushed = client.flush_outbox(account).await.unwrap();
    flushed
        .entries
        .into_iter()
        .find(|e| e.id == queued.id)
        .unwrap()
}

/// A GET to mailpit's API; the body.
async fn mailpit(env: &TestEnv, path: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(&env.mailpit).await.unwrap();
    // HTTP/1.0: the body runs to the end of the connection, never chunked.
    let request = format!("GET {path} HTTP/1.0\r\nHost: {}\r\n\r\n", env.mailpit);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("HTTP response");
    let head = String::from_utf8_lossy(&response[..end]);
    assert!(
        head.starts_with("HTTP/1.0 200") || head.starts_with("HTTP/1.1 200"),
        "{head}"
    );
    response[end + 4..].to_vec()
}

/// mailpit's summary of the message with this Message-ID, if it arrived.
async fn delivered(env: &TestEnv, message_id: &str) -> Option<Value> {
    // Message-IDs here are hex digits and a domain; only the `@` needs escaping.
    let query = format!("message-id:%22{}%22", message_id.replace('@', "%40"));
    let path = format!("/api/v1/search?query={query}");
    let found: Value = serde_json::from_slice(&mailpit(env, &path).await).unwrap();
    let mut messages = found["messages"].as_array().unwrap().clone();
    assert!(messages.len() <= 1, "{messages:?}");
    messages.pop()
}

fn emails(summary: &Value, field: &str) -> Vec<String> {
    summary[field]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|a| a["Address"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_with_starttls_and_saves_to_sent() {
    let Some(env) = testenv::testenv() else {
        return;
    };
    let (client, account, _dir) = client(&env, &env.starttls, "sender").await;
    // Found in Sent by subject, so it has to be this run's.
    let subject = format!("季度报告 {}", run_tag());
    let text = "你好，\n.a line starting with a dot\n.\n..\nend\n";
    let attachment: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let content = DraftContent {
        to: vec![addr(Some("张三"), "zhang@example.com")],
        cc: vec![addr(None, "li@example.com")],
        bcc: vec![addr(None, "boss@example.com")],
        subject: subject.clone(),
        text: text.into(),
        identity: None,
    };
    let entry = send(&client, account, content, Some(&attachment)).await;
    assert_eq!(entry.status, OutboxStatus::Done, "{entry:?}");

    let summary = delivered(&env, &entry.message_id)
        .await
        .expect("relayed to mailpit");
    assert_eq!(emails(&summary, "To"), ["zhang@example.com"]);
    assert_eq!(emails(&summary, "Cc"), ["li@example.com"]);
    // Only in the envelope: mailpit lists (and adds a header for) recipients the
    // message itself doesn't name.
    assert_eq!(emails(&summary, "Bcc"), ["boss@example.com"]);

    let saved = sent_copies(&client, account, &subject).await;
    assert_eq!(saved.len(), 1);
    let copy = client.raw_message(saved[0].id).await.unwrap();

    // What went out is exactly what was saved: the servers only put trace headers in
    // front.
    let id = summary["ID"].as_str().unwrap();
    let relayed = mailpit(&env, &format!("/api/v1/message/{id}/raw")).await;
    assert!(relayed.ends_with(&copy), "relayed and saved copies differ");
    let m = MessageParser::new().parse(&copy).unwrap();
    assert_eq!(m.subject(), Some(subject.as_str()));
    assert!(m.bcc().is_none(), "Bcc stays out of the message");
    assert_eq!(m.body_text(0).unwrap().replace("\r\n", "\n"), text);
    assert_eq!(m.attachment(0).unwrap().contents(), attachment.as_slice());
    assert_eq!(m.attachment(0).unwrap().attachment_name(), Some("数据.bin"));
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_with_implicit_tls() {
    let Some(env) = testenv::testenv() else {
        return;
    };
    let (client, account, _dir) = client(&env, &env.tls, "sender").await;
    let entry = send(&client, account, note("implicit TLS"), None).await;
    assert_eq!(entry.status, OutboxStatus::Done, "{entry:?}");
    assert!(delivered(&env, &entry.message_id).await.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_recipient_stops_the_whole_message() {
    let Some(env) = testenv::testenv() else {
        return;
    };
    let (client, account, _dir) = client(&env, &env.starttls, "sender").await;
    // mailpit refuses refused.example, and Dovecot passes its answer to RCPT TO on.
    let mut content = note("partly refused");
    content.to.push(addr(None, "nobody@refused.example"));
    let entry = send(&client, account, content, None).await;
    assert_eq!(entry.status, OutboxStatus::Failed, "{entry:?}");
    let error = entry.last_error.unwrap();
    assert!(error.contains("nobody@refused.example"), "{error}");
    assert!(
        delivered(&env, &entry.message_id).await.is_none(),
        "nobody gets it if someone can't"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn message_over_the_size_limit_is_not_sent() {
    let Some(env) = testenv::testenv() else {
        return;
    };
    let (client, account, _dir) = client(&env, &env.starttls, "sender").await;
    // Over `submission_max_mail_size` (1 MiB, testenv/dovecot/dovecot.conf.in) once
    // base64-encoded.
    let entry = send(&client, account, note("too large"), Some(&vec![7; 900_000])).await;
    assert_eq!(entry.status, OutboxStatus::Failed, "{entry:?}");
    let error = entry.last_error.unwrap();
    assert!(error.contains("1048576"), "{error}");
    assert!(delivered(&env, &entry.message_id).await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_holds_the_message_and_is_reported() {
    let Some(env) = testenv::testenv() else {
        return;
    };
    let (client, account, _dir) = client(&env, &env.starttls, "wrong").await;
    let entry = send(&client, account, note("wrong password"), None).await;
    // The account's trouble: the message waits, without counting an attempt.
    assert_eq!(
        (entry.status, entry.attempts),
        (OutboxStatus::Queued, 0),
        "{entry:?}"
    );
    let error = entry.last_error.unwrap();
    assert!(error.contains("login rejected"), "{error}");
    let problem = client
        .account_status(account)
        .send_problem()
        .unwrap()
        .1
        .clone();
    assert_eq!(problem.error.kind, ErrorKind::Auth, "{problem:?}");
}
