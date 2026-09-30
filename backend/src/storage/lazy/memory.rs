//! Verified immutable bytes, bounded by retained payload size and entry count.
//! A node shares base blocks; each disk owns its journal cache. Snapshot scans
//! may reuse hot bytes but neither admit cold blocks nor promote scan hits.
use super::failure;
use lru::LruCache;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

pub(super) struct BytesCache {
    entries: LruCache<String, Arc<Vec<u8>>>,
    used: usize,
    budget: usize,
    evictions: u64,
}

impl BytesCache {
    pub fn new(budget: usize) -> Self {
        Self {
            entries: LruCache::unbounded(),
            used: 0,
            budget,
            evictions: 0,
        }
    }

    pub fn get(&mut self, key: &str, foreground: bool) -> Option<Arc<Vec<u8>>> {
        if foreground {
            self.entries.get(key)
        } else {
            self.entries.peek(key)
        }
        .cloned()
    }

    pub fn insert(&mut self, key: &str, bytes: Vec<u8>, foreground: bool) -> Arc<Vec<u8>> {
        let bytes = Arc::new(bytes);
        if !foreground || bytes.len() > self.budget {
            return bytes;
        }
        if let Some(previous) = self.entries.pop(key) {
            self.used -= previous.len();
        }
        while self.used + bytes.len() > self.budget || self.entries.len() >= 8192 {
            self.evict();
        }
        self.used += bytes.len();
        self.entries.put(key.to_owned(), bytes.clone());
        bytes
    }

    fn evict(&mut self) {
        if let Some((_, bytes)) = self.entries.pop_lru() {
            self.used -= bytes.len();
            self.evictions += 1;
        }
    }

    pub fn resize(&mut self, budget: usize) {
        self.budget = budget;
        while self.used > self.budget {
            self.evict();
        }
    }

    pub fn status(&self) -> Value {
        json!({
            "bytes": self.used,
            "budgetBytes": self.budget,
            "entries": self.entries.len(),
            "evictions": self.evictions
        })
    }
}

type SourceLocks = HashMap<(usize, String), Weak<Mutex<()>>>;

pub(super) struct BlockCache {
    pub bytes: Mutex<BytesCache>,
    fetching: Mutex<SourceLocks>,
}

impl BlockCache {
    pub fn for_directory(directory: &Path) -> io::Result<Arc<Self>> {
        type Registry = HashMap<PathBuf, Weak<BlockCache>>;
        static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
        // Sharing is restricted to disks owned by this node's private state.
        // An arbitrary manifest or hash cannot access another node's cache.
        let scope = super::node_state(directory).unwrap_or(directory);
        let mut registry = REGISTRY
            .get_or_init(Mutex::default)
            .lock()
            .map_err(failure)?;
        if let Some(cache) = registry.get(scope).and_then(Weak::upgrade) {
            return Ok(cache);
        }
        registry.retain(|_, cache| cache.strong_count() > 0);
        let cache = Arc::new(Self {
            bytes: Mutex::new(BytesCache::new(256 * 1024 * 1024)),
            fetching: Mutex::default(),
        });
        registry.insert(scope.to_owned(), Arc::downgrade(&cache));
        Ok(cache)
    }

    pub fn fetching(
        &self,
        hash: &str,
        source: &Arc<dyn super::BlockSource>,
    ) -> io::Result<Arc<Mutex<()>>> {
        // A source can wait until its own conversation is cancelled. Only
        // readers using that same source may inherit its in-flight transfer.
        // The source stays alive through the read, so its address cannot be
        // recycled while this lock is held. Verified bytes remain node-wide.
        let key = (Arc::as_ptr(source).cast::<()>() as usize, hash.to_owned());
        let mut fetching = self.fetching.lock().map_err(failure)?;
        if let Some(lock) = fetching.get(&key).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        fetching.retain(|_, lock| lock.strong_count() > 0);
        let lock = Arc::new(Mutex::new(()));
        fetching.insert(key, Arc::downgrade(&lock));
        Ok(lock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_do_not_evict_or_promote_hot_data_and_oversized_values_are_not_retained() {
        let mut cache = BytesCache::new(8);
        cache.insert("first", vec![1; 4], true);
        cache.insert("second", vec![2; 4], true);
        assert!(cache.get("first", false).is_some());
        cache.insert("scan", vec![3; 4], false);
        cache.insert("oversized", vec![4; 9], true);
        assert_eq!(cache.used, 8);
        assert!(cache.get("scan", true).is_none());
        cache.insert("third", vec![5; 4], true);
        assert!(cache.get("first", true).is_none());
        assert!(cache.get("second", true).is_some());
        cache.resize(4);
        assert_eq!(cache.used, 4);
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn node_policy_resizes_the_verified_cache_even_without_a_mounted_disk() {
        let root = tempfile::tempdir().unwrap();
        let owner = crate::storage::NodeBlockCache::new(root.path()).unwrap();
        {
            let mut cache = owner.blocks.bytes.lock().unwrap();
            cache.insert("first", vec![1; 4 * 1024 * 1024], true);
            cache.insert("second", vec![2; 4 * 1024 * 1024], true);
        }
        let policy = crate::storage::policy::Policy {
            memory_cache_mi_b: 4,
            ..Default::default()
        };
        crate::storage::runtime::configure(root.path(), &policy).unwrap();
        let mut cache = owner.blocks.bytes.lock().unwrap();
        assert_eq!(cache.status()["budgetBytes"], 4 * 1024 * 1024);
        assert_eq!(cache.status()["bytes"], 4 * 1024 * 1024);
        assert!(cache.get("first", true).is_none());
        assert!(cache.get("second", true).is_some());
    }
}
