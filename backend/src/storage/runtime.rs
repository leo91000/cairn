//! Controller-owned mounted disks. One live journal per conversation directory.
use super::{Disk, DiskWrite, LazyDisk, policy::Policy, remote::RemoteSource};
use crate::error::{Error, Result};
use serde_json::Value;
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, RwLock, Weak,
        atomic::{AtomicBool, AtomicI64, Ordering},
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

/// Apply node limits without policy-file reads in the CPU safety path.
pub(crate) fn configure(state: &Path, policy: &Policy) -> Result<()> {
    super::NodeBlockCache::new(state)?.resize(policy.memory_cache_mi_b as usize * 1024 * 1024)?;
    let registry = registry().lock().map_err(Error::internal)?;
    for (directory, entry) in registry.iter() {
        if directory.parent().and_then(Path::parent) != Some(state) {
            continue;
        }
        let Entry::Open(volume) = entry else {
            continue;
        };
        let Some(volume) = volume.upgrade() else {
            continue;
        };
        *volume.control_policy.write().map_err(Error::internal)? = policy.clone();
        let (total, free) = super::policy::space(directory)?;
        volume
            .space_pressure
            .store(free <= policy.reserve(total), Ordering::Release);
        volume
            .disk
            .memory_budget(policy.memory_cache_mi_b as usize * 1024 * 1024)?;
    }
    Ok(())
}

pub struct Volume {
    pub disk: Arc<LazyDisk>,
    pub source: Arc<RemoteSource>,
    pub stop: CancellationToken,
    directory: PathBuf,
    policy: Policy,
    control_policy: RwLock<Policy>,
    active_since: AtomicI64,
    active_attempt: Mutex<String>,
    pressure: AtomicBool,
    space_pressure: AtomicBool,
    fault: AtomicBool,
    paused: AtomicBool,
    transition: Mutex<Option<std::time::Instant>>,
}

