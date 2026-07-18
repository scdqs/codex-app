use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::{
    SecretKey,
    elliptic_curve::{rand_core::OsRng, sec1::ToEncodedPoint},
};
use thiserror::Error;

use crate::{SecretStore, SecretStoreError, VAPID_PRIVATE_KEY_KEY};

#[derive(Clone, PartialEq, Eq)]
pub struct VapidKeyMaterial {
    pub private_key_base64: String,
    pub public_key_base64: String,
}

impl std::fmt::Debug for VapidKeyMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VapidKeyMaterial")
            .field("public_key", &"[available]")
            .field("private_key", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum VapidKeyError {
    #[error("VAPID Keychain operation failed: {0}")]
    SecretStore(#[from] SecretStoreError),
    #[error("stored VAPID private key is invalid")]
    InvalidStoredKey,
}

pub struct VapidKeyManager {
    secrets: Arc<dyn SecretStore>,
}

impl VapidKeyManager {
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self { secrets }
    }

    pub fn load_or_create(&self) -> Result<VapidKeyMaterial, VapidKeyError> {
        let private_key_base64 = match self.secrets.get(VAPID_PRIVATE_KEY_KEY)? {
            Some(value) => value,
            None => {
                let key = SecretKey::random(&mut OsRng);
                let value = URL_SAFE_NO_PAD.encode(key.to_bytes());
                self.secrets.set(VAPID_PRIVATE_KEY_KEY, &value)?;
                value
            }
        };
        let key_bytes = URL_SAFE_NO_PAD
            .decode(&private_key_base64)
            .map_err(|_| VapidKeyError::InvalidStoredKey)?;
        let key = SecretKey::from_slice(&key_bytes).map_err(|_| VapidKeyError::InvalidStoredKey)?;
        let public_key_base64 =
            URL_SAFE_NO_PAD.encode(key.public_key().to_encoded_point(false).as_bytes());
        Ok(VapidKeyMaterial {
            private_key_base64,
            public_key_base64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemorySecretStore;

    #[test]
    fn key_manager_generates_once_and_reuses_keychain_value() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
        let manager = VapidKeyManager::new(Arc::clone(&secrets));

        let first = manager.load_or_create().expect("first key loads");
        let second = manager.load_or_create().expect("stored key loads");

        assert_eq!(first, second);
        assert_eq!(first.private_key_base64.len(), 43);
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(&first.public_key_base64)
                .expect("public key decodes")
                .len(),
            65
        );
        assert_ne!(first.private_key_base64, first.public_key_base64);
        assert!(!format!("{first:?}").contains(&first.private_key_base64));
    }

    #[test]
    fn invalid_stored_key_is_rejected_without_echoing_secret() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
        secrets
            .set(VAPID_PRIVATE_KEY_KEY, "invalid-private-key")
            .expect("fixture saves");
        let manager = VapidKeyManager::new(secrets);

        let error = manager.load_or_create().expect_err("invalid key fails");

        assert!(matches!(error, VapidKeyError::InvalidStoredKey));
        assert!(!error.to_string().contains("invalid-private-key"));
    }
}
