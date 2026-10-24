// `fetch_flags(&[1..=2], …)` is a UID set of one range, not a range meant to be collected.
#![allow(clippy::single_range_in_vec_init)]

use std::time::Duration;

use catmail_proto::Credentials;
use catmail_proto::TlsConfig;
use catmail_proto::imap::{ChangedFlags, CopyUid, Error, Session};
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
    Credentials {
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

/// Text plus an attachment whose decoded name is not ASCII, so the fake server sends it
/// as a literal inside BODYSTRUCTURE.
const MULTIPART: &[u8] = b"From: Bob <bob@example.com>\r\n\
    Subject: second\r\n\
    Content-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n\
    --b1\r\n\
    Content-Type: text/plain; charset=utf-8\r\n\r\n\
    See attached.\r\n\
    --b1\r\n\
    Content-Type: application/pdf; name=\"=?UTF-8?B?5oql5ZGKLnBkZg==?=\"\r\n\
    Content-Disposition: attachment; filename=\"=?UTF-8?B?5oql5ZGKLnBkZg==?=\"\r\n\
    Content-Transfer-Encoding: base64\r\n\r\n\
    JVBERi0xLjQK\r\n\
    --b1--\r\n";

fn sample_state() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    let inbox = state.inbox();
    inbox.uidvalidity = 42;
    let raw = "From: =?UTF-8?B?5byg5LiJ?= <zhang@example.com>\r\n\
               Subject: =?UTF-8?B?5L2g5aW9?=\r\n\
               X-Not-Requested: nope\r\n\r\nbody";
    inbox.append(raw.as_bytes(), &["\\Seen", "$Forwarded"]);
    inbox.append(MULTIPART, &[]);
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
    let flags = session.fetch_flags(&[1..=1], None).await.unwrap();
    assert_eq!(flags.iter().map(|f| f.uid).collect::<Vec<_>>(), [1]);
    let flags = session.fetch_flags(&[2..=9], None).await.unwrap();
    assert_eq!(flags.iter().map(|f| f.uid).collect::<Vec<_>>(), [2]);
    assert!(
        session
            .fetch_flags(&[3..=9], None)
            .await
            .unwrap()
            .is_empty()
    );
    let messages = session.fetch_summaries(&[1, 2]).await.unwrap();
    assert_eq!(messages.len(), 2);
    let first = &messages[0];
    assert_eq!(first.state.uid, 1);
    assert_eq!(first.state.flags, Flags::SEEN);
    assert_eq!(first.state.keywords, ["$Forwarded"]);
    assert_eq!(first.headers.subject.as_deref(), Some("你好"));
    assert_eq!(first.headers.from[0].name.as_deref(), Some("张三"));
    // 17-Oct-2026 09:30:01 +0800
    assert_eq!(first.internal_date, Some(1_792_200_601));
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

    let all = session.fetch_flags(&[1..=2], None).await.unwrap();
    assert_eq!(all.len(), 2);
    let changed = session.fetch_flags(&[1..=2], modseq).await.unwrap();
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
    let flags = session.fetch_flags(&[1..=2], Some(1)).await.unwrap();
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
    let flags = session.fetch_flags(&[1..=2], None).await.unwrap();
    assert_eq!(flags.iter().map(|f| f.uid).collect::<Vec<_>>(), [1, 2]);
}

