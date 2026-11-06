//! Settings › Accounts without its widgets: what removing an account or a sending address
//! asks, and what the forms for an address start with.

use catmail_core::{Identity, IdentityId};

use crate::commands::Confirm;
use crate::mailbox::AccountEntry;

/// Removing the account, asked first.
pub fn remove_account(entry: &AccountEntry) -> Confirm {
    Confirm {
        title: format!("Remove “{}”?", entry.account.email),
        description: "Its mail, drafts and outbox are deleted from this computer, and its \
                      password from the keyring. Nothing changes on the server."
            .into(),
    }
}

/// Removing one of the account's addresses, asked first; `None` if it is gone.
pub fn remove_identity(entry: &AccountEntry, id: IdentityId) -> Option<Confirm> {
    let identity = identity(entry, id)?;
    // The default is listed first.
    let default = entry
        .identities
        .first()
        .map_or(&entry.account.email, |i| &i.email);
    Some(Confirm {
        title: format!("Remove “{}”?", identity.email),
        description: format!(
            "Drafts from it will be sent from {default} instead, and its password is deleted \
             from the keyring."
        ),
    })
}

pub fn identity(entry: &AccountEntry, id: IdentityId) -> Option<&Identity> {
    entry.identities.iter().find(|i| i.id == id)
}

/// The title of the dialog that renames the address.
pub fn rename_title(identity: &Identity) -> String {
    format!("Name for {}", identity.email)
}

/// The title of the form for another address of the account.
pub fn new_identity_title(entry: &AccountEntry) -> String {
    format!("New address for {}", entry.account.email)
}

/// The name a new address of the account starts with: the default's, most often the
/// user's own.
pub fn new_identity_name(entry: &AccountEntry) -> Option<String> {
    entry
        .identities
        .iter()
        .find(|i| i.is_default)
        .and_then(|i| i.display_name.clone())
}
