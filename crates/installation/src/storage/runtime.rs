//! Controller-owned mounted disks. One live journal per conversation directory.
use super::{Disk, DiskWrite, LazyDisk, policy::Policy, source::Source};
use crate::error::{Error, Result};
use serde_json::Value;
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, RwLock, TryLockError, Weak,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

enum Entry {
    Open {
        volume: Weak<Volume>,
        disk: Weak<LazyDisk>,
    },
    Replacing,
}

type DiskEntry = Arc<Mutex<Option<Entry>>>;

type Registry = Mutex<HashMap<PathBuf, DiskEntry>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

fn entry(directory: &Path) -> Result<DiskEntry> {
    Ok(registry()
        .lock()
        .map_err(Error::internal)?
        .entry(directory.to_owned())
        .or_insert_with(|| Arc::new(Mutex::new(None)))
        .clone())
}

#[cfg(test)]
pub(crate) fn with_registry_locked(operation: impl FnOnce()) {
    let _registry = registry().lock().unwrap();
    operation();
}

/// Apply node limits without policy-file reads in the CPU safety path.
pub(crate) fn configure(state: &Path, policy: &Policy) -> Result<()> {
    super::NodeBlockCache::new(state)?.resize(policy.memory_cache_mi_b as usize * 1024 * 1024)?;
    let entries: Vec<_> = registry()
        .lock()
        .map_err(Error::internal)?
        .iter()
        .map(|(directory, entry)| (directory.clone(), entry.clone()))
        .collect();
    for (directory, entry) in entries {
        if directory.parent().and_then(Path::parent) != Some(state) {
            continue;
        }
        let entry = match entry.try_lock() {
            Ok(entry) => entry,
            Err(TryLockError::WouldBlock) => continue,
            Err(error) => return Err(Error::internal(error)),
        };
        let Some(Entry::Open { volume, .. }) = entry.as_ref() else {
            continue;
        };
        let Some(volume) = volume.upgrade() else {
            continue;
        };
        *volume.control_policy.write().map_err(Error::internal)? = policy.clone();
        let (total, free) = super::policy::space(&directory)?;
        volume
            .space_pressure
            .store(free <= policy.reserve(total), Ordering::Release);
        volume
            .disk
            .memory_budget(policy.memory_cache_mi_b as usize * 1024 * 1024)?;
    }
    Ok(())
}

/// Longest pause waiting for a late backup before the VM continues without it.
pub const BACKUP_LAG_PAUSE_MS: i64 = 10 * 60 * 1000;

/// An attempt keeps its acquired volume's stop token independently of the registry.
/// Normal completion cancels only `execution`, so a retained VM can keep its disk.
pub(crate) struct AttemptStop {
    pub execution: CancellationToken,
    volume: OnceLock<CancellationToken>,
}

impl AttemptStop {
    pub fn new(execution: CancellationToken) -> Self {
        Self {
            execution,
            volume: OnceLock::new(),
        }
    }

    pub fn bind(&self, volume: &Volume) -> Result<()> {
        self.volume
            .set(volume.stop.clone())
            .map_err(|_| Error::internal("Execution disk is already bound."))?;

        // Publish before checking expiry. Either cancel sees this token, or
        // acquisition sees the cancellation, including a late journal open.
        if self.execution.is_cancelled() {
            volume.stop.cancel();
            return Err(Error::unavailable("VM preparation stopped."));
        }

        Ok(())
    }

    pub fn cancel(&self) {
        self.execution.cancel();
        if let Some(volume) = self.volume.get() {
            volume.cancel();
        }
    }
}

pub struct Volume {
    pub disk: Arc<LazyDisk>,
    pub source: Arc<Source>,
    pub stop: CancellationToken,
    directory: PathBuf,
    policy: RwLock<Policy>,
    control_policy: RwLock<Policy>,
    active_since: AtomicI64,
    active_attempt: Mutex<String>,
    /// When the current late backlog first paused the VM; zero when none.
    backup_lag_since: AtomicI64,
    /// The backlog outlived its pause: the VM runs while backups keep failing.
    backup_degraded: AtomicBool,
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
}

pub fn live(directory: &Path) -> Option<Arc<Volume>> {
    let entry = registry().try_lock().ok()?.get(directory)?.clone();
    let entry = entry.try_lock().ok()?;
    match entry.as_ref()? {
        Entry::Open { volume, .. } => volume.upgrade(),
        Entry::Replacing => None,
    }
}

fn closing_error() -> Error {
    Error::unavailable(
        "Conversation disk is still closing. Its journal has been preserved; retry when its readers have finished.",
    )
}

pub fn open(directory: &Path) -> Result<Arc<Volume>> {
    try_open(directory)?.ok_or_else(closing_error)
}

