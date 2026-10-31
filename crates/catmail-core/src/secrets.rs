use std::collections::HashMap;
use std::sync::Mutex;

use secrecy::{ExposeSecret, SecretString};

use crate::{Error, Result};

/// Storage for account passwords. Calls may block.
pub trait SecretStore: Send + Sync {
    fn set(&self, key: &str, secret: &SecretString) -> Result<()>;
    fn get(&self, key: &str) -> Result<Option<SecretString>>;
    /// Deleting a missing key is not an error.
    fn delete(&self, key: &str) -> Result<()>;
}

/// The OS credential store: Secret Service on Linux, Keychain on macOS,
/// Credential Manager on Windows.
pub struct KeyringSecrets {
    service: String,
}

impl KeyringSecrets {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, key).map_err(keyring_error)
    }
}

impl Default for KeyringSecrets {
    fn default() -> Self {
        Self::new("catmail")
    }
}

impl SecretStore for KeyringSecrets {
    fn set(&self, key: &str, secret: &SecretString) -> Result<()> {
        self.entry(key)?
            .set_password(secret.expose_secret())
            .map_err(keyring_error)
    }

    fn get(&self, key: &str) -> Result<Option<SecretString>> {
        match self.entry(key)?.get_password() {
            Ok(secret) => Ok(Some(secret.into())),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keyring_error(e)),
        }
    }

    fn delete(&self, key: &str) -> Result<()> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keyring_error(e)),
        }
    }
}

fn keyring_error(e: keyring::Error) -> Error {
    let hint = match e {
        keyring::Error::NoDefaultStore | keyring::Error::NoStorageAccess(_) => {
            " (is a Secret Service provider such as gnome-keyring or KeePassXC running?)"
        }
        _ => "",
    };
    Error::Secrets(format!("{e}{hint}"))
}

/// In-process secret store. Nothing is persisted; meant for tests.
#[derive(Default)]
pub struct MemorySecrets {
    secrets: Mutex<HashMap<String, SecretString>>,
}

impl MemorySecrets {
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.secrets.lock().unwrap().len()
    }
}

impl SecretStore for MemorySecrets {
    fn set(&self, key: &str, secret: &SecretString) -> Result<()> {
        self.secrets
            .lock()
            .unwrap()
            .insert(key.into(), secret.clone());
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<SecretString>> {
        Ok(self.secrets.lock().unwrap().get(key).cloned())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.secrets.lock().unwrap().remove(key);
        Ok(())
    }
}
