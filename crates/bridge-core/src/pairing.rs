use std::collections::HashMap;

use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::storage::{Device, Storage};

pub const DEFAULT_PAIRING_TOKEN_TTL_MS: u64 = 5 * 60 * 1000;
pub const DEFAULT_SESSION_TOKEN_TTL_MS: u64 = 24 * 60 * 60 * 1000;

type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRegistration {
    pub device_id: String,
    pub session_token: String,
    pub session_expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PairingError {
    #[error("invalid token")]
    InvalidToken,
    #[error("expired token")]
    ExpiredToken,
    #[error("token already used")]
    TokenAlreadyUsed,
    #[error("device revoked")]
    DeviceRevoked,
    #[error("device not found")]
    DeviceNotFound,
}

#[derive(Debug, Clone)]
struct PairingToken {
    expires_at: u64,
    used: bool,
}

#[derive(Debug, Clone)]
struct SessionToken {
    device_id: String,
    expires_at: u64,
}

pub struct PairingManager {
    storage: Storage,
    now_ms: Clock,
    pairing_token_ttl_ms: u64,
    session_token_ttl_ms: u64,
    pairing_tokens: HashMap<String, PairingToken>,
    session_tokens: HashMap<String, SessionToken>,
}

impl PairingManager {
    pub fn new(storage: Storage) -> Self {
        Self::with_clock(storage, current_time_ms)
    }

    pub fn with_clock(storage: Storage, now_ms: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        Self::with_clock_and_ttls(
            storage,
            now_ms,
            DEFAULT_PAIRING_TOKEN_TTL_MS,
            DEFAULT_SESSION_TOKEN_TTL_MS,
        )
    }

    pub fn with_clock_and_ttls(
        storage: Storage,
        now_ms: impl Fn() -> u64 + Send + Sync + 'static,
        pairing_token_ttl_ms: u64,
        session_token_ttl_ms: u64,
    ) -> Self {
        Self {
            storage,
            now_ms: Box::new(now_ms),
            pairing_token_ttl_ms,
            session_token_ttl_ms,
            pairing_tokens: HashMap::new(),
            session_tokens: HashMap::new(),
        }
    }

    pub fn create_token(&mut self) -> Result<String, PairingError> {
        let token = Uuid::new_v4().to_string();
        let expires_at = self.now() + self.pairing_token_ttl_ms;
        self.pairing_tokens.insert(
            token.clone(),
            PairingToken {
                expires_at,
                used: false,
            },
        );

        Ok(token)
    }

    pub fn register_device(
        &mut self,
        pairing_token: &str,
        device_id: &str,
        display_name: &str,
        device_secret: &str,
    ) -> Result<DeviceRegistration, PairingError> {
        self.register_device_with_origin(
            pairing_token,
            device_id,
            display_name,
            device_secret,
            None,
        )
    }

    pub fn register_device_with_origin(
        &mut self,
        pairing_token: &str,
        device_id: &str,
        display_name: &str,
        device_secret: &str,
        paired_origin: Option<String>,
    ) -> Result<DeviceRegistration, PairingError> {
        let now = self.now();
        let token = self
            .pairing_tokens
            .get(pairing_token)
            .ok_or(PairingError::InvalidToken)?;

        if token.used {
            return Err(PairingError::TokenAlreadyUsed);
        }
        if now >= token.expires_at {
            return Err(PairingError::ExpiredToken);
        }

        let device = Device {
            device_id: device_id.to_string(),
            display_name: display_name.to_string(),
            secret_hash: hash_secret(device_secret),
            paired_origin,
            created_at: now,
            last_seen_at: now,
            revoked_at: None,
        };
        self.storage
            .insert_device(&device)
            .map_err(|_| PairingError::InvalidToken)?;
        self.pairing_tokens
            .get_mut(pairing_token)
            .ok_or(PairingError::InvalidToken)?
            .used = true;
        let (session_token, session_expires_at) = self.mint_session_token_for_device_id(device_id);

        Ok(DeviceRegistration {
            device_id: device_id.to_string(),
            session_token,
            session_expires_at,
        })
    }

    pub fn create_session_token(
        &mut self,
        device_id: &str,
        device_secret: &str,
    ) -> Result<String, PairingError> {
        self.create_session(device_id, device_secret)
            .map(|registration| registration.session_token)
    }

    pub fn create_session(
        &mut self,
        device_id: &str,
        device_secret: &str,
    ) -> Result<DeviceRegistration, PairingError> {
        match self.device_by_id(device_id)? {
            Some(device) if device.revoked_at.is_some() => Err(PairingError::DeviceRevoked),
            Some(device) => {
                if device.secret_hash != hash_secret(device_secret) {
                    return Err(PairingError::InvalidToken);
                }

                let (session_token, session_expires_at) =
                    self.mint_session_token_for_device_id(device_id);
                Ok(DeviceRegistration {
                    device_id: device_id.to_string(),
                    session_token,
                    session_expires_at,
                })
            }
            None => Err(PairingError::DeviceNotFound),
        }
    }

    pub fn validate_session_token(&self, session_token: &str) -> Result<String, PairingError> {
        let token = self
            .session_tokens
            .get(session_token)
            .ok_or(PairingError::InvalidToken)?;

        if self.now() >= token.expires_at {
            return Err(PairingError::ExpiredToken);
        }

        match self.device_by_id(&token.device_id)? {
            Some(device) if device.revoked_at.is_some() => Err(PairingError::DeviceRevoked),
            Some(_) => Ok(token.device_id.clone()),
            None => Err(PairingError::DeviceNotFound),
        }
    }

    pub fn revoke_device(&self, device_id: &str) -> Result<(), PairingError> {
        match self.device_by_id(device_id)? {
            Some(_) => self
                .storage
                .revoke_device(device_id, self.now())
                .map_err(|_| PairingError::DeviceNotFound),
            None => Err(PairingError::DeviceNotFound),
        }
    }

    pub fn active_devices(&self) -> Result<Vec<Device>, PairingError> {
        self.storage
            .active_devices()
            .map_err(|_| PairingError::DeviceNotFound)
    }

    fn now(&self) -> u64 {
        (self.now_ms)()
    }

    fn device_by_id(&self, device_id: &str) -> Result<Option<Device>, PairingError> {
        self.storage
            .device_by_id(device_id)
            .map_err(|_| PairingError::DeviceNotFound)
    }

    fn mint_session_token_for_device_id(&mut self, device_id: &str) -> (String, u64) {
        let token = Uuid::new_v4().to_string();
        let expires_at = self.now() + self.session_token_ttl_ms;
        self.session_tokens.insert(
            token.clone(),
            SessionToken {
                device_id: device_id.to_string(),
                expires_at,
            },
        );

        (token, expires_at)
    }
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time is after unix epoch")
        .as_millis() as u64
}

fn hash_secret(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };
    use tempfile::{TempDir, tempdir};

    fn temp_storage() -> (TempDir, Storage) {
        let dir = tempdir().expect("tempdir is created");
        let path: PathBuf = dir.path().join("bridge.sqlite");
        let storage = Storage::open(path).expect("storage opens");

        (dir, storage)
    }

    fn manager_at(now: Arc<AtomicU64>, storage: Storage) -> PairingManager {
        PairingManager::with_clock(storage, move || now.load(Ordering::SeqCst))
    }

    fn register_phone(manager: &mut PairingManager) -> DeviceRegistration {
        let token = manager.create_token().expect("pairing token creates");

        manager
            .register_device(&token, "phone-1", "Damon's phone", "phone-secret")
            .expect("device registers")
    }

    #[test]
    fn pairing_token_can_only_be_used_once() {
        let (_dir, storage) = temp_storage();
        let now = Arc::new(AtomicU64::new(1_725_000_000_000));
        let mut manager = manager_at(Arc::clone(&now), storage);
        let token = manager.create_token().expect("pairing token creates");

        let registration = manager
            .register_device(&token, "phone-1", "Damon's phone", "phone-secret")
            .expect("device registers");

        assert_eq!(registration.device_id, "phone-1");
        assert!(
            manager
                .validate_session_token(&registration.session_token)
                .is_ok()
        );
        assert_eq!(
            manager.register_device(&token, "phone-2", "Spare phone", "other-secret"),
            Err(PairingError::TokenAlreadyUsed)
        );
    }

    #[test]
    fn expired_pairing_token_is_rejected() {
        let (_dir, storage) = temp_storage();
        let now = Arc::new(AtomicU64::new(1_725_000_000_000));
        let mut manager = manager_at(Arc::clone(&now), storage);
        let token = manager.create_token().expect("pairing token creates");

        now.store(
            1_725_000_000_000 + DEFAULT_PAIRING_TOKEN_TTL_MS + 1,
            Ordering::SeqCst,
        );

        assert_eq!(
            manager.register_device(&token, "phone-1", "Damon's phone", "phone-secret"),
            Err(PairingError::ExpiredToken)
        );
    }

    #[test]
    fn revoked_device_cannot_create_session() {
        let (_dir, storage) = temp_storage();
        let now = Arc::new(AtomicU64::new(1_725_000_000_000));
        let mut manager = manager_at(Arc::clone(&now), storage);

        register_phone(&mut manager);
        manager
            .revoke_device("phone-1")
            .expect("device revokes through pairing manager");

        assert_eq!(
            manager.create_session_token("phone-1", "phone-secret"),
            Err(PairingError::DeviceRevoked)
        );
    }

    #[test]
    fn new_pairing_token_can_rebind_existing_device() {
        let (_dir, storage) = temp_storage();
        let now = Arc::new(AtomicU64::new(1_725_000_000_000));
        let mut manager = manager_at(Arc::clone(&now), storage);
        let first_token = manager.create_token().expect("first token creates");
        manager
            .register_device(&first_token, "phone-1", "Damon's phone", "old-secret")
            .expect("device registers");
        let second_token = manager.create_token().expect("second token creates");

        let registration = manager
            .register_device(&second_token, "phone-1", "Damon's phone", "new-secret")
            .expect("device rebinds");

        assert_eq!(registration.device_id, "phone-1");
        assert_eq!(
            manager.create_session_token("phone-1", "old-secret"),
            Err(PairingError::InvalidToken)
        );
        assert!(
            manager
                .create_session_token("phone-1", "new-secret")
                .is_ok()
        );
    }

    #[test]
    fn expired_session_token_is_rejected() {
        let (_dir, storage) = temp_storage();
        let now = Arc::new(AtomicU64::new(1_725_000_000_000));
        let clock = Arc::clone(&now);
        let mut manager = PairingManager::with_clock_and_ttls(
            storage,
            move || clock.load(Ordering::SeqCst),
            DEFAULT_PAIRING_TOKEN_TTL_MS,
            1_000,
        );
        let registration = register_phone(&mut manager);

        now.store(1_725_000_000_000 + 1_000, Ordering::SeqCst);

        assert_eq!(
            manager.validate_session_token(&registration.session_token),
            Err(PairingError::ExpiredToken)
        );
    }

    #[test]
    fn revoked_device_rejects_existing_session() {
        let (_dir, storage) = temp_storage();
        let now = Arc::new(AtomicU64::new(1_725_000_000_000));
        let mut manager = manager_at(Arc::clone(&now), storage);
        let registration = register_phone(&mut manager);

        manager
            .revoke_device("phone-1")
            .expect("device revokes through pairing manager");

        assert_eq!(
            manager.validate_session_token(&registration.session_token),
            Err(PairingError::DeviceRevoked)
        );
    }

    #[test]
    fn unknown_device_cannot_create_session() {
        let (_dir, storage) = temp_storage();
        let now = Arc::new(AtomicU64::new(1_725_000_000_000));
        let mut manager = manager_at(Arc::clone(&now), storage);

        assert_eq!(
            manager.create_session_token("missing-device", "phone-secret"),
            Err(PairingError::DeviceNotFound)
        );
    }

    #[test]
    fn invalid_session_token_is_rejected() {
        let (_dir, storage) = temp_storage();
        let now = Arc::new(AtomicU64::new(1_725_000_000_000));
        let manager = manager_at(Arc::clone(&now), storage);

        assert_eq!(
            manager.validate_session_token("not-a-session-token"),
            Err(PairingError::InvalidToken)
        );
    }

    #[test]
    fn wrong_device_secret_cannot_create_session() {
        let (_dir, storage) = temp_storage();
        let now = Arc::new(AtomicU64::new(1_725_000_000_000));
        let mut manager = manager_at(Arc::clone(&now), storage);

        register_phone(&mut manager);

        assert_eq!(
            manager.create_session_token("phone-1", "wrong-secret"),
            Err(PairingError::InvalidToken)
        );
    }
}
