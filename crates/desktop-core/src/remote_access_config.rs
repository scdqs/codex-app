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
            Ok(contents) => {
                let preferences: RemoteAccessPreferences = serde_json::from_str(&contents)?;
                let named_tunnel = preferences
                    .named_tunnel
                    .map(|profile| NamedTunnelProfile::new(&profile.hostname, profile.local_port))
                    .transpose()?;
                Ok(RemoteAccessPreferences { named_tunnel })
            }
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

        let write_result = write_temp_file(&temp_path, &contents);

        if let Err(error) = write_result {
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

fn write_temp_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let write_result = (|| {
        file.write_all(contents)?;
        file.flush()?;
        file.sync_all()
    })();
    drop(file);

    if let Err(error) = write_result {
        let _ = fs::remove_file(path);
        return Err(error);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        NamedTunnelProfile, RemoteAccessConfigError, RemoteAccessConfigStore,
        RemoteAccessPreferences, write_temp_file,
    };

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
    fn load_rejects_persisted_invalid_hostname() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote-access.json");
        std::fs::write(
            &path,
            r#"{"namedTunnel":{"hostname":"https://codex.example.com/path","localPort":57324}}"#,
        )
        .unwrap();
        let store = RemoteAccessConfigStore::new(path);

        assert!(matches!(
            store.load(),
            Err(RemoteAccessConfigError::InvalidHostname)
        ));
    }

    #[test]
    fn load_rejects_persisted_zero_port() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote-access.json");
        std::fs::write(
            &path,
            r#"{"namedTunnel":{"hostname":"codex.example.com","localPort":0}}"#,
        )
        .unwrap();
        let store = RemoteAccessConfigStore::new(path);

        assert!(matches!(
            store.load(),
            Err(RemoteAccessConfigError::InvalidPort)
        ));
    }

    #[test]
    fn load_normalizes_persisted_hostname() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote-access.json");
        std::fs::write(
            &path,
            r#"{"namedTunnel":{"hostname":"  CoDeX.Example.COM  ","localPort":57324}}"#,
        )
        .unwrap();
        let store = RemoteAccessConfigStore::new(path);

        assert_eq!(
            store.load().unwrap().named_tunnel.unwrap().hostname,
            "codex.example.com"
        );
    }

    #[test]
    fn save_overwrites_existing_config_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = RemoteAccessConfigStore::new(dir.path().join("remote-access.json"));
        let first = NamedTunnelProfile::new("first.example.com", 57324).unwrap();
        let second = NamedTunnelProfile::new("second.example.com", 57325).unwrap();

        store
            .save(&RemoteAccessPreferences {
                named_tunnel: Some(first),
            })
            .unwrap();
        store
            .save(&RemoteAccessPreferences {
                named_tunnel: Some(second.clone()),
            })
            .unwrap();

        assert_eq!(store.load().unwrap().named_tunnel, Some(second));
    }

    #[test]
    fn temp_file_collision_preserves_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let temp_path = dir.path().join(".remote-access.json.tmp-collision");
        let original = b"pre-existing temporary file";
        std::fs::write(&temp_path, original).unwrap();

        let error = write_temp_file(&temp_path, b"replacement").unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&temp_path).unwrap(), original);
    }

    #[test]
    fn missing_config_loads_defaults_and_delete_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = RemoteAccessConfigStore::new(dir.path().join("remote-access.json"));

        assert_eq!(store.load().unwrap(), RemoteAccessPreferences::default());
        store.delete().unwrap();
    }
}
