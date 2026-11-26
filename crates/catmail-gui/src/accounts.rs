//! The accounts as the main window last read them (`MailClient::overview`), with their
//! folders, identities and status: for every part of it. The sidebar reads them and shows
//! them; the list, the commands, the panes, the settings dialog and the forms look up what
//! they need in them.

use catmail_core::AccountId;
use catmail_frontend::mailbox::AccountEntry;
use nsh_core::prelude::*;

use crate::engine::client;
use crate::tasks::in_view;
use crate::toast::report;

/// The accounts as the sidebar shows them; `None` until the first read.
#[derive(Clone, Default)]
pub struct Accounts(pub Option<Vec<AccountEntry>>);

impl Accounts {
    /// The accounts shown: none before the first read.
    pub fn list(&self) -> &[AccountEntry] {
        self.0.as_deref().unwrap_or_default()
    }
}

/// Whether there is an account to write from or sync. Tracked.
pub fn has_accounts() -> bool {
    global::<Accounts>().with(|a| !a.list().is_empty())
}

pub fn with_accounts<T>(f: impl FnOnce(&[AccountEntry]) -> T) -> T {
    global::<Accounts>().with_untracked(|a| f(a.list()))
}

/// `f` with account `id` as the sidebar has it; `None` while it has no such account.
pub fn with_account<T>(id: AccountId, f: impl FnOnce(&AccountEntry) -> T) -> Option<T> {
    with_accounts(|a| AccountEntry::find(a, id).map(f))
}

/// Whether an account is syncing every folder, as the engine says.
pub fn syncing() -> bool {
    global::<Accounts>().with(|a| a.0.iter().flatten().any(|entry| entry.status.syncing))
}

/// Syncs every account now. The engine's background sync does it: progress and failures
/// show as the accounts' status.
pub fn sync() {
    in_view(async {
        if let Err(e) = client().sync_now().await {
            report("Couldn't sync", e);
        }
    });
}
