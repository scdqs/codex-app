use std::path::Path;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::{SecretKey, elliptic_curve::sec1::ToEncodedPoint};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VapidRuntimeKeyError {
    #[error("VAPID secret file I/O failed")]
    Io,
    #[error("VAPID private key is invalid")]
    InvalidKey,
}

pub struct VapidRuntimeKey {
    private_key_base64: String,
    public_key_base64: String,
    public_key_bytes: Vec<u8>,
}

impl std::fmt::Debug for VapidRuntimeKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VapidRuntimeKey")
            .field("public_key", &"[available]")
            .field("private_key", &"[REDACTED]")
            .finish()
    }
}

impl VapidRuntimeKey {
    pub fn from_secret_file(path: &Path) -> Result<Self, VapidRuntimeKeyError> {
        let content = std::fs::read_to_string(path).map_err(|_| VapidRuntimeKeyError::Io);
        let _ = std::fs::remove_file(path);
        let private_key_base64 = content?.trim().to_string();
        let key_bytes = URL_SAFE_NO_PAD
            .decode(&private_key_base64)
            .map_err(|_| VapidRuntimeKeyError::InvalidKey)?;
        let key =
            SecretKey::from_slice(&key_bytes).map_err(|_| VapidRuntimeKeyError::InvalidKey)?;
        let public_key_bytes = key.public_key().to_encoded_point(false).as_bytes().to_vec();
        let public_key_base64 = URL_SAFE_NO_PAD.encode(&public_key_bytes);
        Ok(Self {
            private_key_base64,
            public_key_base64,
            public_key_bytes,
        })
    }

    pub fn private_key_base64(&self) -> &str {
        &self.private_key_base64
    }

    pub fn public_key_base64(&self) -> &str {
        &self.public_key_base64
    }

    pub fn public_key_bytes(&self) -> &[u8] {
        &self.public_key_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_vapid_secret_file_once_and_removes_it() {
        let dir = tempfile::tempdir().expect("tempdir creates");
        let path = dir.path().join("vapid-secret");
        let private_key = URL_SAFE_NO_PAD.encode([1_u8; 32]);
        std::fs::write(&path, &private_key).expect("fixture writes");

        let material = VapidRuntimeKey::from_secret_file(&path).expect("key loads");

        assert!(!path.exists());
        assert_eq!(material.private_key_base64(), private_key);
        assert_eq!(material.public_key_bytes().len(), 65);
        assert!(!format!("{material:?}").contains(material.private_key_base64()));
    }

    #[test]
    fn invalid_key_is_rejected_and_secret_file_is_still_removed() {
        let dir = tempfile::tempdir().expect("tempdir creates");
        let path = dir.path().join("vapid-secret");
        std::fs::write(&path, "invalid-private-key").expect("fixture writes");

        let error = VapidRuntimeKey::from_secret_file(&path).expect_err("invalid key fails");

        assert!(matches!(error, VapidRuntimeKeyError::InvalidKey));
        assert!(!path.exists());
        assert!(!error.to_string().contains("invalid-private-key"));
    }
}
