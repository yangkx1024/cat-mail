//! Where login credentials come from.
//!
//! The sync engine asks for [`Credentials`] and hands them to the protocol session; it
//! never looks at how an account authenticates. Password accounts read the keyring
//! here. OAuth2 (M6) adds an [`AuthKind`] variant whose arm refreshes the access token
//! and returns an XOAUTH2/OAUTHBEARER credential, without touching sync code.

use catmail_proto::imap::Credentials;
use catmail_types::{Account, AuthKind};

use crate::secrets::SecretStore;
use crate::{Error, Result};

/// Blocking: may talk to the OS keyring.
pub(crate) fn credentials(
    account: &Account,
    secret_key: &str,
    secrets: &dyn SecretStore,
) -> Result<Credentials> {
    match account.auth {
        AuthKind::Password => {
            let password = secrets.get(secret_key)?.ok_or_else(|| {
                Error::Secrets(format!(
                    "no stored password for {}; remove and re-add the account",
                    account.email
                ))
            })?;
            Ok(Credentials::Password {
                username: account.username.clone(),
                password,
            })
        }
    }
}
