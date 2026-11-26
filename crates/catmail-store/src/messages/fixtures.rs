//! What the tests of the messages' rows start from.

use std::collections::HashMap;

use catmail_types::{
    AccountId, Address, BlobHash, FlagState, Flags, Folder, FolderId, FolderKind, FolderRole,
    GmailLabel, ListQuery, SearchQuery,
};

use super::{NewAttachment, NewBody, NewMessage};
use crate::Store;
use crate::folders::RemoteFolder;
use crate::test_util::{self, TestStore};

pub(super) fn setup() -> (TestStore, AccountId, Folder) {
    let store = test_util::store();
    let account = test_util::account(&store, "a@example.com");
    let folder = inbox(&store, account);
    (store, account, folder)
}

/// Gives the account an INBOX, its only folder.
pub(super) fn inbox(store: &Store, account: AccountId) -> Folder {
    let inbox = RemoteFolder {
        remote_name: "INBOX".into(),
        name: "INBOX".into(),
        delimiter: None,
        role: None,
        kind: FolderKind::Location,
        selectable: true,
        inferiors: true,
    };
    store
        .write(|db| db.reconcile_folders(account, &[inbox]))
        .unwrap();
    store
        .read(|db| db.folders().list(account))
        .unwrap()
        .remove(0)
}

pub(super) fn message(uid: u32, date: Option<i64>, flags: Flags) -> NewMessage {
    NewMessage {
        state: FlagState {
            uid,
            flags,
            ..Default::default()
        },
        subject: Some(format!("subject {uid}")),
        from: vec![Address {
            name: Some("Alice".into()),
            email: "alice@example.com".into(),
        }],
        date,
        internal_date: Some(1_000 + uid as i64),
        ..Default::default()
    }
}

pub(super) fn hash(c: char) -> BlobHash {
    BlobHash::parse(&c.to_string().repeat(64)).unwrap()
}

pub(super) fn body() -> NewBody {
    NewBody {
        text: Some("请查收第三季度报告。\nThe numbers are final.".into()),
        html: Some("<p>请查收</p>".into()),
        raw: Some(hash('a')),
        snippet: Some("请查收第三季度报告。".into()),
        attachments: vec![
            NewAttachment {
                section: "2".into(),
                transfer_encoding: Some("base64".into()),
                filename: Some("报告.pdf".into()),
                mime_type: "application/pdf".into(),
                size: Some(10),
                blob: Some(hash('b')),
                ..Default::default()
            },
            NewAttachment {
                section: "3".into(),
                mime_type: "image/png".into(),
                content_id: Some("logo@x".into()),
                inline: true,
                ..Default::default()
            },
        ],
        version: 1,
    }
}

pub(super) fn search(store: &Store, text: &str) -> Vec<Option<String>> {
    store
        .read(|db| db.messages().search(&SearchQuery::new(text)))
        .unwrap()
        .into_iter()
        .map(|m| m.subject)
        .collect()
}

/// The account's folders by name, after creating them with these roles (and
/// UIDVALIDITY 1).
pub(super) fn folders(
    store: &Store,
    account: AccountId,
    roles: &[(&str, Option<FolderRole>)],
) -> HashMap<String, FolderId> {
    let remote: Vec<_> = roles
        .iter()
        .map(|&(name, role)| RemoteFolder {
            remote_name: name.into(),
            name: name.into(),
            delimiter: None,
            role,
            kind: FolderKind::Location,
            selectable: true,
            inferiors: true,
        })
        .collect();
    store
        .write(|db| {
            db.reconcile_folders(account, &remote)?;
            for folder in db.folders().list(account)? {
                db.reset_uidvalidity(folder.id, 1)?;
            }
            Ok(())
        })
        .unwrap();
    let list = store.read(|db| db.folders().list(account)).unwrap();
    list.into_iter().map(|f| (f.name, f.id)).collect()
}

/// A message dated by its UID, with this Message-ID.
pub(super) fn copy(uid: u32, message_id: Option<&str>) -> NewMessage {
    NewMessage {
        message_id: message_id.map(Into::into),
        ..message(uid, Some(uid as i64), Flags::empty())
    }
}

pub(super) fn user(name: &str) -> GmailLabel {
    GmailLabel::user(name, name)
}

/// A Gmail message dated by its UID, its X-GM-MSGID 100 more.
pub(super) fn labelled(uid: u32, flags: Flags, labels: Vec<GmailLabel>) -> NewMessage {
    let mut m = message(uid, Some(uid as i64), flags);
    m.state.labels = Some(labels);
    m.gm_msgid = Some(100 + uid as u64);
    m
}

pub(super) fn subjects(store: &Store, folder: FolderId) -> Vec<String> {
    store
        .read(|db| db.messages().list(folder, &ListQuery::new(50)))
        .unwrap()
        .into_iter()
        .map(|m| m.subject.unwrap())
        .collect()
}
