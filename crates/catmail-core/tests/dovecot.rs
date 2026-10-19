//! Sync against a real Dovecot started by `testenv/dovecot/run.sh` (see `testenv`).
//!
//! The fixture folders are only read; changes happen in a fresh folder per run, written
//! straight into the maildir, and in Sent, where the test finds its message by subject,
//! so it can be rerun against the same server.

mod testenv;

use std::fs;
use std::path::{Path, PathBuf};

use catmail_core::{
    AccountId, Address, DraftContent, Flags, FolderRole, MailClient, MessageSummary, OutboxStatus,
    SearchQuery, Security, ServerConfig, SyncReport,
};
use catmail_testkit::{FakeSmtp, SmtpState};
use testenv::{add_account, open_client, run_tag, sent_copies};

fn message(subject: &str) -> String {
    format!(
        "From: Test <test@example.com>\r\nSubject: {subject}\r\n\
         Date: Sun, 18 Oct 2026 12:00:00 +0000\r\n\r\nbody\r\n"
    )
}

fn added(report: &SyncReport, name: &str) -> (u32, u32, u32) {
    let f = report
        .folders
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{name} not synced: {report:?}"));
    assert_eq!(f.error, None);
    (f.added, f.updated, f.removed)
}

#[tokio::test(flavor = "multi_thread")]
async fn syncs_against_dovecot() {
    let Some(env) = testenv::testenv() else {
        return;
    };
    let (server, maildir) = (env.imap, env.maildir);
    let dir = tempfile::tempdir().unwrap();
    let client = open_client(dir.path(), None);
    let account = add_account(
        &client,
        "catmail",
        "catmail",
        None,
        None,
        server.clone(),
        server.clone(),
    )
    .await;

    // A folder of our own for the changes, created the way Maildir++ lays it out.
    let run = run_tag();
    let folder_dir = maildir.join(format!(".{run}"));
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(folder_dir.join(sub)).unwrap();
    }
    let deliver = |dir: &Path, name: &str, subject: &str| {
        fs::write(dir.join("new").join(name), message(subject)).unwrap();
    };
    deliver(&folder_dir, "1.a", "first");
    deliver(&folder_dir, "2.b", "second");

    let report = client.sync_account(account).await.unwrap();
    assert_eq!(added(&report, "INBOX"), (6, 0, 0));
    assert_eq!(added(&report, &run), (2, 0, 0));

    let folders = client.list_folders(account).await.unwrap();
    let find = |name: &str| folders.iter().find(|f| f.name == name).unwrap();
    assert_eq!(find("INBOX").role, Some(FolderRole::Inbox));
    assert_eq!(find("Sent").role, Some(FolderRole::Sent), "SPECIAL-USE");
    assert_eq!(find("Trash").role, Some(FolderRole::Trash));
    assert_eq!(
        find("垃圾邮件").role,
        Some(FolderRole::Junk),
        "by name, decoded from UTF-7"
    );
    assert_eq!(find("Archive/2026").total_count, 1);
    assert_eq!(find("Archive/2026").delimiter, Some('/'));
    let inbox = find("INBOX");
    assert_eq!((inbox.unread_count, inbox.total_count), (5, 6));

    let messages = client.list_messages(inbox.id, 10, 0).await.unwrap();
    let rows: Vec<_> = messages
        .iter()
        .map(|m| {
            (
                m.subject.clone().unwrap_or_default(),
                m.from.first().and_then(|a| a.name.clone()),
                m.flags,
                m.has_attachments,
            )
        })
        .collect();
    let row = |subject: &str| {
        rows.iter()
            .find(|r| r.0 == subject)
            .unwrap_or_else(|| panic!("{subject}: {rows:?}"))
    };
    assert_eq!(row("你好，世界").1.as_deref(), Some("张三"));
    assert_eq!(row("你好，世界").2, Flags::FLAGGED);
    assert_eq!(
        row("测试邮件").1.as_deref(),
        Some("李娜"),
        "GB2312 encoded words"
    );
    assert_eq!(row("Plain ASCII message").2, Flags::SEEN);
    assert!(row("Quarterly report").3);
    assert!(!row("Plain ASCII message").3);
    // Sorted by Date; the message without one uses its INTERNALDATE (2026-10-18).
    let order: Vec<_> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        order,
        [
            "No Date header",
            "Quarterly report",
            "月度报表",
            "测试邮件",
            "你好，世界",
            "Plain ASCII message"
        ]
    );

    check_bodies(&client, &messages).await;

    let report = client.sync_account(account).await.unwrap();
    assert!(
        report
            .folders
            .iter()
            .all(|f| (f.added, f.updated, f.removed) == (0, 0, 0)),
        "nothing changed: {report:?}"
    );

    // New mail, a flag change (Maildir flags live in the file name) and a deletion.
    deliver(&folder_dir, "3.c", "third");
    // EXAMINE is read-only, so Dovecot may or may not have moved them to cur/ yet.
    let locate = |prefix: &str| -> PathBuf {
        ["new", "cur"]
            .iter()
            .flat_map(|sub| fs::read_dir(folder_dir.join(sub)).unwrap())
            .map(|e| e.unwrap().path())
            .find(|p| p.file_name().unwrap().to_string_lossy().starts_with(prefix))
            .unwrap_or_else(|| panic!("no maildir file {prefix}"))
    };
    fs::rename(locate("1.a"), folder_dir.join("cur").join("1.a:2,S")).unwrap();
    fs::remove_file(locate("2.b")).unwrap();

    let report = client.sync_account(account).await.unwrap();
    assert_eq!(added(&report, &run), (1, 1, 1));
    let folder = client.find_folder(account, &run).await.unwrap().unwrap();
    let messages = client.list_messages(folder.id, 10, 0).await.unwrap();
    let rows: Vec<_> = messages
        .iter()
        .map(|m| (m.subject.clone().unwrap(), m.flags))
        .collect();
    assert_eq!(
        rows,
        [
            ("third".into(), Flags::empty()),
            ("first".into(), Flags::SEEN)
        ]
    );

    // Over the whole-message limit: fetched as BODYSTRUCTURE plus text parts, with the
    // attachment (named in RFC 2231 form) left on the server until opened.
    let video = vec![7u8; 3 * 1024 * 1024];
    fs::write(folder_dir.join("new").join("4.d"), large_message(&video)).unwrap();
    client.sync_account(account).await.unwrap();
    let big = client.list_messages(folder.id, 10, 0).await.unwrap()[0].clone();
    assert_eq!(big.subject.as_deref(), Some("large"));
    let message = client.download_message(big.id).await.unwrap();
    let body = message.body.unwrap();
    assert_eq!(body.text.as_deref().map(str::trim), Some("See the clip."));
    let clip = &body.attachments[0];
    assert_eq!(clip.filename.as_deref(), Some("视频.mp4"));
    assert!(!clip.downloaded);
    assert_eq!(client.attachment_content(clip.id).await.unwrap(), video);

    fs::remove_dir_all(&folder_dir).unwrap();

    check_saving_to_sent(&client, account, server, &maildir, &run).await;
}

