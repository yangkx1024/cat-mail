//! The real servers `testenv/dovecot/run.sh` starts, from the variables it prints, and
//! what the tests against them (`dovecot.rs`, `submission.rs`) share.
//!
//! Without those variables the tests skip; `CATMAIL_TESTENV_REQUIRED=1` turns the skip
//! into a failure (CI).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use catmail_core::{
    AccountId, Config, MailClient, MemorySecrets, MessageSummary, NewAccount, Security,
    ServerConfig, TlsConfig,
};
use tokio::runtime::Handle;

// Each test file reads its own part.
#[allow(dead_code)]
pub struct TestEnv {
    /// Plaintext logins.
    pub imap: ServerConfig,
    /// The `catmail` user's, which `dovecot.rs` works in. `submission.rs` logs in as
    /// `sender`, so the two can run at the same time.
    pub maildir: PathBuf,
    /// Dovecot's submission service, with STARTTLS and with implicit TLS.
    pub starttls: ServerConfig,
    pub tls: ServerConfig,
    /// The servers' self-signed certificate, DER.
    pub cert: Vec<u8>,
    /// mailpit's HTTP API, host:port: what the submission service relayed.
    pub mailpit: String,
}

/// `None` if the servers aren't there (the test should return).
pub fn testenv() -> Option<TestEnv> {
    let var = |name| std::env::var(name).ok();
    let (Some(imap), Some(maildir), Some(starttls), Some(tls), Some(cert), Some(mailpit)) = (
        var("CATMAIL_TEST_IMAP"),
        var("CATMAIL_TEST_MAILDIR"),
        var("CATMAIL_TEST_SUBMISSION"),
        var("CATMAIL_TEST_SUBMISSIONS"),
        var("CATMAIL_TEST_CERT"),
        var("CATMAIL_TEST_MAILPIT"),
    ) else {
        assert!(
            std::env::var_os("CATMAIL_TESTENV_REQUIRED").is_none(),
            "the CATMAIL_TEST_* variables from testenv/dovecot/run.sh are not set"
        );
        eprintln!("skipped: start testenv/dovecot/run.sh and export what it prints");
        return None;
    };
    let server = |addr: &str, security| {
        let (host, port) = addr.rsplit_once(':').expect("host:port");
        ServerConfig {
            host: host.into(),
            port: port.parse().expect("port"),
            security,
        }
    };
    Some(TestEnv {
        imap: server(&imap, Security::Plain),
        maildir: maildir.into(),
        starttls: server(&starttls, Security::StartTls),
        tls: server(&tls, Security::Tls),
        cert: std::fs::read(cert).expect("CATMAIL_TEST_CERT"),
        mailpit,
    })
}

/// A client on `dir`, trusting `tls` (or the platform) for certificates.
pub fn open_client(dir: &Path, tls: Option<TlsConfig>) -> MailClient {
    let mut builder = MailClient::builder(Config::with_data_dir(dir))
        .runtime(Handle::current())
        .secrets(Arc::new(MemorySecrets::default()));
    if let Some(tls) = tls {
        builder = builder.tls(tls);
    }
    builder.open().unwrap()
}

/// An account logging in as `user`, `user@example.com` unless `email` says otherwise.
pub async fn add_account(
    client: &MailClient,
    user: &str,
    password: &str,
    email: Option<&str>,
    display_name: Option<&str>,
    imap: ServerConfig,
    smtp: ServerConfig,
) -> AccountId {
    client
        .add_account(
            NewAccount {
                email: email.map_or_else(|| format!("{user}@example.com"), Into::into),
                display_name: display_name.map(Into::into),
                username: user.into(),
                imap,
                smtp,
            },
            password.into(),
        )
        .await
        .unwrap()
        .id
}

/// The messages in the account's Sent folder with this subject, after a sync.
pub async fn sent_copies(
    client: &MailClient,
    account: AccountId,
    subject: &str,
) -> Vec<MessageSummary> {
    client.sync_account(account).await.unwrap();
    let sent = client.find_folder(account, "Sent").await.unwrap().unwrap();
    client
        .list_messages(sent.id, u32::MAX, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|m| m.subject.as_deref() == Some(subject))
        .collect()
}

/// Tells this run's folders and messages from earlier runs' against the same servers.
pub fn run_tag() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("run{nanos}")
}
