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
enum Entry {
    Open(Weak<Volume>),
    Replacing,
}
type Registry = Mutex<HashMap<PathBuf, Entry>>;
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
    let registry = registry().try_lock().ok()?;
    match registry.get(directory)? {
        Entry::Open(volume) => volume.upgrade(),
        Entry::Replacing => None,
    }
}
pub fn open(directory: &Path) -> Result<Arc<Volume>> {
    let mut registry = registry().lock().map_err(Error::internal)?;
    match registry.get(directory) {
        Some(Entry::Open(volume)) => {
            if let Some(volume) = volume.upgrade() {
                return Ok(volume);
            }
        }
        Some(Entry::Replacing) => return Err(Error::new(409, "Disk replacement is in progress.")),
        None => {}
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
    registry.insert(directory.to_owned(), Entry::Open(Arc::downgrade(&volume)));
    Ok(volume)
}

/// The caller also holds the conversation disk lock. Existing inspections must
/// finish before replacement; subsequent opens are rejected until the durable switch.
pub struct Replacement {
    directory: PathBuf,
}
impl Drop for Replacement {
    fn drop(&mut self) {
        if let Ok(mut registry) = registry().lock() {
            registry.remove(&self.directory);
        }
    }
}
pub async fn replacement(directory: &Path) -> Result<Replacement> {
    let directory = directory.to_owned();
    blocking(move || {
        let mut registry = registry().lock().map_err(Error::internal)?;
        match registry.get(&directory) {
            Some(Entry::Replacing) => {
                return Err(Error::new(409, "Disk replacement is in progress."));
            }
            Some(Entry::Open(volume)) if volume.strong_count() > 0 => {
                return Err(Error::new(409, "Disk is still in use; retry restoration."));
            }
            _ => {}
        }
        registry.insert(directory.clone(), Entry::Replacing);
        Ok(Replacement { directory })
    })
    .await
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
/// Initialize and synchronize a journal on the same bounded pool as journal opens.
pub(crate) async fn create(
    directory: &Path,
    manifest: &Value,
    context: &Value,
) -> Result<Arc<LazyDisk>> {
    create_at_generation(directory, manifest, context, 1).await
}
pub(crate) async fn create_at_generation(
    directory: &Path,
    manifest: &Value,
    context: &Value,
    generation: i64,
) -> Result<Arc<LazyDisk>> {
    let (directory, manifest, context) = (directory.to_owned(), manifest.clone(), context.clone());
    blocking(move || {
        let source = Arc::new(RemoteSource::new(
            &context,
            tokio::runtime::Handle::current(),
            CancellationToken::new(),
        )?);
        let disk = Arc::new(LazyDisk::create_at_generation(
            &directory, &manifest, source, generation,
        )?);
        disk.set_context(&context)?;
        disk.sync()?;
        Ok(disk)
    })
    .await
}
/// Retain the mounted disk's authorization and monotone publication sequence
/// through resize; the next acknowledged publication can then retire its old base.
pub(crate) async fn rebuild_identity(directory: &Path) -> Result<(Value, i64)> {
    let volume = load(directory).await?;
    blocking(move || {
        let context = LazyDisk::context(&volume.directory.join("lazy"))?;
        let generation = volume.disk.accounting()?["generation"]
            .as_i64()
            .ok_or_else(|| Error::bad("Missing journal generation."))?;
        Ok((context, generation))
    })
    .await
}
impl Volume {
    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
    pub fn set_paused(&self, value: bool) {
        self.paused.store(value, Ordering::SeqCst);
    }
    pub async fn seal(self: &Arc<Self>) -> Result<i64> {
        let disk = self.disk.clone();
        blocking(move || Ok(disk.seal()?)).await
    }
    pub async fn inspect(self: &Arc<Self>) -> Result<Value> {
        let volume = self.clone();
        blocking(move || volume.status()).await
    }
    pub async fn needs_pause(self: &Arc<Self>) -> Result<bool> {
        let volume = self.clone();
        blocking(move || Ok(!volume.health()?["waitingFor"].is_null())).await
    }
    /// Called under the attempt's control lock, shared with checkpoint capture.
    pub async fn enforce_limits(
        self: &Arc<Self>,
        state: &Path,
        attempt: &str,
        stop: &CancellationToken,
    ) -> Result<()> {
        use crate::microvm::host;
        use std::time::Duration;
        // A journal can be open while boot is still preparing the VM. Its
        // execution identity is installed only after activation.
        if matches!(
            tokio::fs::try_exists(state.join(format!("{attempt}.vm.json"))).await,
            Ok(false)
        ) {
            return Ok(());
        }
        let blocked = tokio::select! {
            _ = stop.cancelled() => return Err(Error::new(409, "Execution stopped.")),
            value = tokio::time::timeout(Duration::from_secs(1), self.needs_pause()) => value.ok().and_then(std::result::Result::ok).unwrap_or(true),
        };
        if blocked != self.paused() {
            tracing::info!(target: "leo_performance", operation = "storage_backpressure", id = attempt, paused = blocked);
            let result = if blocked {
                host::pause_attempt(state, attempt).await
            } else {
                host::resume_attempt(state, attempt).await
            };
            if let Err(error) = result {
                // Either command may have taken effect despite a lost response.
                // Stop the attempt and release blocked reads for its shutdown.
                stop.cancel();
                self.stop.cancel();
                return Err(error);
            }
            self.set_paused(blocked);
        }
        Ok(())
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
        status["performance"] = self.disk.performance();
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
    drop(cancel_reads);
    drop(volume);
    let _replacement = replacement(directory).await?;
    tokio::fs::rename(target, directory.join("data.ext4")).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    tokio::fs::rename(directory.join("lazy"), directory.join("resize-source")).await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn missing_remote_block_requires_resolution_and_preserves_local_work() {
        permanent_remote_failure_preserves_local_work(axum::http::StatusCode::CONFLICT).await;
    }

    #[tokio::test]
    async fn refused_remote_read_requires_resolution_and_preserves_local_work() {
        permanent_remote_failure_preserves_local_work(axum::http::StatusCode::FAILED_DEPENDENCY)
            .await;
    }

    async fn permanent_remote_failure_preserves_local_work(status: axum::http::StatusCode) {
        use axum::{Router, routing::get};
        use std::{sync::atomic::AtomicUsize, time::Duration};
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let server = Router::new().route(
            "/internal/node-restore/{hash}",
            get(move || {
                count.fetch_add(1, Ordering::SeqCst);
                async move { status }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let serving = tokio::spawn(async { axum::serve(listener, server).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        let policy = Policy {
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Default::default()
        };
        let context = json!({"master":origin,"grant":"fixture","policy":policy});
        let source = Arc::new(
            RemoteSource::new(
                &context,
                tokio::runtime::Handle::current(),
                CancellationToken::new(),
            )
            .unwrap(),
        );
        let disk = LazyDisk::create(
            &directory.join("lazy"),
            &json!({"version":1,"size":4096,"blockSize":4194304,
                "blocks":[{"offset":0,"size":4096,"hash":"a".repeat(64)}]}),
            source,
        )
        .unwrap();
        disk.set_context(&context).unwrap();
        disk.write_at(0, b"unsaved work").unwrap();
        drop(disk);
        let volume = open(&directory).unwrap();
        let reading = volume.clone();
        let mut read = tokio::task::spawn_blocking(move || reading.read_at(1024, &mut [0; 8]));
        let result = tokio::time::timeout(Duration::from_secs(2), &mut read).await;
        // A regression must fail promptly instead of retaining a blocked test thread.
        if result.is_err() {
            volume.stop.cancel();
            let _ = read.await;
        }
        assert_eq!(
            result.unwrap().unwrap().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(!volume.source.waiting());
        assert_eq!(volume.status().unwrap()["waitingFor"], "integrity");
        assert!(volume.needs_pause().await.unwrap());
        let mut saved = [0; 12];
        volume.read_at(0, &mut saved).unwrap();
        assert_eq!(&saved, b"unsaved work");
        // A subsequent successful local read must not silently clear the fault.
        assert_eq!(volume.status().unwrap()["waitingFor"], "integrity");
        drop(volume);
        let reopened = open(&directory).unwrap();
        reopened.read_at(0, &mut saved).unwrap();
        assert_eq!(&saved, b"unsaved work");
        serving.abort();
    }

    #[tokio::test]
    async fn replacement_excludes_openers_and_releases_the_disk_after_failure() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        let manifest = json!({"version":1,"size":4096,"blockSize":4194304,
            "blocks":[{"offset":0,"size":4096,"hash":null}]});
        let context = json!({"master":"http://127.0.0.1:1/","grant":"fixture",
            "policy":Policy::default()});
        drop(
            create(&directory.join("lazy"), &manifest, &context)
                .await
                .unwrap(),
        );
        let volume = load(&directory).await.unwrap();
        assert_eq!(replacement(&directory).await.err().unwrap().status, 409);
        drop(volume);
        let guard = replacement(&directory).await.unwrap();
        assert_eq!(load(&directory).await.err().unwrap().status, 409);
        assert_eq!(replacement(&directory).await.err().unwrap().status, 409);
        // Returning an error during restoration drops the same guard.
        drop(guard);
        let reopened = load(&directory).await.unwrap();
        let mut bytes = [1; 3];
        reopened.read_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [0; 3]);
    }

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
