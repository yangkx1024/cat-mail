//! Where login credentials come from.
//!
//! The sync engine and the outbox ask for [`Credentials`] and hand them to the IMAP or
//! SMTP session; they never look at how an account authenticates. Password accounts read the keyring
//! here. OAuth2 (M6) adds an [`AuthKind`] variant whose arm refreshes the access token
//! and returns an XOAUTH2/OAUTHBEARER credential, without touching sync code.

use catmail_proto::Credentials;
use catmail_types::{Account, AuthKind, NewAccount};
use secrecy::SecretString;

use crate::secrets::SecretStore;
use crate::{Error, Result};

/// For an account that isn't stored yet, from the secret the user just entered.
pub(crate) fn new_account_credentials(new: &NewAccount, secret: &SecretString) -> Credentials {
    match new.auth {
        AuthKind::Password => Credentials::Password {
            username: new.username.clone(),
            password: secret.clone(),
        },
    }
}

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
