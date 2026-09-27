//! Controller-owned mounted disks. One live journal per conversation directory.
use super::{Disk, LazyDisk, policy::Policy, remote::RemoteSource};
use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio_util::sync::CancellationToken;
type Registry = Mutex<HashMap<PathBuf, Weak<Volume>>>;
fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
pub struct Volume {
    pub disk: Arc<LazyDisk>,
    pub source: Arc<RemoteSource>,
    pub stop: CancellationToken,
    directory: PathBuf,
    policy: Policy,
    pressure: AtomicBool,
    fault: AtomicBool,
    paused: AtomicBool,
}
impl Drop for Volume {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
pub fn exists(directory: &Path) -> bool {
    directory.join("lazy/journal.sqlite").exists()
}
pub fn live(directory: &Path) -> Option<Arc<Volume>> {
    registry().try_lock().ok()?.get(directory)?.upgrade()
}
pub fn open(directory: &Path) -> Result<Arc<Volume>> {
    let mut registry = registry().lock().map_err(Error::internal)?;
    if let Some(volume) = registry.get(directory).and_then(Weak::upgrade) {
        return Ok(volume);
    }
    let root = directory.join("lazy");
    let context = LazyDisk::context(&root)?;
    let stop = CancellationToken::new();
    let source = Arc::new(RemoteSource::new(
        &context,
        tokio::runtime::Handle::current(),
        stop.clone(),
    )?);
    let disk = Arc::new(LazyDisk::open(&root, source.clone())?);
    let policy: Policy = serde_json::from_value(context["policy"].clone()).unwrap_or_default();
    policy.validate()?;
    let volume = Arc::new(Volume {
        disk,
        source,
        stop,
        directory: directory.to_owned(),
        policy,
        pressure: AtomicBool::new(false),
        fault: AtomicBool::new(false),
        paused: AtomicBool::new(false),
    });
    registry.insert(directory.to_owned(), Arc::downgrade(&volume));
    Ok(volume)
}
async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    static IO: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);
    let permit = IO.acquire().await.map_err(Error::internal)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(Error::internal)?
}
pub async fn load(directory: &Path) -> Result<Arc<Volume>> {
    let directory = directory.to_owned();
    blocking(move || open(&directory)).await
}
impl Volume {
    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
    pub fn set_paused(&self, value: bool) {
        self.paused.store(value, Ordering::SeqCst);
    }
    pub async fn inspect(self: &Arc<Self>) -> Result<Value> {
        let volume = self.clone();
        blocking(move || volume.status()).await
    }
    pub async fn needs_pause(self: &Arc<Self>) -> Result<bool> {
        let volume = self.clone();
        blocking(move || Ok(!volume.health()?["waitingFor"].is_null())).await
    }
    fn policy(&self) -> io::Result<Policy> {
        let state = self
            .directory
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| io::Error::other("Invalid disk path"))?;
        match std::fs::read(state.join("storage-policy.json")) {
            Ok(bytes) => {
                let policy: Policy = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                policy.validate().map_err(|e| io::Error::other(e.message))?;
                Ok(policy)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(self.policy.clone()),
            Err(e) => Err(e),
        }
    }
    pub fn status(&self) -> Result<Value> {
        let mut status = self.health()?;
        status["localBytes"] = allocated(&self.directory)?.into();
        status["activeLocalBytes"] = allocated(&self.directory.join("lazy"))?.into();
        Ok(status)
    }
    fn health(&self) -> Result<Value> {
        let mut status = self.disk.accounting()?;
        let state = self
            .directory
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| Error::bad("Invalid disk directory."))?;
        let policy = self.policy()?;
        super::cache::maintain(state, &policy)?;
        let (total, free) = super::policy::space(&self.directory)?;
        let reason = if self.fault.load(Ordering::SeqCst) {
            Some("integrity")
        } else if self.pressure.load(Ordering::SeqCst) {
            Some("disk-space")
        } else if self.source.waiting() {
            Some("storage-unavailable")
        } else {
            policy.pause_reason(
                total,
                free,
                status["dirtySince"].as_i64(),
                crate::config::now(),
            )
        };
        status["mode"] = "on-demand".into();
        status["migrated"] = true.into();
        status["grantId"] = self.source.grant_id().into();
        status["waitingFor"] = json!(reason);
        status["freeBytes"] = free.into();
        status["reserveBytes"] = policy.reserve(total).into();
        status["backupSeconds"] = policy.backup_seconds.into();
        Ok(status)
    }
}
impl Disk for Volume {
    fn size(&self) -> u64 {
        self.disk.size()
    }
    fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        let result = self.disk.read_at(offset, bytes);
        if result.as_ref().is_err_and(|e| {
            !matches!(
                e.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            )
        }) {
            self.fault.store(true, Ordering::SeqCst);
        }
        result
    }
    fn write_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        // Serialized admission across the node closes the free-space race between
        // concurrent writers. Space for SQLite WAL/pages is reserved conservatively.
        loop {
            if self.stop.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "Disk stopped"));
            }
            let guard = super::cache::admission()?;
            let (total, free) = super::policy::space(&self.directory)?;
            if free
                > self
                    .policy()?
                    .reserve(total)
                    .saturating_add(bytes.len() as u64 * 4 + 1024 * 1024)
            {
                self.pressure.store(false, Ordering::SeqCst);
                return self.disk.write_at(offset, bytes);
            }
            self.pressure.store(true, Ordering::SeqCst);
            drop(guard);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    fn sync(&self) -> io::Result<()> {
        self.disk.sync()
    }
}