impl Drop for Volume {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub fn exists(directory: &Path) -> bool {
    directory.join("lazy/journal-v2.sqlite").exists()
        || directory.join("lazy/journal.sqlite").exists()
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
        Some(Entry::Replacing) => return Err(Error::conflict("Disk replacement is in progress.")),
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
    disk.memory_budget(policy.memory_cache_mi_b as usize * 1024 * 1024)?;
    let volume = Arc::new(Volume {
        disk,
        source,
        stop,
        directory: directory.to_owned(),
        control_policy: RwLock::new(policy.clone()),
        policy,
        active_since: AtomicI64::new(0),
        active_attempt: Mutex::new(String::new()),
        pressure: AtomicBool::new(false),
        space_pressure: AtomicBool::new(false),
        fault: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        transition: Mutex::new(None),
    });
    volume.health()?;
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
                return Err(Error::conflict("Disk replacement is in progress."));
            }
            Some(Entry::Open(volume)) if volume.strong_count() > 0 => {
                return Err(Error::conflict("Disk is still in use; retry restoration."));
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

    pub fn transition_pending(&self) -> bool {
        self.transition.lock().map_or(true, |state| state.is_some())
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
        Ok(self.control_reason(crate::config::now())?.is_some())
    }

    fn control_reason(&self, now: i64) -> Result<Option<&'static str>> {
        if self.fault.load(Ordering::SeqCst) {
            return Ok(Some("integrity"));
        }
        if self.pressure.load(Ordering::SeqCst) || self.space_pressure.load(Ordering::Acquire) {
            return Ok(Some("disk-space"));
        }
        if self.source.waiting() {
            return Ok(Some("storage-unavailable"));
        }
        let policy = self.control_policy.read().map_err(Error::internal)?;
        let active_since = self.active_since.load(Ordering::Acquire);
        Ok(self
            .disk
            .dirty_since()
            .filter(|&at| {
                now.saturating_sub(at.max(active_since)) >= (policy.max_dirty_seconds * 1000) as i64
            })
            .map(|_| "backup-lag"))
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
        if stop.is_cancelled() {
            return Err(Error::conflict("Execution stopped."));
        }
        let now = crate::config::now();
        // A resumed VM gets one bounded active synchronization window. Its
        // original dirty timestamp is retained for backup urgency and telemetry.
        {
            let mut active = self.active_attempt.lock().map_err(Error::internal)?;
            if active.as_str() != attempt {
                self.active_since.store(now, Ordering::Release);
                *active = attempt.to_owned();
            }
        }
        // No filesystem scan, policy read, journal lock, or blocking task belongs
        // in this CPU safety decision. Writers fence space admission themselves.
        let reason = match self.control_reason(now) {
            Ok(reason) => reason,
            Err(error) => {
                stop.cancel();
                self.stop.cancel();
                return Err(error);
            }
        };
        let blocked = reason.is_some();
        let pending = self.transition.lock().map_err(Error::internal)?.is_some();
        if blocked != self.paused() || pending {
            let transition_started = std::time::Instant::now();
            tracing::info!(target: "leo_performance", operation = "storage_backpressure", id = attempt, phase = "requested", paused = blocked, reason = reason.unwrap_or("ready"));
            let result = if blocked {
                host::pause_attempt(state, attempt).await
            } else {
                host::resume_attempt(state, attempt).await
            };
            if let Err(error) = result {
                if error.status == 408 {
                    // A lost response is an unknown transition, not VM death.
                    // Retry the current desired state on the next monitor tick.
                    // Keep the confirmed state unchanged until an explicit 204.
                    let mut transition = self.transition.lock().map_err(Error::internal)?;
                    let since = transition.get_or_insert_with(std::time::Instant::now);
                    if since.elapsed() < Duration::from_secs(30) {
                        return Ok(());
                    }
                }
                // Explicit refusal or a sustained inability to fence CPUs is
                // unsafe. Preserve the existing fail-closed teardown and leases.
                stop.cancel();
                self.stop.cancel();
                return Err(error);
            }
            self.set_paused(blocked);
            *self.transition.lock().map_err(Error::internal)? = None;
            tracing::info!(target: "leo_performance", operation = "storage_backpressure", id = attempt, phase = "confirmed", paused = blocked, reason = reason.unwrap_or("ready"), elapsed_ms = transition_started.elapsed().as_millis() as u64);
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
        let state = self
            .directory
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| Error::bad("Invalid disk directory."))?;
        // Cache reconciliation is maintenance, not a CPU safety probe. Writers
        // continue to enforce the disk reserve under node-wide admission.
        super::cache::maintain(state, &self.policy()?)?;
        let mut status = self.health()?;
        status["localBytes"] = allocated(&self.directory)?.into();
        status["activeLocalBytes"] = allocated(&self.directory.join("lazy"))?.into();
        status["performance"] = self.disk.performance();
        Ok(status)
    }