/// Sends through the fake SMTP server and saves to Dovecot's Sent folder (sending through
/// Dovecot is `submission.rs`; here saving is what matters, sent exactly once). The first
/// APPEND fails, so the second one first searches Sent for the message, against a real
/// server too. The failure comes from a read-only directory, so this can't run as root.
async fn check_saving_to_sent(
    client: &MailClient,
    reader: AccountId,
    imap: ServerConfig,
    maildir: &Path,
    run: &str,
) {
    let fake = FakeSmtp::start(SmtpState::new("catmail", "catmail")).await;
    // Never synced: the Sent folder comes from LIST.
    let smtp = ServerConfig {
        host: "127.0.0.1".into(),
        port: fake.port(),
        security: Security::Plain,
    };
    let sender = add_account(
        client,
        "catmail",
        "catmail",
        Some("sender@example.com"),
        Some("发件人"),
        imap,
        smtp,
    )
    .await;
    let subject = format!("sent {run}");
    let draft = client
        .create_draft(
            sender,
            DraftContent {
                subject: subject.clone(),
                to: vec![Address {
                    name: None,
                    email: "bob@example.com".into(),
                }],
                text: "发送测试".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    client.send_draft(draft.id).await.unwrap();

    let tmp = maildir.join(".Sent").join("tmp");
    let writable = fs::metadata(&tmp).unwrap().permissions();
    let mut readonly = writable.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&tmp, readonly).unwrap();
    let first = client.flush_outbox(sender).await;
    fs::set_permissions(&tmp, writable).unwrap();
    let first = first.unwrap();
    assert_eq!(first[0].status, OutboxStatus::Accepted, "{first:?}");
    assert!(first[0].last_error.is_some());
    assert_eq!(fake.state().received.len(), 1);

    let second = client.flush_outbox(sender).await.unwrap();
    assert_eq!(
        (second[0].status, second[0].attempts),
        (OutboxStatus::Done, 2),
        "{second:?}"
    );
    assert_eq!(fake.state().received.len(), 1, "sent once");

    let copies = sent_copies(client, reader, &subject).await;
    assert_eq!(copies.len(), 1, "saved once");
    assert!(copies[0].flags.contains(Flags::SEEN));
    let from = &copies[0].from[0];
    assert_eq!(
        (from.name.as_deref(), from.email.as_str()),
        (Some("发件人"), "sender@example.com")
    );
}

fn large_message(attachment: &[u8]) -> Vec<u8> {
    use base64::Engine;
    let mut raw = b"From: Test <test@example.com>\r\nSubject: large\r\n\
        Date: Sun, 18 Oct 2026 13:00:00 +0000\r\n\
        Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
        --b\r\nContent-Type: text/plain\r\n\r\nSee the clip.\r\n\
        --b\r\nContent-Type: video/mp4\r\n\
        Content-Disposition: attachment; filename*=utf-8''%E8%A7%86%E9%A2%91.mp4\r\n\
        Content-Transfer-Encoding: base64\r\n\r\n"
        .to_vec();
    let encoded = base64::prelude::BASE64_STANDARD.encode(attachment);
    for line in encoded.as_bytes().chunks(76) {
        raw.extend_from_slice(line);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"--b--\r\n");
    raw
}

/// Downloads the fixture bodies (all under the whole-message limit) and searches them.
async fn check_bodies(client: &MailClient, inbox: &[MessageSummary]) {
    let id = |subject: &str| {
        inbox
            .iter()
            .find(|m| m.subject.as_deref() == Some(subject))
            .unwrap()
            .id
    };
    assert!(
        client
            .search(SearchQuery::new("请查收"))
            .await
            .unwrap()
            .is_empty()
    );

    let report = client
        .download_message(id("Quarterly report"))
        .await
        .unwrap();
    let body = report.body.unwrap();
    assert_eq!(body.text.as_deref().map(str::trim), Some("See attached."));
    let pdf = &body.attachments[0];
    assert_eq!(pdf.filename.as_deref(), Some("report.pdf"));
    assert_eq!(
        client.attachment_content(pdf.id).await.unwrap(),
        b"%PDF-1.4\n"
    );

    let html = client.download_message(id("月度报表")).await.unwrap();
    let body = html.body.unwrap();
    assert_eq!(body.text.as_deref(), Some("请查收附件。"), "GBK, base64");
    let markup = body.html.unwrap();
    assert!(
        markup.contains("请查收附件。"),
        "quoted-printable: {markup}"
    );
    assert!(markup.contains(r#"<table width="600">"#), "{markup}");
    assert!(markup.contains(r#"src="cid:logo@example.com""#), "{markup}");
    assert!(!markup.contains("script"), "{markup}");
    let parts: Vec<_> = body
        .attachments
        .iter()
        .map(|a| (a.filename.as_deref(), a.mime_type.as_str(), a.inline))
        .collect();
    assert_eq!(
        parts,
        [
            (None, "image/png", true),
            (Some("报表.csv"), "text/csv", false)
        ]
    );
    assert!(html.summary.has_attachments);
    let csv = client
        .attachment_content(body.attachments[1].id)
        .await
        .unwrap();
    assert_eq!(csv, b"a,b\r\n1,2");

    let gb2312 = client.download_message(id("测试邮件")).await.unwrap();
    assert_eq!(
        gb2312.body.unwrap().text.as_deref().map(str::trim),
        Some("test")
    );

    let found: Vec<_> = client
        .search(SearchQuery::new("请查收"))
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.subject.unwrap())
        .collect();
    assert_eq!(found, ["月度报表"]);
    let found = client
        .search(SearchQuery::new("from:张三 报表"))
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
}
