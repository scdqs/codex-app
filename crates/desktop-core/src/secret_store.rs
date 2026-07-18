use std::{collections::HashMap, sync::Mutex};

use thiserror::Error;

pub const CLOUDFLARE_TUNNEL_TOKEN_KEY: &str = "cloudflare-tunnel-token";
pub const VAPID_PRIVATE_KEY_KEY: &str = "vapid-private-key";

#[derive(Debug, Error)]
pub enum SecretStoreError {
    #[error("secret store operation failed: {0}")]
    Backend(String),
}

pub trait SecretStore: Send + Sync {
    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError>;
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError>;
    fn delete(&self, key: &str) -> Result<(), SecretStoreError>;
}

pub struct KeyringSecretStore {
    service: String,
}

impl KeyringSecretStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, SecretStoreError> {
        keyring::Entry::new(&self.service, key)
            .map_err(|error| SecretStoreError::Backend(error.to_string()))
    }
}

impl SecretStore for KeyringSecretStore {
    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        self.entry(key)?
            .set_password(value)
            .map_err(|error| SecretStoreError::Backend(error.to_string()))
    }

    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        match self.entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretStoreError::Backend(error.to_string())),
        }
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SecretStoreError::Backend(error.to_string())),
        }
    }
}

#[derive(Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        self.values
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        Ok(self.values.lock().unwrap().get(key).cloned())
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        self.values.lock().unwrap().remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{MemorySecretStore, SecretStore};

    #[test]
    fn memory_secret_store_round_trips_and_deletes_secret() {
        let store = MemorySecretStore::default();

        store
            .set("cloudflare-tunnel-token", "secret-value")
            .unwrap();
        assert_eq!(
            store.get("cloudflare-tunnel-token").unwrap().as_deref(),
            Some("secret-value")
        );

        store.delete("cloudflare-tunnel-token").unwrap();
        assert_eq!(store.get("cloudflare-tunnel-token").unwrap(), None);
    }
}
