use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use uuid::Uuid;

const DEFAULT_LOCAL_ASSET_MAX_ENTRIES: usize = 256;
const DEFAULT_LOCAL_ASSET_TTL_MS: u64 = 30 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalAssetRegistryConfig {
    pub max_entries: usize,
    pub ttl_ms: u64,
}

impl Default for LocalAssetRegistryConfig {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_LOCAL_ASSET_MAX_ENTRIES,
            ttl_ms: DEFAULT_LOCAL_ASSET_TTL_MS,
        }
    }
}

pub struct LocalAssetRegistry {
    by_path: HashMap<PathBuf, String>,
    by_token: HashMap<String, LocalAssetEntry>,
    config: LocalAssetRegistryConfig,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    access_sequence: u64,
}

impl std::fmt::Debug for LocalAssetRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalAssetRegistry")
            .field("by_path_len", &self.by_path.len())
            .field("by_token_len", &self.by_token.len())
            .field("config", &self.config)
            .field("access_sequence", &self.access_sequence)
            .finish()
    }
}

impl Default for LocalAssetRegistry {
    fn default() -> Self {
        Self::with_config(LocalAssetRegistryConfig::default())
    }
}

#[derive(Debug, Clone)]
struct LocalAssetEntry {
    path: PathBuf,
    created_at: u64,
    last_accessed_at: u64,
    last_access_sequence: u64,
}

impl LocalAssetRegistry {
    pub fn with_config(config: LocalAssetRegistryConfig) -> Self {
        Self::with_clock(config, current_time_ms)
    }

    pub fn with_clock(
        config: LocalAssetRegistryConfig,
        clock: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            by_path: HashMap::new(),
            by_token: HashMap::new(),
            config,
            clock: Arc::new(clock),
            access_sequence: 0,
        }
    }

    pub fn register_image(&mut self, path: PathBuf) -> String {
        let now = self.now();
        self.prune_expired(now);

        if let Some(token) = self.by_path.get(&path).cloned() {
            if self.touch_existing(&token, now) {
                return token;
            }
            self.remove_token(&token);
        }

        let token = Uuid::new_v4().to_string();
        let access_sequence = self.next_access_sequence();
        self.by_path.insert(path.clone(), token.clone());
        self.by_token.insert(
            token.clone(),
            LocalAssetEntry {
                path,
                created_at: now,
                last_accessed_at: now,
                last_access_sequence: access_sequence,
            },
        );
        self.enforce_capacity();
        token
    }

    pub fn path_for(&mut self, token: &str) -> Option<PathBuf> {
        let now = self.now();
        self.prune_expired(now);
        if !self.touch_existing(token, now) {
            return None;
        }

        self.by_token.get(token).map(|entry| entry.path.clone())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.by_token.len()
    }

    fn touch_existing(&mut self, token: &str, now: u64) -> bool {
        let Some(entry) = self.by_token.get(token) else {
            return false;
        };
        if self.is_expired(entry, now) {
            return false;
        }

        let access_sequence = self.next_access_sequence();
        if let Some(entry) = self.by_token.get_mut(token) {
            entry.last_accessed_at = now;
            entry.last_access_sequence = access_sequence;
            true
        } else {
            false
        }
    }

    fn prune_expired(&mut self, now: u64) {
        let expired_tokens = self
            .by_token
            .iter()
            .filter(|(_token, entry)| self.is_expired(entry, now))
            .map(|(token, _entry)| token.clone())
            .collect::<Vec<_>>();
        for token in expired_tokens {
            self.remove_token(&token);
        }
    }

    fn enforce_capacity(&mut self) {
        while self.by_token.len() > self.config.max_entries {
            let Some(token) = self
                .by_token
                .iter()
                .min_by_key(|(_token, entry)| entry.last_access_sequence)
                .map(|(token, _entry)| token.clone())
            else {
                break;
            };
            self.remove_token(&token);
        }
    }

    fn remove_token(&mut self, token: &str) {
        if let Some(entry) = self.by_token.remove(token) {
            self.by_path.remove(&entry.path);
        }
    }

    fn is_expired(&self, entry: &LocalAssetEntry, now: u64) -> bool {
        now.saturating_sub(entry.created_at) > self.config.ttl_ms
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    fn next_access_sequence(&mut self) -> u64 {
        self.access_sequence = self.access_sequence.saturating_add(1);
        self.access_sequence
    }
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is after unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    #[test]
    fn registry_reuses_token_for_same_path() {
        let mut registry = LocalAssetRegistry::default();
        let path = PathBuf::from("/var/folders/codex-clipboard.png");

        let first = registry.register_image(path.clone());
        let second = registry.register_image(path.clone());

        assert_eq!(first, second);
        assert_eq!(registry.path_for(&first), Some(path));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn registry_returns_none_for_unknown_token() {
        let mut registry = LocalAssetRegistry::default();

        assert_eq!(registry.path_for("missing"), None);
    }

    #[test]
    fn registry_evicts_least_recently_used_entry_when_capacity_is_reached() {
        let mut registry = LocalAssetRegistry::with_config(LocalAssetRegistryConfig {
            max_entries: 2,
            ttl_ms: 60_000,
        });
        let first_path = PathBuf::from("/var/folders/first.png");
        let second_path = PathBuf::from("/var/folders/second.png");
        let third_path = PathBuf::from("/var/folders/third.png");

        let first = registry.register_image(first_path.clone());
        let second = registry.register_image(second_path.clone());
        assert_eq!(registry.path_for(&first), Some(first_path.clone()));
        let third = registry.register_image(third_path.clone());

        assert_eq!(registry.len(), 2);
        assert_eq!(registry.path_for(&first), Some(first_path));
        assert_eq!(registry.path_for(&second), None);
        assert_eq!(registry.path_for(&third), Some(third_path));

        let second_after_eviction = registry.register_image(second_path);
        assert_ne!(second, second_after_eviction);
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn registry_expires_entries_after_ttl_and_allows_new_token_for_same_path() {
        let now = Arc::new(AtomicU64::new(1_000));
        let clock_now = Arc::clone(&now);
        let mut registry = LocalAssetRegistry::with_clock(
            LocalAssetRegistryConfig {
                max_entries: 10,
                ttl_ms: 10,
            },
            move || clock_now.load(Ordering::SeqCst),
        );
        let path = PathBuf::from("/var/folders/expiring.png");

        let first = registry.register_image(path.clone());
        now.store(1_010, Ordering::SeqCst);
        assert_eq!(registry.path_for(&first), Some(path.clone()));

        now.store(1_011, Ordering::SeqCst);
        assert_eq!(registry.path_for(&first), None);
        assert_eq!(registry.len(), 0);

        let second = registry.register_image(path.clone());
        assert_ne!(first, second);
        assert_eq!(registry.path_for(&second), Some(path));
    }
}
