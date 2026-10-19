use serde::{Deserialize, Serialize};

use crate::{AccountId, FolderId, OutboxId};

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
    /// Drafts were created, changed, queued for sending or deleted.
    DraftsChanged {
        account: AccountId,
    },
    /// Entries in the outbox changed state, appeared or went away.
    OutboxChanged {
        account: AccountId,
    },
    /// The server accepted a message for delivery.
    MessageSent {
        account: AccountId,
        outbox: OutboxId,
    },
}
