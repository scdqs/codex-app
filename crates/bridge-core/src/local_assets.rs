use std::{collections::HashMap, path::PathBuf};

use uuid::Uuid;

#[derive(Debug, Default)]
pub struct LocalAssetRegistry {
    by_path: HashMap<PathBuf, String>,
    by_token: HashMap<String, PathBuf>,
}

impl LocalAssetRegistry {
    pub fn register_image(&mut self, path: PathBuf) -> String {
        if let Some(token) = self.by_path.get(&path) {
            return token.clone();
        }

        let token = Uuid::new_v4().to_string();
        self.by_path.insert(path.clone(), token.clone());
        self.by_token.insert(token.clone(), path);
        token
    }

    pub fn path_for(&self, token: &str) -> Option<PathBuf> {
        self.by_token.get(token).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn registry_reuses_token_for_same_path() {
        let mut registry = LocalAssetRegistry::default();
        let path = PathBuf::from("/var/folders/codex-clipboard.png");

        let first = registry.register_image(path.clone());
        let second = registry.register_image(path.clone());

        assert_eq!(first, second);
        assert_eq!(registry.path_for(&first), Some(path));
    }

    #[test]
    fn registry_returns_none_for_unknown_token() {
        let registry = LocalAssetRegistry::default();

        assert_eq!(registry.path_for("missing"), None);
    }
}
