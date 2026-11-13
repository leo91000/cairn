//! Serialize node preparation and local usage observations per conversation.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

#[derive(Default)]
struct Disk {
    operation: Arc<AsyncMutex<()>>,
    storage_refresh: Arc<AsyncMutex<()>>,
}

#[derive(Default)]
pub struct Coordination {
    disks: Mutex<HashMap<String, Weak<Disk>>>,
}

pub struct Operation {
    _guard: OwnedMutexGuard<()>,
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
        let guard = disk.operation.clone().lock_owned().await;
        Operation {
            _guard: guard,
            _disk: disk,
        }
    }

    pub async fn storage_refresh(&self, run: &str) -> Operation {
        let disk = self.disk(run);
        let guard = disk.storage_refresh.clone().lock_owned().await;
        Operation {
            _guard: guard,
            _disk: disk,
        }
    }
}