fn try_open(directory: &Path) -> Result<Option<Arc<Volume>>> {
    try_open_with(directory, open_volume)
}

fn try_open_with(
    directory: &Path,
    open: impl FnOnce(&Path) -> Result<Arc<Volume>>,
) -> Result<Option<Arc<Volume>>> {
    let entry = entry(directory)?;
    // Do not occupy the bounded I/O pool while another task opens this disk.
    // The async loader retries without delaying any other conversation.
    let mut entry = match entry.try_lock() {
        Ok(entry) => entry,
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(error) => return Err(Error::internal(error)),
    };
    match entry.as_ref() {
        Some(Entry::Open { volume, disk }) => {
            if let Some(volume) = volume.upgrade() {
                if volume.stop.is_cancelled() {
                    return Ok(None);
                }
                return Ok(Some(volume));
            }
            // A publication may retain the journal after its Volume is gone.
            // Its exclusive writer lock must drain before reopening it.
            if disk.strong_count() > 0 {
                return Ok(None);
            }
        }
        Some(Entry::Replacing) => return Err(Error::conflict("Disk replacement is in progress.")),
        None => {}
    }
    let volume = open(directory)?;
    *entry = Some(Entry::Open {
        volume: Arc::downgrade(&volume),
        disk: Arc::downgrade(&volume.disk),
    });
    Ok(Some(volume))
}

fn open_volume(directory: &Path) -> Result<Arc<Volume>> {
    let root = directory.join("lazy");
    let context = LazyDisk::context(&root)?;
    let stop = CancellationToken::new();
    let unassigned = context["unassigned"] == true;
    if unassigned && (context.get("master").is_some() || context.get("grant").is_some()) {
        return Err(Error::conflict(
            "Unassigned disk must not carry remote authority.",
        ));
    }
    if unassigned
        && directory
            .parent()
            .and_then(Path::file_name)
            .is_none_or(|name| name != "environments")
    {
        return Err(Error::conflict(
            "Unassigned disks must belong to the prepared environment namespace.",
        ));
    }
    let source = Arc::new(if unassigned {
        Source::prepared(tokio::runtime::Handle::current(), stop.clone())
    } else {
        Source::new(&context, tokio::runtime::Handle::current(), stop.clone())?
    });
    let disk = Arc::new(LazyDisk::open(&root, source.clone())?);
    if unassigned && disk.has_remote_base()? {
        return Err(Error::conflict(
            "Unassigned disk must not contain a remote base.",
        ));
    }
    let policy: Policy = serde_json::from_value(context["policy"].clone()).unwrap_or_default();
    policy.validate().map_err(Error::bad)?;
    disk.memory_budget(policy.memory_cache_mi_b as usize * 1024 * 1024)?;
    let volume = Arc::new(Volume {
        disk,
        source,
        stop,
        directory: directory.to_owned(),
        control_policy: RwLock::new(policy.clone()),
        policy: RwLock::new(policy),
        active_since: AtomicI64::new(0),
        active_attempt: Mutex::new(String::new()),
        backup_lag_since: AtomicI64::new(0),
        backup_degraded: AtomicBool::new(false),
        pressure: AtomicBool::new(false),
        space_pressure: AtomicBool::new(false),
        fault: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        transition: Mutex::new(None),
    });
    volume.health()?;
    Ok(volume)
}

/// The caller also holds the conversation disk lock. Existing inspections must
/// finish before replacement; subsequent opens are rejected until the durable switch.
pub struct Replacement {
    directory: PathBuf,
}

impl Drop for Replacement {
    fn drop(&mut self) {
        if let Ok(entry) = entry(&self.directory)
            && let Ok(mut entry) = entry.lock()
        {
            *entry = None;
        }
    }
}

pub async fn replacement(directory: &Path) -> Result<Replacement> {
    let directory = directory.to_owned();
    blocking(move || {
        let entry = entry(&directory)?;
        let mut entry = match entry.try_lock() {
            Ok(entry) => entry,
            Err(TryLockError::WouldBlock) => {
                return Err(Error::conflict("Disk is opening; retry restoration."));
            }
            Err(error) => return Err(Error::internal(error)),
        };
        match entry.as_ref() {
            Some(Entry::Replacing) => {
                return Err(Error::conflict("Disk replacement is in progress."));
            }
            Some(Entry::Open { volume, disk })
                if volume.strong_count() > 0 || disk.strong_count() > 0 =>
            {
                return Err(Error::conflict("Disk is still in use; retry restoration."));
            }
            _ => {}
        }
        *entry = Some(Entry::Replacing);
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
    load_with_wait(directory, std::time::Duration::from_secs(60)).await
}

async fn load_with_wait(directory: &Path, wait: std::time::Duration) -> Result<Arc<Volume>> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let path = directory.to_owned();
        if let Some(volume) = blocking(move || try_open(&path)).await? {
            return Ok(volume);
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(closing_error());
        }
        // Release both the registry lock and the bounded I/O permit while the
        // previous readers drain; another disk must still be able to open.
        tokio::time::sleep(remaining.min(std::time::Duration::from_millis(20))).await;
    }
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
    create_source(directory, manifest, context, generation, false).await
}