#[tokio::test]
async fn reads_internaldate_with_a_one_digit_day() {
    // QQ Mail leaves out the space RFC 3501 puts before a one-digit day.
    let mut state = sample_state();
    state.inbox().message_mut(2).internal_date = "2-Oct-2026 08:13:27 +0800".into();
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();

    let messages = session.fetch_summaries(&[1, 2]).await.unwrap();
    assert_eq!(
        messages.iter().map(|m| m.state.uid).collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(messages[1].internal_date, Some(1_790_900_007));
    assert!(
        messages[1].has_attachments,
        "the rest of the response is read too"
    );
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
async fn mailbox_names_with_line_breaks_go_as_literals() {
    // Names come from the server and are used as they are, so a CR LF in one must not
    // end the command early: the rest would run as a command of its own.
    let name = "Inbox\r\nX";
    let mut state = sample_state();
    state.mailboxes.push(FakeMailbox::new(name));
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    let start = fake.state().log.len();
    session.examine(name).await.unwrap();

    // The fake ends a command at a line break unless a literal carries it, so the name
    // arriving whole, in one command, means it went as a literal.
    let lines: Vec<_> = fake.state().log[start..]
        .iter()
        .map(|c| c.line.clone())
        .collect();
    assert_eq!(lines, [format!("EXAMINE \"{name}\" (CONDSTORE)")]);
}

#[tokio::test]
async fn malformed_requested_items_make_answers_incomplete() {
    // imap-proto reads a malformed item it knows as one it doesn't, so the line parses;
    // an item that was asked for and is missing counts as unparsed all the same.
    let mut state = sample_state();
    state.faults.broken_flags = vec![2];
    state.faults.broken_bodystructure = vec![2];
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();

    let err = session.fetch_flags(&[1..=2], None).await.unwrap_err();
    assert!(matches!(err, Error::Incomplete("UID FETCH")), "{err:?}");
    assert!(!err.is_fatal(), "the connection is still usable");
    let err = session.fetch_structure(2).await.unwrap_err();
    assert!(matches!(err, Error::Incomplete("UID FETCH")), "{err:?}");
    assert!(session.fetch_structure(1).await.unwrap().is_some());
    assert_eq!(session.fetch_flags(&[1..=1], None).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_malformed_bodystructure_is_asked_for_again_without() {
    let mut state = sample_state();
    state.faults.broken_bodystructure = vec![2];
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();
    let start = fake.state().log.len();

    let messages = session.fetch_summaries(&[1, 2]).await.unwrap();
    let second = messages
        .iter()
        .find(|m| m.state.uid == 2)
        .expect("not hidden");
    assert!(
        !second.has_attachments,
        "unknown until the body is downloaded"
    );
    let fetches: Vec<String> = fake.state().log[start..]
        .iter()
        .map(|c| c.line.clone())
        .collect();
    assert_eq!(fetches.len(), 2, "{fetches:?}");
    assert!(
        fetches[1].starts_with("UID FETCH 2 ") && !fetches[1].contains("BODYSTRUCTURE"),
        "{fetches:?}"
    );
}

#[tokio::test]
async fn servers_with_only_imap4rev2() {
    // imap-proto's CAPABILITY parser insists on IMAP4rev1.
    for auth_plain in [false, true] {
        let mut state = sample_state();
        state.faults.imap4rev2_only = true;
        state.faults.auth_plain = auth_plain;
        let fake = FakeImap::start(state).await;
        let mut session = login(&fake).await;
        assert!(session.capabilities().has("IMAP4rev2"));
        assert!(session.capabilities().has("IDLE"));
        let logged_in = fake
            .state()
            .log
            .iter()
            .map(|c| c.line.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            logged_in
                .iter()
                .any(|l| l.starts_with("AUTHENTICATE PLAIN")),
            auth_plain,
            "{logged_in:?}"
        );
        assert!(!session.list().await.unwrap().is_empty());
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

    // Left out rather than garbled: only the missing INBOX gives it away, since the
    // user does have one.
    let mut state = sample_state();
    state.faults.omit = Some("INBOX".into());
    let fake = FakeImap::start(state).await;
    check(login(&fake).await.list().await.unwrap_err(), "LIST");

    // A user without INBOX: the same listing is complete (RFC 3501 §6.3.8).
    let mut state = sample_state();
    state.mailboxes.retain(|m| m.name != "INBOX");
    let fake = FakeImap::start(state).await;
    let listed = login(&fake).await.list().await.unwrap();
    assert!(!listed.is_empty() && listed.iter().all(|m| m.raw_name != "INBOX"));

    let (_fake, mut session) = garbled("* SEARCH").await;
    session.examine("INBOX").await.unwrap();
    check(session.uid_search_all().await.unwrap_err(), "UID SEARCH");

    let (_fake, mut session) = garbled("FETCH (UID 2 FLAGS").await;
    session.examine("INBOX").await.unwrap();
    check(
        session.fetch_flags(&[1..=2], None).await.unwrap_err(),
        "UID FETCH",
    );

    // For summaries a lost line has the missing messages asked for again, without the
    // BODYSTRUCTURE that usually makes an answer unparsable. What is still missing (here
    // UID 9, which doesn't exist) is merely absent; the caller notices the gap.
    let (fake, mut session) = garbled("FETCH (UID 1 ").await;
    session.examine("INBOX").await.unwrap();
    let start = fake.state().log.len();
    let messages = session.fetch_summaries(&[1, 2, 9]).await.unwrap();
    let mut uids: Vec<u32> = messages.iter().map(|m| m.state.uid).collect();
    uids.sort_unstable();
    assert_eq!(uids, [1, 2]);
    let fetches: Vec<String> = fake.state().log[start..]
        .iter()
        .map(|c| c.line.clone())
        .collect();
    assert_eq!(fetches.len(), 2, "{fetches:?}");
    assert!(fetches[0].contains("BODYSTRUCTURE"), "{fetches:?}");
    assert!(
        fetches[1].starts_with("UID FETCH 1,9 ") && !fetches[1].contains("BODYSTRUCTURE"),
        "{fetches:?}"
    );

    // Without a lost line, a missing message is simply gone: no second FETCH.
    let start = fake.state().log.len();
    let messages = session.fetch_summaries(&[2, 9]).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(fake.state().log.len(), start + 1);
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

#[tokio::test]
async fn fetches_bodies_without_marking_them_seen() {
    let fake = FakeImap::start(sample_state()).await;
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();

    assert_eq!(session.fetch_raw(2).await.unwrap().unwrap(), MULTIPART);
    assert_eq!(session.fetch_raw(99).await.unwrap(), None);

    let structure = session.fetch_structure(2).await.unwrap().unwrap();
    assert_eq!(structure.text_sections(1_000), ["1"]);
    let fetched = session
        .fetch_sections(2, &["1".into(), "2".into(), "7".into()])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fetched["1"], b"See attached.");
    assert_eq!(fetched["2"], b"JVBERi0xLjQK");
    assert!(
        !fetched.contains_key("7"),
        "NIL for a section that doesn't exist"
    );

    let content = structure.content(&fetched);
    assert_eq!(content.text.as_deref(), Some("See attached."));
    assert_eq!(content.parts[0].filename.as_deref(), Some("报告.pdf"));
    assert_eq!(content.parts[0].data.as_deref(), Some(&b"%PDF-1.4\n"[..]));
    assert_eq!(session.fetch_structure(99).await.unwrap(), None);

    let log = fake.state().log.clone();
    let fetches: Vec<_> = log.iter().filter(|c| c.line.contains("FETCH")).collect();
    assert!(
        fetches.iter().all(|c| !c.line.contains("BODY[")),
        "{fetches:?}"
    );
    assert!(fetches.iter().any(|c| c.line.contains("BODY.PEEK[2]")));
    assert!(fetches.iter().any(|c| c.line.contains("BODY.PEEK[]")));
}

#[tokio::test]
async fn appends_and_finds_by_message_id() {
    let mut state = sample_state();
    state.faults.reject_appends = 1;
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    let raw = b"From: alice@example.com\r\nMessage-ID: <sent.1@example.com>\r\n\r\nhi\r\n";

    let err = session
        .append("&XfJT0ZAB-", Flags::SEEN, raw)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Command { .. }), "{err:?}");
    assert!(!err.is_fatal());

    let appended = session
        .append("&XfJT0ZAB-", Flags::SEEN, raw)
        .await
        .unwrap();
    assert_eq!(appended, Some((1, 1)), "APPENDUID");
    {
        let mut state = fake.state();
        let sent = state.mailbox_mut("&XfJT0ZAB-");
        assert_eq!(sent.messages[0].raw, raw, "stored byte for byte");
        assert_eq!(sent.messages[0].flags, ["\\Seen"]);
    }

    session.examine("&XfJT0ZAB-").await.unwrap();
    assert_eq!(
        session
            .uid_search_message_id("sent.1@example.com")
            .await
            .unwrap(),
        [1]
    );
    assert!(
        session
            .uid_search_message_id("sent.2@example.com")
            .await
            .unwrap()
            .is_empty()
    );

    fake.state().faults.uidplus = false;
    let mut session = login(&fake).await;
    assert_eq!(
        session.append("INBOX", Flags::empty(), raw).await.unwrap(),
        None
    );
    let err = session
        .append("Missing", Flags::empty(), raw)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Command { .. }), "{err:?}");
}

#[tokio::test]
async fn a_message_imap_cant_carry_is_not_sent() {
    let fake = FakeImap::start(sample_state()).await;
    let mut session = login(&fake).await;
    let err = session
        .append("INBOX", Flags::empty(), b"Subject: x\r\n\r\na\0b\r\n")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
    assert!(!err.is_fatal());
    assert_eq!(fake.state().received("APPEND"), 0);
    // The connection is as it was.
    session.examine("INBOX").await.unwrap();
}

#[tokio::test]
async fn append_refused_before_the_message_is_a_refusal() {
    let mut state = sample_state();
    state.faults.reject_append_literal = true;
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    let raw = b"Subject: x\r\n\r\nhi\r\n";
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        session.append("&XfJT0ZAB-", Flags::SEEN, raw),
    )
    .await
    .expect("answered at once")
    .unwrap_err();
    assert!(matches!(err, Error::Command { .. }), "{err:?}");
    assert!(!err.is_fatal(), "the message was never sent");
    // The command is over: the connection goes on.
    session.examine("INBOX").await.unwrap();
}

