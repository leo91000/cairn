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
            disks: Default::default(),
            cache: Default::default(),
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
}