pub(crate) async fn create_prepared(
    directory: &Path,
    manifest: &Value,
    context: &Value,
) -> Result<Arc<LazyDisk>> {
    if context["unassigned"] != true
        || context.get("master").is_some()
        || context.get("grant").is_some()
        || manifest["blocks"]
            .as_array()
            .is_none_or(|blocks| blocks.iter().any(|block| !block["hash"].is_null()))
    {
        return Err(Error::bad("Prepared disks require a local-only base."));
    }
    create_source(directory, manifest, context, 1, true).await
}

/// Prepare a private clone in a disposable environment before publishing it
/// atomically into the conventional native disk. Startup removes abandoned
/// temporary environments; no new authoritative pending format is introduced.
pub(crate) async fn clone_native_base(source: &Path, target: &Path, context: &Value) -> Result<()> {
    let physical = target
        .parent()
        .ok_or_else(|| Error::bad("Missing clone environment."))?;
    if target.file_name().is_none_or(|name| name != "lazy")
        || physical
            .parent()
            .and_then(Path::file_name)
            .is_none_or(|name| name != "environments")
        || physical.join("owner").exists()
    {
        return Err(Error::conflict(
            "Native bases require a new unclaimed environment.",
        ));
    }
    crate::validation::uuid(
        physical
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(""),
    )?;
    let (source, target, context) = (source.to_owned(), target.to_owned(), context.clone());
    blocking(move || {
        // Validate current authority; immutable cached bases never carry it.
        Source::new(
            &context,
            tokio::runtime::Handle::current(),
            CancellationToken::new(),
        )?;
        LazyDisk::clone_sealed(&source, &target)?;
        LazyDisk::bind_cloned_context(&target, &context)?;
        Ok(())
    })
    .await
}

