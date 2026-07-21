use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum SecretFileError {
    #[error("temporary secret file I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

pub struct TemporarySecretFile {
    path: PathBuf,
    removed: bool,
}

impl TemporarySecretFile {
    pub fn create(
        runtime_dir: &Path,
        filename_prefix: &str,
        secret: &[u8],
    ) -> Result<Self, SecretFileError> {
        std::fs::create_dir_all(runtime_dir)?;
        let path = runtime_dir.join(format!("{filename_prefix}-{}", Uuid::new_v4()));
        Self::create_at_path(path, secret)
    }

    pub(crate) fn create_at_path(path: PathBuf, secret: &[u8]) -> Result<Self, SecretFileError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        let write_result = (|| -> Result<(), std::io::Error> {
            file.write_all(secret)?;
            file.sync_all()
        })();
        if let Err(error) = write_result {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error.into());
        }
        Ok(Self {
            path,
            removed: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn remove(mut self) -> Result<(), SecretFileError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {
                self.removed = true;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.removed = true;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for TemporarySecretFile {
    fn drop(&mut self) {
        if !self.removed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn secret_file_is_mode_0600_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir creates");
        let path;
        {
            let file = TemporarySecretFile::create(dir.path(), "test-secret", b"secret-value")
                .expect("secret file creates");
            path = file.path().to_path_buf();
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("metadata reads")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(!path.exists());
    }

    #[test]
    fn explicit_remove_is_idempotent_with_drop() {
        let dir = tempfile::tempdir().expect("tempdir creates");
        let file = TemporarySecretFile::create(dir.path(), "test-secret", b"secret-value")
            .expect("secret file creates");
        let path = file.path().to_path_buf();

        file.remove().expect("secret file removes");

        assert!(!path.exists());
    }

    #[test]
    fn collision_never_removes_an_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir creates");
        let path = dir.path().join("existing-secret-file");
        std::fs::write(&path, "existing-secret").expect("fixture writes");

        assert!(TemporarySecretFile::create_at_path(path.clone(), b"new-secret").is_err());
        assert_eq!(
            std::fs::read_to_string(path).expect("fixture remains"),
            "existing-secret"
        );
    }
}