    fn health(&self) -> Result<Value> {
        let mut status = self.disk.accounting()?;
        let policy = self.policy()?;
        *self.control_policy.write().map_err(Error::internal)? = policy.clone();
        self.disk
            .memory_budget(policy.memory_cache_mi_b as usize * 1024 * 1024)?;
        let (total, free) = super::policy::space(&self.directory)?;
        self.space_pressure
            .store(free <= policy.reserve(total), Ordering::Release);
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
                status["dirtySince"]
                    .as_i64()
                    .map(|at| at.max(self.active_since.load(Ordering::Acquire))),
                crate::config::now(),
            )
        };
        status["mode"] = "on-demand".into();
        status["grantId"] = self.source.grant_id().into();
        status["waitingFor"] = reason.into();
        status["freeBytes"] = free.into();
        status["reserveBytes"] = policy.reserve(total).into();
        status["backupSeconds"] = policy.backup_seconds.into();
        status["maxDirtySeconds"] = policy.max_dirty_seconds.into();
        status["activeSince"] = self.active_since.load(Ordering::Acquire).into();
        status["backupUrgent"] = status["dirtySince"]
            .as_i64()
            .is_some_and(|at| {
                crate::config::now().saturating_sub(at) >= (policy.backup_seconds * 1000) as i64
            })
            .into();
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
        self.admit_write(bytes.len(), 1, || self.disk.write_at(offset, bytes))
    }

    fn write_batch(&self, writes: &[DiskWrite<'_>]) -> io::Result<()> {
        if writes.len() > 128 {
            return Err(io::Error::other("Too many disk writes in one batch"));
        }
        let bytes = writes.iter().try_fold(0usize, |total, write| {
            total
                .checked_add(write.bytes.len())
                .filter(|total| *total <= 8 * 1024 * 1024)
                .ok_or_else(|| io::Error::other("Disk write batch exceeds its byte limit"))
        })?;
        self.admit_write(bytes, writes.len(), || self.disk.write_batch(writes))
    }

    fn sync(&self) -> io::Result<()> {
        let result = self.disk.sync();
        if result.is_err() {
            self.fault.store(true, Ordering::SeqCst);
        }
        result
    }
}

impl Volume {
    fn admit_write(
        &self,
        bytes: usize,
        frames: usize,
        write: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let sample = self.disk.start_write_admission();
        // Serialize space checks and reservations across the node, then release
        // admission before journal I/O. Pending writes remain conservatively
        // charged to every writer/cache-fill check until their I/O finishes.
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
                    .saturating_add(bytes as u64 * 4 + 1024 * 1024)
            {
                self.pressure.store(false, Ordering::SeqCst);
                self.space_pressure.store(false, Ordering::Release);
                let state = self
                    .directory
                    .parent()
                    .and_then(Path::parent)
                    .ok_or_else(|| io::Error::other("Invalid disk path"))?;
                let _headroom =
                    super::cache::reserve_write(&guard, bytes as u64 * 4 + 1024 * 1024)?;
                crate::microvm::budget::charge_disk(
                    state,
                    bytes as u64 * 4 + frames as u64 * 4096,
                )?;
                drop(guard);
                let result = write();
                if result.is_err() {
                    self.fault.store(true, Ordering::SeqCst);
                } else {
                    sample.finish(bytes);
                }
                return result;
            }
            self.pressure.store(true, Ordering::SeqCst);
            drop(guard);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
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
        () = stop.cancelled() => return Err(Error::conflict("Disk materialization cancelled.")),
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
    use serde_json::json;

    #[tokio::test]
    async fn resumed_backlog_gets_one_active_window_without_hiding_its_age() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        volume.disk.write_at(0, b"durable backlog").unwrap();
        let dirty_since = volume.disk.dirty_since().unwrap();
        let resumed = dirty_since + 3_600_000;
        volume.active_since.store(resumed, Ordering::Release);
        assert_eq!(volume.control_reason(resumed + 299_999).unwrap(), None);
        assert_eq!(
            volume.control_reason(resumed + 300_000).unwrap(),
            Some("backup-lag")
        );
        assert_eq!(volume.disk.accounting().unwrap()["dirtySince"], dirty_since);
        volume.fault.store(true, Ordering::SeqCst);
        assert_eq!(volume.control_reason(resumed).unwrap(), Some("integrity"));
        volume.fault.store(false, Ordering::SeqCst);
        volume.pressure.store(true, Ordering::SeqCst);
        assert_eq!(volume.control_reason(resumed).unwrap(), Some("disk-space"));
    }

