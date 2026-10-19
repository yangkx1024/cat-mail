//! Sync against a real Dovecot started by `testenv/dovecot/run.sh`.
//!
//! Skipped unless `CATMAIL_TEST_IMAP` (host:port) and `CATMAIL_TEST_MAILDIR` are set;
//! `CATMAIL_TEST_IMAP_REQUIRED=1` turns the skip into a failure (CI).
//! The fixture folders are only read; changes happen in a fresh folder per run, written
//! straight into the maildir, so the test can be rerun against the same server.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use catmail_core::{
    AuthKind, Config, Flags, FolderRole, MailClient, MemorySecrets, NewAccount, Security,
    ServerConfig, SyncReport,
};
use tokio::runtime::Handle;

fn target() -> Option<(String, u16, PathBuf)> {
    let addr = std::env::var("CATMAIL_TEST_IMAP").ok()?;
    let maildir = std::env::var("CATMAIL_TEST_MAILDIR").ok()?;
    let (host, port) = addr
        .rsplit_once(':')
        .expect("CATMAIL_TEST_IMAP is host:port");
    Some((host.into(), port.parse().expect("port"), maildir.into()))
}

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
    let Some((host, port, maildir)) = target() else {
        assert!(
            std::env::var_os("CATMAIL_TEST_IMAP_REQUIRED").is_none(),
            "CATMAIL_TEST_IMAP is not set"
        );
        eprintln!("skipped: start testenv/dovecot/run.sh and export what it prints");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let client = MailClient::builder(Config::with_data_dir(dir.path()))
        .runtime(Handle::current())
        .secrets(Arc::new(MemorySecrets::default()))
        .open()
        .unwrap();
    let server = ServerConfig {
        host,
        port,
        security: Security::Plain,
    };
    let account = client
        .add_account(
            NewAccount {
                email: "catmail@example.com".into(),
                display_name: None,
                username: "catmail".into(),
                imap: server.clone(),
                smtp: server,
                auth: AuthKind::Password,
            },
            "catmail".into(),
        )
        .await
        .unwrap()
        .id;

    // A folder of our own for the changes, created the way Maildir++ lays it out.
    let run = format!("run{}", std::process::id());
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
    assert_eq!(added(&report, "INBOX"), (5, 0, 0));
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
    assert_eq!((inbox.unread_count, inbox.total_count), (4, 5));

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
            "测试邮件",
            "你好，世界",
            "Plain ASCII message"
        ]
    );

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

    fs::remove_dir_all(&folder_dir).unwrap();
}