async fn create_source(
    directory: &Path,
    manifest: &Value,
    context: &Value,
    generation: i64,
    prepared: bool,
) -> Result<Arc<LazyDisk>> {
    let (directory, manifest, context) = (directory.to_owned(), manifest.clone(), context.clone());
    blocking(move || {
        let source = Arc::new(if prepared {
            Source::prepared(tokio::runtime::Handle::current(), CancellationToken::new())
        } else {
            Source::new(
                &context,
                tokio::runtime::Handle::current(),
                CancellationToken::new(),
            )?
        });
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
    /// Called only after the environment's owner was durably assigned. The
    /// journal and VMM stay open; an existing grant is never replaced.
    pub async fn authorize(
        self: &Arc<Self>,
        owner: &super::environment::OwnerLease,
        context: &Value,
    ) -> Result<()> {
        if owner.directory != self.directory
            || self
                .directory
                .parent()
                .and_then(Path::file_name)
                .is_none_or(|name| name != "environments")
            || context["unassigned"] == true
        {
            return Err(Error::conflict(
                "Disk authorization requires its prepared environment owner.",
            ));
        }
        let policy: Policy = serde_json::from_value(context["policy"].clone())?;
        policy.validate().map_err(Error::bad)?;
        let (volume, owner, context) = (self.clone(), owner.clone(), context.clone());
        blocking(move || {
            // A dropped async caller must not unlock the conversation while
            // its blocking authorization fsync can still change this journal.
            let _owner = owner;
            volume.source.assign(&context, || {
                volume.disk.set_context(&context)?;
                volume.disk.sync()?;
                Ok(())
            })?;
            *volume.policy.write().map_err(Error::internal)? = policy.clone();
            *volume.control_policy.write().map_err(Error::internal)? = policy;
            Ok(())
        })
        .await
    }

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

    pub async fn seal_completed(self: &Arc<Self>) -> Result<i64> {
        let disk = self.disk.clone();
        blocking(move || Ok(disk.seal_completed()?)).await
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
        let late = self.disk.dirty_since().is_some_and(|at| {
            now.saturating_sub(at.max(active_since)) >= (policy.max_dirty_seconds * 1000) as i64
        });
        if !late {
            self.backup_lag_since.store(0, Ordering::Release);
            self.backup_degraded.store(false, Ordering::Release);
            return Ok(None);
        }

        // The pause gives backups time to catch up. When they keep failing,
        // the VM continues without them instead of freezing indefinitely.
        let since = match self.backup_lag_since.compare_exchange(
            0,
            now,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => now,
            Err(since) => since,
        };
        let expired = now.saturating_sub(since) >= BACKUP_LAG_PAUSE_MS;
        self.backup_degraded.store(expired, Ordering::Release);
        Ok((!expired).then_some("backup-lag"))
    }

    /// VM and native attempts share the bounded active synchronization window.
    /// This never changes the original dirty timestamp or an existing attempt's
    /// deadline, even when a monitor retries a transition.
    pub(crate) fn activate_attempt(&self, attempt: &str) -> Result<()> {
        let mut active = self.active_attempt.lock().map_err(Error::internal)?;
        if active.as_str() != attempt {
            self.active_since
                .store(crate::config::now(), Ordering::Release);
            *active = attempt.to_owned();
        }
        Ok(())
    }

    /// CPU protection must not wait for filesystem accounting or remote reads.
    pub(crate) fn cpu_pause_reason(&self) -> Result<Option<&'static str>> {
        self.control_reason(crate::config::now())
    }

    /// Called under the attempt's control lock, shared with checkpoint capture.
    pub async fn enforce_limits(
        self: &Arc<Self>,
        state: &Path,
        attempt: &str,
        stop: &CancellationToken,
    ) -> Result<()> {
        use crate::microvm::suspension::{self, Reason};
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
        // A resumed VM gets one bounded active synchronization window. Its
        // original dirty timestamp is retained for backup urgency and telemetry.
        self.activate_attempt(attempt)?;
        // No filesystem scan, policy read, journal lock, or blocking task belongs
        // in this CPU safety decision. Writers fence space admission themselves.
        let reason = match self.cpu_pause_reason() {
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
            tracing::info!(target: "cairn_performance", operation = "storage_backpressure", id = attempt, phase = "requested", paused = blocked, reason = reason.unwrap_or("ready"));
            let result = suspension::set_reason(state, attempt, &Reason::Storage, blocked).await;
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
            tracing::info!(target: "cairn_performance", operation = "storage_backpressure", id = attempt, phase = "confirmed", paused = blocked, reason = reason.unwrap_or("ready"), elapsed_ms = transition_started.elapsed().as_millis() as u64);
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
                policy.validate().map_err(io::Error::other)?;
                Ok(policy)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(self
                .policy
                .read()
                .map_err(|error| io::Error::other(error.to_string()))?
                .clone()),
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
        status["completedGeneration"] = self
            .disk
            .completed()?
            .and_then(|receipt| receipt["generation"].as_i64())
            .into();
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
        let degraded = self.backup_degraded.load(Ordering::Acquire);
        let reason = reason.filter(|reason| !(degraded && *reason == "backup-lag"));
        status["backupDegraded"] = degraded.into();
        status["mode"] = "on-demand".into();
        status["grantId"] = self.source.authorization()?.into();
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

#[derive(Clone, Copy)]
enum WriteAdmission {
    Wait,
    BestEffort,
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
        self.admit_write(bytes.len(), 1, WriteAdmission::Wait, || {
            self.disk.write_at(offset, bytes)
        })
    }

    fn discard(&self, offset: u64, length: usize) -> io::Result<()> {
        super::device::range(self.size(), offset, length)?;
        if self.stop.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Disk stopped"));
        }
        // The shared loop determines no-op ranges before asking for write
        // admission. An empty sparse tail must not debit the node's quota.
        let result = self.disk.discard_with(offset, length, |position, bytes| {
            self.admit_write(bytes.len(), 1, WriteAdmission::BestEffort, || {
                self.disk.write_at(position, bytes)
            })
        });
        if result.as_ref().is_err_and(|error| {
            !matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            )
        }) {
            self.fault.store(true, Ordering::SeqCst);
        }
        result
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
        self.admit_write(bytes, writes.len(), WriteAdmission::Wait, || {
            self.disk.write_batch(writes)
        })
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
        admission: WriteAdmission,
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
            if matches!(admission, WriteAdmission::BestEffort) {
                // The VMM handles native discard on its control thread. Routine
                // maintenance must not wait for a space-pressure pause that
                // requires that same thread to service the management API.
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Discard deferred for disk space",
                ));
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
    use crate::{
        microvm::suspension,
        storage::{BlockSource, environment, remote::RemoteSource},
    };
    use serde_json::json;

    fn prepared_manifest() -> Value {
        json!({
            "version": 1, "size": 4096, "blockSize": 4_194_304,
            "blocks": [{ "offset": 0, "size": 4096, "hash": null }],
        })
    }

    fn prepared_context() -> Value {
        json!({ "unassigned": true, "policy": { "reserveMiB": 64, "reservePercent": 1 } })
    }

    #[tokio::test]
    async fn slow_disk_open_does_not_block_another_conversation_or_fill_the_io_pool() {
        let root = tempfile::tempdir().unwrap();
        let slow = root.path().join("disks/slow");
        let other_root = tempfile::tempdir().unwrap();
        let (other, _, _api) = controller_fixture(other_root.path()).await;
        let (entered, opening) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let opening_path = slow.clone();
        let worker = std::thread::spawn(move || {
            try_open_with(&opening_path, |_| {
                entered.send(()).unwrap();
                wait.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Err(Error::unavailable("Synthetic slow opening finished."))
            })
        });
        opening.await.unwrap();

        let mut waiters = Vec::new();
        for _ in 0..16 {
            let path = slow.clone();
            waiters.push(tokio::spawn(async move {
                load_with_wait(&path, std::time::Duration::from_millis(100)).await
            }));
        }
        let loaded = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            load(&other_root.path().join("disks/conversation")),
        )
        .await;
        for waiter in waiters {
            assert!(waiter.await.unwrap().err().unwrap().is_unavailable());
        }
        release.send(()).unwrap();
        assert!(worker.join().unwrap().err().unwrap().is_unavailable());
        assert!(Arc::ptr_eq(&loaded.unwrap().unwrap(), &other));
    }

    #[tokio::test]
    async fn expired_attempt_stops_a_volume_acquired_after_expiry() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _api) = controller_fixture(root.path()).await;
        let attempt = AttemptStop::new(CancellationToken::new());
        attempt.cancel();

        assert!(attempt.bind(&volume).unwrap_err().is_unavailable());
        assert!(volume.stop.is_cancelled());
        assert_eq!(
            volume.write_at(0, b"late").unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[tokio::test]
    async fn completed_attempt_preserves_the_disk_until_the_next_attempt_expires() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _api) = controller_fixture(root.path()).await;
        let completed = AttemptStop::new(CancellationToken::new());
        completed.bind(&volume).unwrap();
        completed.execution.cancel();
        drop(completed);
        volume.write_at(0, b"retained").unwrap();

        let next = AttemptStop::new(CancellationToken::new());
        next.bind(&volume).unwrap();
        next.cancel();
        assert_eq!(
            volume.write_at(0, b"expired").unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[tokio::test]
    async fn reopened_conversation_never_returns_a_stopped_disk() {
        let root = tempfile::tempdir().unwrap();
        let (previous, _, _api) = controller_fixture(root.path()).await;
        previous.write_at(0, b"BEFORE").unwrap();
        previous.sync().unwrap();

        // VM shutdown cancels this token. An inspector/publication task can
        // retain the previous Volume while another VM loads the same disk.
        previous.stop.cancel();
        let directory = root.path().join("disks/conversation");
        let reopening = load(&directory);
        tokio::pin!(reopening);
        if let Ok(early) =
            tokio::time::timeout(std::time::Duration::from_millis(40), &mut reopening).await
        {
            let volume = early.unwrap();
            panic!(
                "VM boot received a disk before its stopped reader drained: same_volume={}, stopped={}, write={:?}",
                Arc::ptr_eq(&previous, &volume),
                volume.stop.is_cancelled(),
                volume.write_at(8, b"AFTER!"),
            );
        }
        assert!(previous.stop.is_cancelled());
        drop(previous);
        let reopened = tokio::time::timeout(std::time::Duration::from_secs(1), reopening)
            .await
            .unwrap()
            .unwrap();
        reopened.write_at(8, b"AFTER!").unwrap();
        let mut bytes = [0; 6];
        reopened.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"BEFORE");
    }

    #[tokio::test]
    async fn stopped_conversation_reopens_after_last_owner_is_released() {
        let root = tempfile::tempdir().unwrap();
        let (previous, _, _api) = controller_fixture(root.path()).await;
        previous.write_at(0, b"BEFORE").unwrap();
        previous.sync().unwrap();
        previous.stop.cancel();
        drop(previous);

        let reopened = load(&root.path().join("disks/conversation")).await.unwrap();
        reopened.write_at(8, b"AFTER!").unwrap();
        let mut bytes = [0; 6];
        reopened.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"BEFORE");
        reopened.read_at(8, &mut bytes).unwrap();
        assert_eq!(&bytes, b"AFTER!");
    }

    #[tokio::test]
    async fn stopped_journal_wait_is_bounded_and_preserves_other_disk_admission() {
        let root = tempfile::tempdir().unwrap();
        let (previous, _, _api) = controller_fixture(root.path()).await;
        previous.write_at(0, b"BEFORE").unwrap();
        let journal = previous.disk.clone();
        previous.stop.cancel();
        drop(previous);
        let directory = root.path().join("disks/conversation");
        assert!(replacement(&directory).await.err().unwrap().is_conflict());

        let waiting = load_with_wait(&directory, std::time::Duration::from_millis(80));
        let other_root = tempfile::tempdir().unwrap();
        let (other, _, _other_api) = controller_fixture(other_root.path()).await;
        let other_directory = other_root.path().join("disks/conversation");
        let other_open = async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let opened = load(&other_directory).await.unwrap();
            assert!(Arc::ptr_eq(&opened, &other));
            opened.write_at(0, b"OTHER").unwrap();
        };
        let (result, ()) = tokio::join!(waiting, other_open);
        assert!(result.err().unwrap().is_unavailable());
        let mut bytes = [0; 6];
        journal.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"BEFORE");
        drop(journal);

        let reopened = load(&directory).await.unwrap();
        reopened.write_at(8, b"AFTER!").unwrap();
        reopened.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"BEFORE");
    }

    #[tokio::test]
    #[ignore = "explicit local stopped-volume read/write measurement; run alone"]
    async fn stopped_volume_reopen_measurement() {
        fn cpu_micros() -> u64 {
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
            assert_eq!(
                unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
                0
            );
            let usage = unsafe { usage.assume_init() };
            ((usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) * 1_000_000
                + usage.ru_utime.tv_usec
                + usage.ru_stime.tv_usec) as u64
        }

        for cancelled in [false, true] {
            let root = tempfile::tempdir_in("/var/tmp").unwrap();
            let (previous, _, _api) = controller_fixture(root.path()).await;
            let bytes = [7; 4096];
            previous.write_at(0, &bytes).unwrap();
            previous.sync().unwrap();
            let directory = root.path().join("disks/conversation");
            let attempt = AttemptStop::new(CancellationToken::new());
            attempt.bind(&previous).unwrap();
            let cpu = cpu_micros();
            let opening = std::time::Instant::now();
            let releasing = if cancelled {
                attempt.cancel();
                Some(tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    drop(previous);
                }))
            } else {
                drop(previous);
                None
            };
            let reopened = load(&directory).await.unwrap();
            let open_us = opening.elapsed().as_micros();
            if let Some(releasing) = releasing {
                releasing.await.unwrap();
            }
            let mut output = [0; 4096];
            let reading = std::time::Instant::now();
            reopened.read_at(0, &mut output).unwrap();
            assert_eq!(output, bytes);
            let cold_read_us = reading.elapsed().as_micros();
            let reading = std::time::Instant::now();
            for _ in 0..256 {
                reopened.read_at(0, &mut output).unwrap();
                assert_eq!(output, bytes);
            }
            let warm_reads_us = reading.elapsed().as_micros();
            let writing = std::time::Instant::now();
            for _ in 0..16 {
                reopened.write_at(0, &bytes).unwrap();
            }
            reopened.sync().unwrap();
            let writes_us = writing.elapsed().as_micros();
            let cpu_us = cpu_micros() - cpu;
            let journal_bytes: u64 = std::fs::read_dir(directory.join("lazy"))
                .unwrap()
                .map(|entry| entry.unwrap().metadata().unwrap())
                .filter(std::fs::Metadata::is_file)
                .map(|metadata| metadata.len())
                .sum();
            println!(
                "{}",
                json!({
                    "cancelled": cancelled,
                    "openUs": open_us,
                    "coldReaderReadUs": cold_read_us,
                    "warmReads256Us": warm_reads_us,
                    "writes16AndSyncUs": writes_us,
                    "cpuUs": cpu_us,
                    "journalFileBytes": journal_bytes,
                    "remoteEncodedBytes": 0,
                })
            );
        }
    }

    #[tokio::test]
    async fn adopted_disk_binds_authority_without_reopening_journal_and_survives_restart() {
        let state = tempfile::tempdir().unwrap();
        let id = crate::config::id();
        let directory = state.path().join("environments").join(&id);
        let context = prepared_context();
        let disk = create_prepared(&directory.join("lazy"), &prepared_manifest(), &context)
            .await
            .unwrap();
        disk.write_at(0, b"BOOT").unwrap();
        drop(disk);
        let volume = load(&directory).await.unwrap();
        let live = volume.disk.clone();
        assert!(volume.inspect().await.unwrap()["grantId"].is_null());
        assert!(volume.source.grant_id().is_err());
        assert_eq!(
            volume
                .source
                .fetch(&format!("b3-{}", "a".repeat(64)))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let wrong = environment::ownership(state.path(), &crate::config::id(), "busy")
            .await
            .unwrap();
        let authorized = json!({
            "master": "http://127.0.0.1:1/",
            "grant": "owner-grant",
            "policy": context["policy"],
        });
        assert!(volume.authorize(&wrong, &authorized).await.is_err());
        assert!(volume.source.grant_id().is_err());
        drop(wrong);
        let physical = crate::file_lock::exclusive(&directory.join("lock"), "busy").unwrap();
        let run = crate::config::id();
        let owner = environment::assign(state.path(), &id, &run).await.unwrap();
        volume.authorize(&owner, &authorized).await.unwrap();
        assert!(Arc::ptr_eq(&live, &volume.disk));
        assert_eq!(
            volume.source.grant_id().unwrap(),
            crate::auth::digest("owner-grant")
        );
        assert!(volume.authorize(&owner, &authorized).await.is_err());
        volume.write_at(4, b"USER").unwrap();
        let mut bytes = [0; 8];
        volume.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"BOOTUSER");
        drop((live, volume, physical, owner));
        let physical = environment::directory(state.path(), &run).unwrap();
        let reopened = load(&physical).await.unwrap();
        assert_eq!(
            reopened.source.grant_id().unwrap(),
            crate::auth::digest("owner-grant")
        );
        reopened.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"BOOTUSER");
    }

    #[tokio::test]
    async fn unassigned_journal_cannot_hide_a_remote_base_or_replace_a_legacy_disk() {
        let state = tempfile::tempdir().unwrap();
        let context = prepared_context();
        let mut remote = prepared_manifest();
        remote["blocks"][0]["hash"] = format!("b3-{}", "a".repeat(64)).into();
        let physical = state.path().join("environments").join(crate::config::id());
        assert!(
            create_prepared(&physical.join("lazy"), &remote, &context)
                .await
                .is_err()
        );
        let authorized = json!({
            "master": "http://127.0.0.1:1/",
            "grant": "fixture",
            "policy": context["policy"],
        });
        let disk = create(&physical.join("lazy"), &remote, &authorized)
            .await
            .unwrap();
        disk.set_context(&context).unwrap();
        drop(disk);
        assert!(load(&physical).await.is_err());
        let legacy = state.path().join("disks").join(crate::config::id());
        drop(
            create_prepared(&legacy.join("lazy"), &prepared_manifest(), &context)
                .await
                .unwrap(),
        );
        assert!(load(&legacy).await.is_err());
    }

    #[tokio::test]
    async fn native_activation_and_control_preserve_cpu_protection_without_a_vm_record() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        volume.disk.write_at(0, b"durable native backlog").unwrap();
        let dirty = volume.disk.dirty_since().unwrap();
        volume.activate_attempt("native-first").unwrap();
        let first = volume.active_since.load(Ordering::Acquire);
        assert!(first >= dirty);
        volume.activate_attempt("native-first").unwrap();
        assert_eq!(volume.active_since.load(Ordering::Acquire), first);
        // Retrying the same attempt cannot reset an expired active window.
        volume.active_since.store(1, Ordering::Release);
        volume.activate_attempt("native-first").unwrap();
        assert_eq!(volume.active_since.load(Ordering::Acquire), 1);
        volume.activate_attempt("native-resumed").unwrap();
        assert!(volume.active_since.load(Ordering::Acquire) >= first);
        assert_eq!(volume.disk.dirty_since().unwrap(), dirty);
        volume.fault.store(true, Ordering::Release);
        assert_eq!(volume.cpu_pause_reason().unwrap(), Some("integrity"));
        volume.fault.store(false, Ordering::Release);
        volume.space_pressure.store(true, Ordering::Release);
        assert_eq!(volume.cpu_pause_reason().unwrap(), Some("disk-space"));
    }

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
    async fn failing_backups_pause_a_vm_once_then_let_it_run() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        volume.disk.write_at(0, b"unsaved work").unwrap();
        let dirty_since = volume.disk.dirty_since().unwrap();
        let lagging = dirty_since + 300_000;
        assert_eq!(volume.control_reason(lagging).unwrap(), Some("backup-lag"));
        assert_eq!(
            volume
                .control_reason(lagging + BACKUP_LAG_PAUSE_MS - 1)
                .unwrap(),
            Some("backup-lag")
        );

        // Production 2026-10-05: a node whose backups kept failing held its
        // VM paused for 11 hours. The VM continues once the pause expires.
        assert_eq!(
            volume
                .control_reason(lagging + BACKUP_LAG_PAUSE_MS)
                .unwrap(),
            None
        );
        assert_eq!(
            volume
                .control_reason(lagging + 10 * BACKUP_LAG_PAUSE_MS)
                .unwrap(),
            None
        );
        assert_eq!(volume.health().unwrap()["backupDegraded"], true);

        // Pauses that protect the disk itself are never bounded.
        volume.pressure.store(true, Ordering::SeqCst);
        assert_eq!(
            volume
                .control_reason(lagging + 10 * BACKUP_LAG_PAUSE_MS)
                .unwrap(),
            Some("disk-space")
        );
        volume.pressure.store(false, Ordering::SeqCst);

        // Once the backlog is no longer late, a later backlog pauses again.
        assert_eq!(volume.control_reason(dirty_since).unwrap(), None);
        assert_eq!(volume.control_reason(lagging).unwrap(), Some("backup-lag"));
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
        suspension::prepare_attempt(root, &attempt, &vm)
            .await
            .unwrap();
        std::fs::write(
            root.join("attempt.vm.json"),
            json!({"vmId":vm,"runId":"run"}).to_string(),
        )
        .unwrap();
        suspension::bind_attempt(root, &attempt, &vm).await.unwrap();
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
        let (volume, _, _api) = controller_fixture(root.path()).await;
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
                "blocks": [{"offset": 0,"size": 4096,"hash": format!("b3-{}", "a".repeat(64))}]
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
    async fn sparse_discard_never_charges_quota_and_reaches_later_deleted_bytes() {
        const BLOCK: u64 = 4 * 1024 * 1024;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        let manifest = json!({
            "version": 1,
            "size": 16 * BLOCK,
            "blockSize": BLOCK,
            "blocks": (0..16).map(|index| json!({
                "offset": index * BLOCK,
                "size": BLOCK,
                "hash": null
            })).collect::<Vec<_>>()
        });
        let context = json!({
            "master": "http://127.0.0.1:1/",
            "grant": "test",
            "policy": Policy::default()
        });
        let disk = create(&directory.join("lazy"), &manifest, &context)
            .await
            .unwrap();
        disk.write_at(15 * BLOCK + 1024, b"deleted").unwrap();
        drop(disk);
        let volume = load(&directory).await.unwrap();
        let budget = crate::microvm::budget::Budget {
            slots: 1,
            limits: crate::nodes::Resources {
                cpu: 1,
                memory_mi_b: 2048,
                disk_mi_b: 128,
            },
        };
        std::fs::write(
            root.path().join(crate::microvm::budget::FILE),
            serde_json::to_vec(&budget).unwrap(),
        )
        .unwrap();
        crate::microvm::budget::disk_bytes(root.path()).unwrap();
        // Charging the empty prefix as writes exhausts this quota before the
        // final block is reached. Only that block needs a zero frame.
        volume.discard(0, volume.size() as usize).unwrap();
        let mut bytes = [1; 7];
        volume.read_at(15 * BLOCK + 1024, &mut bytes).unwrap();
        assert_eq!(bytes, [0; 7]);
        let accounting = volume.disk.accounting().unwrap();
        let charged = crate::microvm::budget::disk_bytes(root.path()).unwrap();
        for _ in 0..8 {
            volume.discard(0, volume.size() as usize).unwrap();
        }
        assert_eq!(volume.disk.accounting().unwrap(), accounting);
        assert!(crate::microvm::budget::disk_bytes(root.path()).unwrap() <= charged);
        assert!(!volume.fault.load(Ordering::SeqCst));
        assert!(!volume.pressure.load(Ordering::SeqCst));
        volume.stop.cancel();
        assert_eq!(
            volume.discard(0, 4096).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn disk_pressure_waits_without_acknowledging_or_losing_writes_and_resumes() {
        // Other tests deliberately hold the process-global admission mutex.
        // Keep the discard deadline and pressure recovery in an isolated process.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "storage::runtime::tests::disk_pressure_recovery_child",
                "--ignored",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "isolated process-global admission and pressure recovery child"]
    async fn disk_pressure_recovery_child() {
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
        let sparse = volume.disk.accounting().unwrap();
        volume.discard(0, 4096).unwrap();
        assert_eq!(volume.disk.accounting().unwrap(), sparse);
        volume.disk.write_at(0, b"deleted").unwrap();
        let dirty_before = volume.disk.accounting().unwrap()["dirtyBytes"].clone();
        let started = std::time::Instant::now();
        assert_eq!(
            volume.discard(0, 4096).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(!volume.fault.load(Ordering::SeqCst));
        assert!(!volume.pressure.load(Ordering::SeqCst));
        let writer = {
            let volume = volume.clone();
            tokio::task::spawn_blocking(move || volume.write_at(3, b"retained"))
        };
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(!writer.is_finished());
        assert_eq!(volume.status().unwrap()["waitingFor"], "disk-space");
        assert_eq!(
            volume.disk.accounting().unwrap()["dirtyBytes"],
            dirty_before
        );
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
