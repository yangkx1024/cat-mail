use serde::{Deserialize, Serialize};

use crate::{AccountId, FolderId, MessageId, OutboxId};

/// Change notifications broadcast by the engine. Frontends subscribe and re-query the
/// affected data instead of receiving full payloads, so a lagging subscriber can always
/// recover by refreshing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    AccountAdded {
        account: AccountId,
    },
    AccountRemoved {
        account: AccountId,
    },
    /// Whether the engine gets through to the account's servers changed; see
    /// `MailClient::account_status`.
    AccountStatusChanged {
        account: AccountId,
    },
    /// The account changed: identities were added to it, removed or changed, or what is
    /// known of its server (whether it is Gmail's, its mode), which
    /// `Identity::server_saves_sent` and `Account::all_mail_hidden` go by.
    AccountChanged {
        account: AccountId,
    },
    /// Folders were added, removed or renamed.
    FoldersChanged {
        account: AccountId,
    },
    /// Messages in the folder were added, removed or changed, and with them possibly the
    /// folder's unread and total counts.
    MessagesChanged {
        account: AccountId,
        folder: FolderId,
    },
    /// Mail arrived in the account's INBOX, unread: messages the server got since the
    /// last sync, not older ones fetched late. Sent along with `MessagesChanged`, after
    /// the messages are stored; none for a folder synced for the first time or since
    /// its UIDVALIDITY changed. They may have been read or moved since: look them up
    /// before telling the user.
    NewMail {
        account: AccountId,
        messages: Vec<MessageId>,
    },
    /// Drafts were created, changed, queued for sending or deleted.
    DraftsChanged {
        account: AccountId,
    },
    /// Entries in the outbox changed state, appeared or went away.
    OutboxChanged {
        account: AccountId,
    },
    /// Changes to messages were queued for the server, made there, refused, or became
    /// conflicts; see `MailClient::list_pending`.
    PendingOpsChanged {
        account: AccountId,
    },
    /// Some of the account's queued changes were refused by the server or became
    /// conflicts, and were undone locally. Sent along with `PendingOpsChanged`, only when
    /// that happened; see `MailClient::dismiss_settled`.
    ChangesRefused {
        account: AccountId,
    },
    /// The server accepted a message for delivery.
    MessageSent {
        account: AccountId,
        outbox: OutboxId,
    },
}