/// INBOX with UIDs 1..=3, and an Archive.
fn three_messages() -> ServerState {
    let mut state = ServerState::new("alice", "pw");
    for subject in ["a", "b", "c"] {
        state.inbox().append_simple(subject);
    }
    let mut archive = FakeMailbox::new("Archive");
    archive.uidvalidity = 9;
    archive.append_simple("old");
    state.mailboxes.push(archive);
    state
}

fn uids(fake: &FakeImap, mailbox: &str) -> Vec<(u32, Vec<String>)> {
    fake.state()
        .mailbox_mut(mailbox)
        .messages
        .iter()
        .map(|m| (m.uid, m.flags.clone()))
        .collect()
}

#[tokio::test]
async fn changes_flags_moves_and_deletes() {
    let fake = FakeImap::start(three_messages()).await;
    let mut session = login(&fake).await;

    // EXAMINE is read-only; the session doesn't even ask.
    session.examine("INBOX").await.unwrap();
    let err = session
        .uid_store(&[1], Flags::SEEN, Flags::empty())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Protocol(_)), "{err}");

    let status = session.select("INBOX").await.unwrap();
    assert_eq!(status.exists, 3);
    fake.state().inbox().set_flags(2, &["\\Flagged"]);
    session
        .uid_store(&[1, 2, 99], Flags::SEEN, Flags::FLAGGED)
        .await
        .unwrap();
    assert_eq!(
        uids(&fake, "INBOX"),
        [
            (1, vec!["\\Seen".into()]),
            (2, vec!["\\Seen".into()]),
            (3, vec![])
        ]
    );

    assert_eq!(session.status("Archive").await.unwrap(), (9, 2));
    let copied = session.uid_move(&[2, 3], "Archive").await.unwrap();
    assert_eq!(
        copied,
        Some(CopyUid {
            uidvalidity: 9,
            uids: vec![(2, 2), (3, 3)]
        })
    );
    assert_eq!(uids(&fake, "INBOX"), [(1, vec!["\\Seen".into()])]);
    assert_eq!(uids(&fake, "Archive")[1], (2, vec!["\\Seen".into()]));

    session.uid_delete(&[1]).await.unwrap();
    assert!(uids(&fake, "INBOX").is_empty());
    // EXISTS followed the expunges, so a search knows the mailbox is empty.
    assert!(session.uid_search_all().await.unwrap().is_empty());
    let log = fake
        .state()
        .log
        .iter()
        .map(|c| c.line.clone())
        .collect::<Vec<_>>();
    assert!(log.contains(&"UID MOVE 2:3 Archive".to_owned()), "{log:?}");
    assert!(log.contains(&"UID EXPUNGE 1".to_owned()), "{log:?}");
}