    #[tokio::test]
    async fn slow_policy_files_do_not_suspend_a_safe_vm() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let (volume, attempt, _) = controller_fixture(root.path()).await;
        let path = root.path().join("storage-policy.json");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        // Any policy-file read blocks indefinitely. CPU safety must rely on the
        // configured policy, atomics and locally enforced write admission.
        let stop = CancellationToken::new();
        let started = std::time::Instant::now();
        volume
            .enforce_limits(root.path(), &attempt, &stop)
            .await
            .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
        assert!(!volume.paused());
        assert!(!stop.is_cancelled());
    }

    async fn controller_fixture(root: &Path) -> (Arc<Volume>, String, tokio::net::UnixListener) {
        let directory = root.join("disks/conversation");
        let context = json!({"master":"http://127.0.0.1:1/","grant":"fixture",
            "policy":{"reserveMiB":64,"reservePercent":1}});
        drop(
            create(
                &directory.join("lazy"),
                &json!({"version":1,"size":4096,"blockSize":4194304,
            "blocks":[{"offset":0,"size":4096,"hash":null}]}),
                &context,
            )
            .await
            .unwrap(),
        );
        let volume = load(&directory).await.unwrap();
        let attempt = "attempt".to_owned();
        let vm = uuid::Uuid::new_v4().to_string();
        let jail = root.join("jails/firecracker").join(&vm).join("root");
        std::fs::create_dir_all(&jail).unwrap();
        std::fs::write(root.join("attempt.vm.json"), json!({"vmId":vm}).to_string()).unwrap();
        (
            volume,
            attempt,
            tokio::net::UnixListener::bind(jail.join("api.sock")).unwrap(),
        )
    }

    async fn request_state(
        stream: tokio::net::UnixStream,
    ) -> (tokio::io::BufReader<tokio::net::UnixStream>, String) {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        let mut stream = tokio::io::BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        (stream, value["state"].as_str().unwrap().to_owned())
    }

    #[tokio::test]
    async fn lost_pause_response_retries_latest_state_without_restarting() {
        use tokio::io::AsyncWriteExt;
        let root = tempfile::tempdir().unwrap();
        let (volume, attempt, listener) = controller_fixture(root.path()).await;
        let server = tokio::spawn(async move {
            let (_, state) = request_state(listener.accept().await.unwrap().0).await;
            assert_eq!(state, "Paused"); // applied, but the connection loses its reply
            let (mut stream, state) = request_state(listener.accept().await.unwrap().0).await;
            assert_eq!(state, "Resumed");
            stream
                .get_mut()
                .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
                .await
                .unwrap();
        });
        let stop = CancellationToken::new();
        volume.pressure.store(true, Ordering::SeqCst);
        volume
            .enforce_limits(root.path(), &attempt, &stop)
            .await
            .unwrap();
        assert!(!stop.is_cancelled());
        assert!(volume.transition.lock().unwrap().is_some());
        volume.pressure.store(false, Ordering::SeqCst);
        volume
            .enforce_limits(root.path(), &attempt, &stop)
            .await
            .unwrap();
        assert!(!stop.is_cancelled());
        assert!(!volume.paused());
        assert!(volume.transition.lock().unwrap().is_none());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn sustained_unknown_transition_still_fences_the_attempt() {
        let root = tempfile::tempdir().unwrap();
        let (volume, attempt, listener) = controller_fixture(root.path()).await;
        let server = tokio::spawn(async move {
            let (_, state) = request_state(listener.accept().await.unwrap().0).await;
            assert_eq!(state, "Paused");
        });
        let stop = CancellationToken::new();
        volume.pressure.store(true, Ordering::SeqCst);
        *volume.transition.lock().unwrap() =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(31));
        assert!(
            volume
                .enforce_limits(root.path(), &attempt, &stop)
                .await
                .is_err()
        );
        assert!(stop.is_cancelled());
        assert!(volume.stop.is_cancelled());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn delayed_pause_response_is_retried_and_explicit_refusal_still_stops() {
        use tokio::io::AsyncWriteExt;
        let root = tempfile::tempdir().unwrap();
        let (volume, attempt, listener) = controller_fixture(root.path()).await;
        let server = tokio::spawn(async move {
            let (stream, state) = request_state(listener.accept().await.unwrap().0).await;
            assert_eq!(state, "Paused");
            let delayed = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                let mut stream = stream;
                let _ = stream
                    .get_mut()
                    .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
                    .await;
            });
            let (mut stream, state) = request_state(listener.accept().await.unwrap().0).await;
            assert_eq!(state, "Paused");
            stream
                .get_mut()
                .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
                .await
                .unwrap();
            let (mut stream, state) = request_state(listener.accept().await.unwrap().0).await;
            assert_eq!(state, "Resumed");
            stream
                .get_mut()
                .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                .await
                .unwrap();
            delayed.await.unwrap();
        });
        let stop = CancellationToken::new();
        volume.pressure.store(true, Ordering::SeqCst);
        volume
            .enforce_limits(root.path(), &attempt, &stop)
            .await
            .unwrap();
        assert!(!stop.is_cancelled());
        assert!(
            !volume.paused(),
            "an unconfirmed pause must not be marked complete"
        );
        volume
            .enforce_limits(root.path(), &attempt, &stop)
            .await
            .unwrap();
        assert!(volume.paused());
        volume.pressure.store(false, Ordering::SeqCst);
        assert!(
            volume
                .enforce_limits(root.path(), &attempt, &stop)
                .await
                .is_err()
        );
        assert!(stop.is_cancelled());
        assert!(volume.stop.is_cancelled());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cache_admission_contention_does_not_interrupt_the_vm() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        let context = json!({
            "master": "http://127.0.0.1:1/", "grant": "fixture",
            "policy": { "reserveMiB": 64, "reservePercent": 1 }
        });
        drop(
            create(
                &directory.join("lazy"),
                &json!({
                    "version": 1, "size": 4096, "blockSize": 4194304,
                    "blocks": [{"offset": 0, "size": 4096, "hash": null}]
                }),
                &context,
            )
            .await
            .unwrap(),
        );
        let volume = load(&directory).await.unwrap();
        std::fs::write(
            root.path().join("attempt.vm.json"),
            json!({"vmId":uuid::Uuid::new_v4().to_string()}).to_string(),
        )
        .unwrap();
        let (ready, acquired) = std::sync::mpsc::channel();
        let admission = std::thread::spawn(move || {
            let _guard = super::super::cache::admission().unwrap();
            ready.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1300));
        });
        acquired.recv().unwrap();
        let stop = CancellationToken::new();
        let start = std::time::Instant::now();
        let result = volume.enforce_limits(root.path(), "attempt", &stop).await;
        let elapsed = start.elapsed();
        admission.join().unwrap();
        assert!(
            result.is_ok(),
            "contention was treated as a VM failure: {result:?}"
        );
        assert!(!stop.is_cancelled());
        assert!(!volume.paused());
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "health waited on maintenance: {elapsed:?}"
        );
    }

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
        let context = json!({
            "master": origin,
            "grant": "fixture",
            "policy": policy
        });
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
            &json!({
                "version": 1,
                "size": 4096,
                "blockSize": 4194304,
                "blocks": [{"offset": 0,"size": 4096,"hash": "a".repeat(64)}]
            }),
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
        let manifest = json!({
            "version": 1,
            "size": 4096,
            "blockSize": 4194304,
            "blocks": [{"offset": 0,"size": 4096,"hash": null}]
        });
        let context = json!({
            "master": "http://127.0.0.1:1/",
            "grant": "fixture",
            "policy": Policy::default()
        });
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
        let context = json!({
            "master": "http://127.0.0.1:1/",
            "grant": "test",
            "policy": policy
        });
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
            &json!({
                "version": 1,
                "size": 4096,
                "blockSize": 4194304,
                "blocks": [{"offset": 0,"size": 4096,"hash": null}]
            }),
            source,
        )
        .unwrap();
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