fn allocated(directory: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0u64;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if entry.file_type()?.is_dir() {
            total = total.saturating_add(allocated(&entry.path())?);
        } else {
            total = total.saturating_add(metadata.blocks().saturating_mul(512));
        }
    }
    Ok(total)
}

/// Exceptional local materialization for existing ext4 resizing tools. The caller
/// holds the conversation disk lock. Failure leaves the journal authoritative.
pub async fn materialize(directory: &Path, stop: &CancellationToken) -> Result<()> {
    let volume = load(directory).await?;
    let cancel_reads = volume.stop.clone().drop_guard();
    let (total, free) = super::policy::space(directory)?;
    let required = volume
        .disk
        .size()
        .saturating_add(volume.policy()?.reserve(total));
    if free < required {
        return Err(Error::new(
            507,
            format!(
                "Disk resize needs {} additional local bytes for materialization.",
                required - free
            ),
        ));
    }
    let staging = tempfile::Builder::new()
        .prefix("materialize-")
        .tempdir_in(directory)?;
    let target = staging.path().join("data.ext4");
    let copy = target.clone();
    let disk = volume.disk.clone();
    let writer = tokio::task::spawn_blocking(move || super::export(disk.as_ref(), &copy));
    tokio::select! {
        _ = stop.cancelled() => { return Err(Error::new(409, "Disk materialization cancelled.")); },
        result = writer => result.map_err(Error::internal)??,
    }
    tokio::fs::rename(target, directory.join("data.ext4")).await?;
    std::fs::File::open(directory)?.sync_all()?;
    drop(cancel_reads);
    drop(volume);
    tokio::fs::rename(
        directory.join("lazy"),
        directory.join(format!("stale-lazy-{}", crate::config::id())),
    )
    .await?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

/// A cold archive streams the mounted view without another full raw disk copy.
pub struct ExportMount {
    pub path: PathBuf,
    volume: Arc<Volume>,
    mounted: Option<super::fuse::MountedDisk>,
}
impl ExportMount {
    pub async fn close(mut self) -> Result<()> {
        self.volume.stop.cancel();
        if let Some(mounted) = self.mounted.take() {
            tokio::task::spawn_blocking(move || mounted.close())
                .await
                .map_err(Error::internal)??;
        }
        std::fs::remove_dir(&self.path)?;
        Ok(())
    }
}
impl Drop for ExportMount {
    fn drop(&mut self) {
        self.volume.stop.cancel();
        if let Some(mounted) = self.mounted.take() {
            let path = self.path.clone();
            // Cancellation of an async archive must not join a FUSE thread on
            // the same Tokio worker its cancelled remote transfer needs.
            std::thread::spawn(move || {
                let _ = mounted.close();
                let _ = std::fs::remove_dir(path);
            });
        }
    }
}
pub async fn mount_export(directory: &Path) -> Result<ExportMount> {
    let volume = load(directory).await?;
    let path = directory.join(format!("export-{}", crate::config::id()));
    std::fs::create_dir(&path)?;
    let mounted = super::fuse::mount_disk(volume.disk.clone(), &path, unsafe { libc::geteuid() })?;
    Ok(ExportMount {
        path,
        volume,
        mounted: Some(mounted),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn disk_pressure_waits_without_acknowledging_or_losing_writes_and_resumes() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        let policy = Policy {
            reserve_mi_b: 16 * 1024 * 1024,
            ..Default::default()
        };
        let context = json!({"master":"http://127.0.0.1:1/","grant":"test","policy":policy});
        let source = Arc::new(
            RemoteSource::new(
                &context,
                tokio::runtime::Handle::current(),
                CancellationToken::new(),
            )
            .unwrap(),
        );
        let disk=LazyDisk::create(&directory.join("lazy"),&json!({"version":1,"size":4096,"blockSize":4194304,"blocks":[{"offset":0,"size":4096,"hash":null}]}),source).unwrap();
        disk.set_context(&context).unwrap();
        drop(disk);
        let volume = open(&directory).unwrap();
        let writer = {
            let volume = volume.clone();
            tokio::task::spawn_blocking(move || volume.write_at(3, b"retained"))
        };
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(!writer.is_finished());
        assert_eq!(volume.status().unwrap()["waitingFor"], "disk-space");
        assert_eq!(volume.disk.accounting().unwrap()["dirtyBytes"], 0);
        let policy = Policy {
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Default::default()
        };
        std::fs::write(
            root.path().join("storage-policy.json"),
            serde_json::to_vec(&policy).unwrap(),
        )
        .unwrap();
        // The controller applies changed limits to an already waiting write.
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), writer).await;
        volume.stop.cancel();
        result.unwrap().unwrap().unwrap();
        let mut data = [0; 8];
        volume.read_at(3, &mut data).unwrap();
        assert_eq!(&data, b"retained");
    }
}
