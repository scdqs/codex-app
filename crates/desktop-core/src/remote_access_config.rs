use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum RemoteAccessConfigError {
    #[error("local port must be between 1 and 65535")]
    InvalidPort,
    #[error("public hostname must be a hostname without scheme, port, path, query, or fragment")]
    InvalidHostname,
    #[error("remote access configuration I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("remote access configuration JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAccessPreferences {
    pub named_tunnel: Option<NamedTunnelProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedTunnelProfile {
    pub hostname: String,
    pub local_port: u16,
}

impl NamedTunnelProfile {
    pub fn new(hostname: &str, local_port: u16) -> Result<Self, RemoteAccessConfigError> {
        if local_port == 0 {
            return Err(RemoteAccessConfigError::InvalidPort);
        }

        let trimmed = hostname.trim().to_ascii_lowercase();
        if trimmed.contains("://") || trimmed.contains('/') {
            return Err(RemoteAccessConfigError::InvalidHostname);
        }

        let parsed = url::Url::parse(&format!("https://{trimmed}"))
            .map_err(|_| RemoteAccessConfigError::InvalidHostname)?;
        if parsed.host_str() != Some(trimmed.as_str()) || parsed.port().is_some() {
            return Err(RemoteAccessConfigError::InvalidHostname);
        }

        Ok(Self {
            hostname: trimmed,
            local_port,
        })
    }

    pub fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.local_port)
    }

    pub fn public_url(&self) -> String {
        format!("https://{}", self.hostname)
    }
}

pub struct RemoteAccessConfigStore {
    path: PathBuf,
}

impl RemoteAccessConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<RemoteAccessPreferences, RemoteAccessConfigError> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => Ok(serde_json::from_str(&contents)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(RemoteAccessPreferences::default())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(
        &self,
        preferences: &RemoteAccessPreferences,
    ) -> Result<(), RemoteAccessConfigError> {
        let contents = serde_json::to_vec_pretty(preferences)?;
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let file_name = self.path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "remote access configuration path has no file name",
            )
        })?;
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(
            ".{}.tmp-{}-{counter}",
            file_name.to_string_lossy(),
            std::process::id()
        ));

        let write_result = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)?;
            file.write_all(&contents)?;
            file.flush()?;
            file.sync_all()?;
            Ok(())
        })();

        if let Err(error) = write_result {
            let _ = fs::remove_file(&temp_path);
            return Err(error.into());
        }

        if let Err(error) = fs::rename(&temp_path, &self.path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error.into());
        }

        Ok(())
    }

    pub fn delete(&self) -> Result<(), RemoteAccessConfigError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NamedTunnelProfile, RemoteAccessConfigStore, RemoteAccessPreferences};

    #[test]
    fn named_profile_normalizes_hostname_and_round_trips_without_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = RemoteAccessConfigStore::new(dir.path().join("remote-access.json"));
        let profile = NamedTunnelProfile::new(" Codex.Example.COM ", 57324).unwrap();

        store
            .save(&RemoteAccessPreferences {
                named_tunnel: Some(profile.clone()),
            })
            .unwrap();

        assert_eq!(profile.hostname, "codex.example.com");
        assert_eq!(store.load().unwrap().named_tunnel, Some(profile));
        let raw = std::fs::read_to_string(dir.path().join("remote-access.json")).unwrap();
        assert!(!raw.to_ascii_lowercase().contains("token"));
    }

    #[test]
    fn named_profile_rejects_url_paths_and_zero_port() {
        assert!(NamedTunnelProfile::new("https://codex.example.com/path", 57324).is_err());
        assert!(NamedTunnelProfile::new("codex.example.com", 0).is_err());
    }

    #[test]
    fn missing_config_loads_defaults_and_delete_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = RemoteAccessConfigStore::new(dir.path().join("remote-access.json"));

        assert_eq!(store.load().unwrap(), RemoteAccessPreferences::default());
        store.delete().unwrap();
    }
}