#[tokio::test]
async fn ignores_a_copyuid_naming_more_uids_than_were_moved() {
    let fake = FakeImap::start(three_messages()).await;
    let mut session = login(&fake).await;
    session.select("INBOX").await.unwrap();

    // Expanded, these ranges would be two 16 GiB vectors: the process would abort.
    fake.state().faults.forged_copyuid = Some("9 1:4294967295 1:4294967295".into());
    assert_eq!(session.uid_move(&[1], "Archive").await.unwrap(), None);
    // The sets pair up, but name UIDs that weren't asked for.
    fake.state().faults.forged_copyuid = Some("9 2:3 5:6".into());
    assert_eq!(session.uid_copy(&[2], "Archive").await.unwrap(), None);
    // Within bounds, a range is paired in order, whichever end comes first.
    fake.state().faults.forged_copyuid = Some("9 3:2 7,6".into());
    assert_eq!(
        session.uid_copy(&[2, 3], "Archive").await.unwrap(),
        Some(CopyUid {
            uidvalidity: 9,
            uids: vec![(2, 7), (3, 6)]
        })
    );
}

#[tokio::test]
async fn copies_and_deletes_without_move_or_uidplus() {
    let mut state = three_messages();
    state.faults.move_command = false;
    state.faults.uidplus = false;
    // Someone else marked 3 deleted; without UIDPLUS the expunge takes it along.
    state.inbox().set_flags(3, &["\\Deleted"]);
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    session.select("INBOX").await.unwrap();

    let err = session.uid_move(&[1], "Archive").await.unwrap_err();
    assert!(matches!(err, Error::Unsupported("MOVE")), "{err}");
    assert_eq!(session.uid_copy(&[1], "Archive").await.unwrap(), None);
    session.uid_delete(&[1]).await.unwrap();
    assert_eq!(uids(&fake, "INBOX"), [(2, vec![])]);
    assert_eq!(uids(&fake, "Archive").len(), 2);
    let log = fake
        .state()
        .log
        .iter()
        .map(|c| c.line.clone())
        .collect::<Vec<_>>();
    assert!(
        log.ends_with(&[
            "UID COPY 1 Archive".into(),
            "UID STORE 1 +FLAGS.SILENT (\\Deleted)".into(),
            "EXPUNGE".into()
        ]),
        "{log:?}"
    );
}

