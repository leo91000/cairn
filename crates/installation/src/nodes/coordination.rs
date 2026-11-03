//! Serialize each disk independently. Cache eviction still excludes all mutations.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{
    Mutex as AsyncMutex, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard,
    OwnedSemaphorePermit, RwLock, Semaphore,
};

pub const SYNC_CONCURRENCY: usize = 2;

#[derive(Default)]
struct Disk {
    operation: Arc<AsyncMutex<()>>,
    storage_refresh: Arc<AsyncMutex<()>>,
    readers: Arc<RwLock<()>>,
}

pub struct Coordination {
    disks: Mutex<HashMap<String, Weak<Disk>>>,
    cache: Arc<RwLock<()>>,
    transfers: Arc<Semaphore>,
}

impl Default for Coordination {
    fn default() -> Self {
        Self {
            disks: Mutex::default(),
            cache: Arc::default(),
            transfers: Arc::new(Semaphore::new(SYNC_CONCURRENCY)),
        }
    }
}

pub struct Operation {
    _operation: OwnedMutexGuard<()>,
    _cache: OwnedRwLockReadGuard<()>,
    _disk: Arc<Disk>,
}

pub struct Reader {
    _read: OwnedRwLockReadGuard<()>,
    _disk: Arc<Disk>,
}

pub struct StorageRefresh {
    _refresh: OwnedMutexGuard<()>,
    _disk: Arc<Disk>,
}

pub struct ReadersDrained {
    _write: OwnedRwLockWriteGuard<()>,
    _disk: Arc<Disk>,
}

impl Coordination {
    fn disk(&self, run: &str) -> Arc<Disk> {
        let mut disks = self.disks.lock().unwrap();
        if let Some(disk) = disks.get(run).and_then(Weak::upgrade) {
            return disk;
        }
        disks.retain(|_, disk| disk.strong_count() > 0);
        let disk = Arc::new(Disk::default());
        disks.insert(run.into(), Arc::downgrade(&disk));
        disk
    }

    pub async fn lock(&self, run: &str) -> Operation {
        let disk = self.disk(run);
        let operation = disk.operation.clone().lock_owned().await;
        let cache = self.cache.clone().read_owned().await;
        Operation {
            _operation: operation,
            _cache: cache,
            _disk: disk,
        }
    }

    pub fn try_cache_eviction(&self) -> Option<OwnedRwLockWriteGuard<()>> {
        self.cache.clone().try_write_owned().ok()
    }

    pub async fn transfer(&self) -> OwnedSemaphorePermit {
        self.transfers.clone().acquire_owned().await.unwrap()
    }

    pub async fn read(&self, run: &str) -> Reader {
        let disk = self.disk(run);
        let read = disk.readers.clone().read_owned().await;
        Reader {
            _read: read,
            _disk: disk,
        }
    }

    pub async fn storage_refresh(&self, run: &str) -> StorageRefresh {
        let disk = self.disk(run);
        let refresh = disk.storage_refresh.clone().lock_owned().await;
        StorageRefresh {
            _refresh: refresh,
            _disk: disk,
        }
    }

    pub async fn drain(&self, run: &str) -> ReadersDrained {
        let disk = self.disk(run);
        let write = disk.readers.clone().write_owned().await;
        ReadersDrained {
            _write: write,
            _disk: disk,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn scopes_survive_waiters_and_eviction_excludes_mutations() {
        let locks = Coordination::default();
        let a = locks.lock("a").await;
        assert!(locks.try_cache_eviction().is_none());
        let mut same = Box::pin(locks.lock("a"));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut same)
                .await
                .is_err()
        );
        let b = tokio::time::timeout(Duration::from_millis(100), locks.lock("b"))
            .await
            .unwrap();
        drop(a);
        let same = same.await;
        assert!(locks.try_cache_eviction().is_none());
        drop((same, b));
        let cache = locks.try_cache_eviction().unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), locks.lock("a"))
                .await
                .is_err()
        );
        drop(cache);
        let read = locks.read("a").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), locks.drain("a"))
                .await
                .is_err()
        );
        let other = tokio::time::timeout(Duration::from_millis(100), locks.drain("b"))
            .await
            .unwrap();
        drop((read, other));
        let _drained = locks.drain("a").await;
        let (one, two) = (locks.transfer().await, locks.transfer().await);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), locks.transfer())
                .await
                .is_err()
        );
        drop((one, two));
        let _next = locks.transfer().await;
    }

    #[tokio::test]
    async fn storage_refreshes_serialize_per_run_and_retain_weak_map_scope() {
        let locks = Coordination::default();
        let first = locks.storage_refresh("same").await;

        let mut same_run = Box::pin(locks.storage_refresh("same"));
        assert!(futures_util::poll!(same_run.as_mut()).is_pending());

        let mut different_run = Box::pin(locks.storage_refresh("different"));
        let different_run = match futures_util::poll!(different_run.as_mut()) {
            std::task::Poll::Ready(guard) => guard,
            std::task::Poll::Pending => panic!("different runs must refresh independently"),
        };
        drop(different_run);

        drop(first);
        let same_run = same_run.await;

        let mut next_same_run = Box::pin(locks.storage_refresh("same"));
        assert!(futures_util::poll!(next_same_run.as_mut()).is_pending());
        drop(same_run);
        let _next_same_run = next_same_run.await;
    }
}