#[tokio::test]
async fn refused_changes_leave_the_connection_usable() {
    let mut state = three_messages();
    state.faults.refuse_changes = Some("[CANNOT] Permission denied".into());
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    session.select("INBOX").await.unwrap();
    let err = session.uid_move(&[1], "Archive").await.unwrap_err();
    assert!(
        matches!(err, Error::Command { .. }) && !err.is_fatal(),
        "{err}"
    );
    let err = session.uid_move(&[1], "Nowhere").await.unwrap_err();
    assert!(matches!(err, Error::Command { .. }), "{err}");
    assert_eq!(session.uid_search_all().await.unwrap(), [1, 2, 3]);
}

/// Runs IDLE until the server reports a change, making `change` once the server has
/// received the `n`-th IDLE.
async fn idle_while(
    fake: &FakeImap,
    session: &mut Session,
    n: usize,
    change: impl FnOnce(&mut ServerState),
) -> catmail_proto::imap::Result<bool> {
    let (result, ()) = tokio::join!(session.idle(std::future::pending(), TIMEOUT), async {
        fake.wait_until(|st| st.received("IDLE") >= n).await;
        change(&mut fake.state());
    });
    result
}

#[tokio::test]
async fn idle_reports_changes_until_stopped() {
    let fake = FakeImap::start(three_messages()).await;
    let mut session = login(&fake).await;
    assert!(session.can_idle());
    session.examine("INBOX").await.unwrap();

    // New mail.
    let arrive = |st: &mut ServerState| {
        st.inbox().append_simple("four");
    };
    assert!(idle_while(&fake, &mut session, 1, arrive).await.unwrap());
    assert_eq!(session.changed_flags(), &ChangedFlags::default());
    assert_eq!(session.uid_search_all().await.unwrap(), [1, 2, 3, 4]);

    // Another client's flag change, then an expunge.
    session.examine("INBOX").await.unwrap();
    let flag = |st: &mut ServerState| st.inbox().set_flags(2, &["\\Seen"]);
    assert!(idle_while(&fake, &mut session, 2, flag).await.unwrap());
    // With CONDSTORE, MODSEQs tell which.
    assert_eq!(session.changed_flags(), &ChangedFlags::Unknown);
    session.examine("INBOX").await.unwrap();
    assert_eq!(session.changed_flags(), &ChangedFlags::default());
    let expunge = |st: &mut ServerState| st.inbox().expunge(1);
    assert!(idle_while(&fake, &mut session, 3, expunge).await.unwrap());

    // Nothing happens: `stop` or `max` ends it, and the connection goes on.
    session.examine("INBOX").await.unwrap();
    let stop = tokio::time::sleep(Duration::from_millis(50));
    assert!(!session.idle(stop, TIMEOUT).await.unwrap());
    assert!(
        !session
            .idle(std::future::pending(), Duration::from_millis(50))
            .await
            .unwrap()
    );
    assert_eq!(session.uid_search_all().await.unwrap(), [2, 3, 4]);
    assert_eq!(fake.state().received("DONE"), 0, "DONE is not a command");
    assert_eq!(fake.state().received("IDLE"), 5);
}

#[tokio::test]
async fn idle_names_messages_with_changed_flags_by_uid() {
    let mut state = three_messages();
    state.faults.condstore = false;
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();

    // Reported as `* 2 FETCH` after `* 1 EXPUNGE`: UID 3.
    let change = |st: &mut ServerState| {
        st.inbox().expunge(1);
        st.inbox().set_flags(3, &["\\Seen"]);
    };
    assert!(idle_while(&fake, &mut session, 1, change).await.unwrap());
    assert_eq!(session.changed_flags(), &ChangedFlags::Uids(vec![3]));
    // The answer to our own FETCH is no change.
    session.fetch_flags(&[1..=3], None).await.unwrap();
    assert_eq!(session.changed_flags(), &ChangedFlags::Uids(vec![3]));

    // A line it can't parse might have been anything.
    session.examine("INBOX").await.unwrap();
    fake.state().faults.garble_idle = true;
    assert!(session.idle(std::future::pending(), TIMEOUT).await.unwrap());
    assert_eq!(session.changed_flags(), &ChangedFlags::Unknown);
}

#[tokio::test]
async fn idle_returns_at_once_for_changes_reported_earlier() {
    let fake = FakeImap::start(three_messages()).await;
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();
    fake.state().inbox().append_simple("four");
    // The server mentions the new message in passing, with the answer to the SEARCH.
    assert_eq!(session.uid_search_all().await.unwrap(), [1, 2, 3, 4]);
    assert!(session.idle(std::future::pending(), TIMEOUT).await.unwrap());
    assert_eq!(fake.state().received("IDLE"), 0);

    // Opening the mailbox again starts afresh.
    session.examine("INBOX").await.unwrap();
    let stop = tokio::time::sleep(Duration::from_millis(50));
    assert!(!session.idle(stop, TIMEOUT).await.unwrap());
}

#[tokio::test]
async fn idle_ends_with_the_connection() {
    let fake = FakeImap::start(three_messages()).await;
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();
    let drop = ServerState::drop_open_connections;
    let err = idle_while(&fake, &mut session, 1, drop).await.unwrap_err();
    assert!(err.is_disconnected(), "{err:?}");

    // A new UIDVALIDITY ends it with BYE.
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();
    let reset = |st: &mut ServerState| st.inbox().reset_uidvalidity(9);
    let err = idle_while(&fake, &mut session, 2, reset).await.unwrap_err();
    assert!(matches!(err, Error::Bye(_)), "{err:?}");
}

#[tokio::test]
async fn idle_needs_server_support() {
    let mut state = three_messages();
    state.faults.idle = false;
    let fake = FakeImap::start(state).await;
    let mut session = login(&fake).await;
    session.examine("INBOX").await.unwrap();
    assert!(!session.can_idle());
    let err = session
        .idle(std::future::pending(), TIMEOUT)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported("IDLE")), "{err:?}");
}
