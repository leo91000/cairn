//! Controller-owned mounted disks. One live journal per conversation directory.
use super::{Disk, DiskWrite, LazyDisk, assignment::Assignment, policy::Policy};
use crate::{
    error::{Error, Result},
    microvm::budget::allocated,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, RwLock, TryLockError, Weak,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
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

    #[cfg(test)]
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
    pub(crate) assignment: Arc<Assignment>,
    pub stop: CancellationToken,
    directory: PathBuf,
    maintenance: Option<tokio::runtime::Handle>,
    policy: RwLock<Policy>,
    task_limit: AtomicU64,
    task_usage: Mutex<Option<TaskUsage>>,
    task_pending: super::cache::PendingWrites,
    task_waiting: AtomicU64,
    flush_budget_failed: AtomicBool,
    active_since: AtomicI64,
    active_attempt: Mutex<String>,
    pressure: AtomicBool,
    space_pressure: AtomicBool,
    fault: AtomicBool,
    paused: AtomicBool,
    transition: Mutex<Option<std::time::Instant>>,
}

struct TaskUsage {
    sampled: std::time::Instant,
    bytes: u64,
    revision: u64,
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

/// Task accounting excludes only the bounded internal replacement copy.
/// Node accounting continues to use every physically allocated file.
#[cfg(test)]
pub(crate) fn retained_backing_bytes(directory: &Path) -> io::Result<u64> {
    let entry = registry()
        .lock()
        .map_err(|error| io::Error::other(error.to_string()))?
        .get(directory)
        .cloned();
    let (volume, disk) = if let Some(entry) = entry {
        // Native capacity sampling runs in the blocking pool. Wait for the
        // registry update rather than briefly charging an internal copy.
        let entry = entry
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?;
        match entry.as_ref() {
            Some(Entry::Open { volume, disk }) => (volume.upgrade(), disk.upgrade()),
            _ => (None, None),
        }
    } else {
        (None, None)
    };
    if let Some(volume) = volume {
        let (bytes, _) = volume.disk.retained_backing(directory)?;
        return Ok(bytes.saturating_add(volume.task_pending.bytes()));
    }
    if let Some(disk) = disk {
        return disk.retained_backing(directory).map(|(bytes, _)| bytes);
    }
    allocated(directory)
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
    try_open_with_intent(directory, open, true)
}

fn try_open_with_intent(
    directory: &Path,
    open: impl FnOnce(&Path) -> Result<Arc<Volume>>,
    yield_background: bool,
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
            // A background consolidation may retain the journal after its
            // Volume is gone. Ask it to yield; its exclusive lock then drains
            // and the reopened volume consolidates the same generations.
            if let Some(disk) = disk.upgrade() {
                if yield_background {
                    disk.yield_consolidation();
                }
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
    let assignment = Arc::new(if unassigned {
        Assignment::prepared()
    } else {
        Assignment::new(&context)?
    });
    let disk = Arc::new(LazyDisk::open(&root)?);
    if unassigned && disk.has_base_blocks()? {
        return Err(Error::conflict(
            "Unassigned disk must remain entirely journal-backed.",
        ));
    }
    let policy = super::policy::decode_policy(context["policy"].clone()).unwrap_or_default();
    policy.validate().map_err(Error::bad)?;
    disk.memory_budget(policy.memory_cache_mi_b as usize * 1024 * 1024)?;
    let volume = Arc::new(Volume {
        disk,
        assignment,
        stop,
        directory: directory.to_owned(),
        maintenance: tokio::runtime::Handle::try_current().ok(),
        policy: RwLock::new(policy),
        task_limit: AtomicU64::new(u64::MAX),
        task_usage: Mutex::new(None),
        task_pending: super::cache::PendingWrites::default(),
        task_waiting: AtomicU64::new(0),
        flush_budget_failed: AtomicBool::new(false),
        active_since: AtomicI64::new(0),
        active_attempt: Mutex::new(String::new()),
        pressure: AtomicBool::new(false),
        space_pressure: AtomicBool::new(false),
        fault: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        transition: Mutex::new(None),
    });
    volume.health()?;
    monitor_consolidation(&volume, CONSOLIDATE_JOURNAL_BYTES);
    Ok(volume)
}

/// Consolidate a mounted journal once it retains this much since its base.
const CONSOLIDATE_JOURNAL_BYTES: u64 = 1024 * 1024 * 1024;
const CONSOLIDATION_CHECK: std::time::Duration = std::time::Duration::from_secs(5);
/// A refused consolidation (usually for disk space) is retried less often.
const CONSOLIDATION_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// Consolidations hold only their journal, outside the bounded open pool.
static CONSOLIDATIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// While a volume is open, fold sealed generations into its local base and
/// seal a crash-consistent boundary once the journal exceeds its bound, as
/// a periodic capture did. The guest keeps running; the task ends with the volume.
fn monitor_consolidation(volume: &Arc<Volume>, journal_threshold: u64) {
    let Some(runtime) = volume.maintenance.as_ref() else {
        return;
    };
    if volume.consolidates()
        && volume.disk.sealed_pending()
        && volume
            .disk
            .completed()
            .is_ok_and(|completed| completed.is_some())
    {
        runtime.spawn(consolidate(volume.disk.clone()));
    }
    let volume = Arc::downgrade(volume);
    runtime.spawn(async move {
        let mut wait = CONSOLIDATION_CHECK;
        loop {
            tokio::time::sleep(wait).await;
            wait = CONSOLIDATION_CHECK;
            let Some(volume) = volume.upgrade() else {
                return;
            };
            if volume.stop.is_cancelled() {
                return;
            }
            if !volume.consolidates() {
                continue;
            }
            // Leave journal headroom before a small task reaches its allowance.
            let threshold = journal_threshold
                .min(volume.task_limit.load(Ordering::Acquire) / 4)
                .max(4096);
            if !volume.disk.sealed_pending()
                && (volume.disk.journal_bytes() >= threshold
                    || (volume.disk.journal_bytes() > 0
                        && (volume.pressure.load(Ordering::Acquire)
                            || volume.space_pressure.load(Ordering::Acquire))))
                && let Err(error) = volume.seal().await
            {
                tracing::warn!(target: "cairn_performance", operation = "journal_consolidation", phase = "seal", message = %error.message);
                wait = CONSOLIDATION_RETRY;
                continue;
            }
            if !volume.disk.sealed_pending() {
                continue;
            }
            let disk = volume.disk.clone();
            drop(volume);
            if !consolidate(disk).await {
                wait = CONSOLIDATION_RETRY;
            }
        }
    });
}

/// Drain sealed work without retaining the volume. Contention and transient
/// failures keep the disk scheduled after execution releases its ownership.
async fn consolidate(disk: Arc<LazyDisk>) -> bool {
    let stop = disk.consolidation_stop();
    let _background = tokio::select! {
        () = stop.cancelled() => return true,
        guard = disk.background_consolidation() => guard,
    };
    // Completion may seal a newer generation while reconstruction is running.
    // Retain the disk and drain that demand even after its Volume is released.
    while disk.sealed_pending() {
        let permit = tokio::select! {
            () = stop.cancelled() => return true,
            permit = async {
                if ISOLATED_NATIVE.get() { return Ok(None); }
                CONSOLIDATIONS.acquire().await.map(Some)
            } => match permit {
                Ok(permit) => permit,
                Err(_) => return false,
            }
        };
        let identity = disk.performance_identity();
        let worker = disk.clone();
        let result = tokio::task::spawn_blocking(move || worker.consolidate()).await;
        drop(permit);
        let retry = match result {
            Ok(Ok(_)) => std::time::Duration::from_millis(20),
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => return true,
            Ok(Err(error)) => {
                tracing::warn!(target: "cairn_performance", operation = "journal_consolidation", id = identity, kind = ?error.kind(), message = %error);
                CONSOLIDATION_RETRY
            }
            Err(error) => {
                tracing::warn!(target: "cairn_performance", operation = "journal_consolidation", id = identity, message = %error);
                CONSOLIDATION_RETRY
            }
        };
        if disk.sealed_pending() {
            tokio::select! {
                () = stop.cancelled() => return true,
                () = tokio::time::sleep(retry) => {}
            }
        }
    }
    true
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
    // A background consolidation yields after its current batch; an owner does not.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let path = directory.to_owned();
        let replacement = blocking(move || {
            let entry = entry(&path)?;
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
                Some(Entry::Open { volume, .. }) if volume.strong_count() > 0 => {
                    return Err(Error::conflict("Disk is still in use; retry restoration."));
                }
                Some(Entry::Open { disk, .. }) => {
                    if let Some(disk) = disk.upgrade() {
                        disk.yield_consolidation();
                        return Ok(None);
                    }
                }
                None => {}
            }
            *entry = Some(Entry::Replacing);
            Ok(Some(Replacement { directory: path }))
        })
        .await?;
        if let Some(replacement) = replacement {
            return Ok(replacement);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::conflict("Disk is still in use; retry restoration."));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

thread_local! {
    // Native physical workers already have a bounded per-disk scheduler. A
    // kernel-blocked call there must not consume the VM/shared metadata pool.
    static ISOLATED_NATIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn isolated_native_io() {
    ISOLATED_NATIVE.set(true);
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    static IO: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);
    if ISOLATED_NATIVE.get() {
        // This runtime's blocking pool belongs solely to the physical worker.
        // Leave its event loop live so consolidation can relieve write pressure.
        return tokio::task::spawn_blocking(operation)
            .await
            .map_err(Error::internal)?;
    }
    let permit = IO.acquire().await.map_err(Error::internal)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(Error::internal)?
}

fn try_inspect(directory: &Path) -> Result<Option<Arc<Volume>>> {
    try_open_with_intent(directory, open_volume, false)
}

/// Observation waits for detached maintenance without asking it to yield.
pub(crate) async fn load_for_inspection(directory: &Path) -> Result<Arc<Volume>> {
    load_with_opener(directory, std::time::Duration::from_secs(60), try_inspect).await
}

pub async fn load(directory: &Path) -> Result<Arc<Volume>> {
    load_with_wait(directory, std::time::Duration::from_secs(60)).await
}

async fn load_with_wait(directory: &Path, wait: std::time::Duration) -> Result<Arc<Volume>> {
    load_with_opener(directory, wait, try_open).await
}

async fn load_with_opener(
    directory: &Path,
    wait: std::time::Duration,
    opener: fn(&Path) -> Result<Option<Arc<Volume>>>,
) -> Result<Arc<Volume>> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let path = directory.to_owned();
        if let Some(volume) = blocking(move || opener(&path)).await? {
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
#[cfg(test)]
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
async fn create_source(
    directory: &Path,
    manifest: &Value,
    context: &Value,
    generation: i64,
    prepared: bool,
) -> Result<Arc<LazyDisk>> {
    let (directory, manifest, context) = (directory.to_owned(), manifest.clone(), context.clone());
    blocking(move || {
        if !prepared {
            Assignment::new(&context)?;
        }
        let disk = Arc::new(LazyDisk::create_at_generation(
            &directory, &manifest, generation,
        )?);
        disk.set_context(&context)?;
        disk.sync()?;
        Ok(disk)
    })
    .await
}

/// Retain the mounted disk's assignment and monotone local generation through resize.
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
            volume.assignment.assign(&context, || {
                volume.disk.set_context(&context)?;
                volume.disk.sync()?;
                Ok(())
            })?;
            *volume.policy.write().map_err(Error::internal)? = policy;
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

    /// The completed turn is folded into the local base in the background;
    /// releasing the execution never waits for that consolidation.
    pub async fn seal_completed(self: &Arc<Self>) -> Result<i64> {
        let disk = self.disk.clone();
        let generation = blocking(move || Ok(disk.seal_completed()?)).await?;
        if self.consolidates()
            && let Some(runtime) = self.maintenance.as_ref()
        {
            runtime.spawn(consolidate(self.disk.clone()));
        }
        Ok(generation)
    }

    /// The VMM and block backend have stopped. Fold their final acknowledged
    /// metadata writes without moving the last completed-turn boundary.
    #[cfg(test)]
    pub(crate) async fn seal_stopped(self: &Arc<Self>) -> Result<()> {
        if !self.consolidates() {
            return Ok(());
        }
        if self.disk.journal_bytes() > 0 {
            self.seal().await?;
        }
        if self.disk.sealed_pending()
            && let Some(runtime) = self.maintenance.as_ref()
        {
            runtime.spawn(consolidate(self.disk.clone()));
        }
        Ok(())
    }

    /// Prepared disks stay entirely journal-backed until a conversation claims them.
    fn consolidates(&self) -> bool {
        self.assignment
            .assigned()
            .is_ok_and(|grant| grant.is_some())
    }

    pub async fn inspect(self: &Arc<Self>) -> Result<Value> {
        let volume = self.clone();
        blocking(move || volume.status()).await
    }

    pub async fn needs_pause(self: &Arc<Self>) -> Result<bool> {
        Ok(self.control_reason()?.is_some())
    }

    fn control_reason(&self) -> Result<Option<&'static str>> {
        if self.fault.load(Ordering::SeqCst) {
            return Ok(Some("integrity"));
        }
        if self.pressure.load(Ordering::SeqCst) || self.space_pressure.load(Ordering::Acquire) {
            return Ok(Some("disk-space"));
        }
        // Local disks have no publication backlog; only integrity and space pause.
        Ok(None)
    }

    /// Records when the current VM or native attempt became active. A retried
    /// monitor transition keeps the original timestamp.
    pub(crate) fn activate_attempt(&self, attempt: &str) -> Result<()> {
        let mut active = self.active_attempt.lock().map_err(Error::internal)?;
        if active.as_str() != attempt {
            self.active_since
                .store(crate::config::now(), Ordering::Release);
            *active = attempt.to_owned();
        }
        Ok(())
    }

    /// CPU protection must not wait for filesystem accounting or disk I/O.
    pub(crate) fn cpu_pause_reason(&self) -> Result<Option<&'static str>> {
        self.control_reason()
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
                let policy = super::policy::from_persisted(&bytes).map_err(io::Error::other)?;
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
        self.disk
            .memory_budget(policy.memory_cache_mi_b as usize * 1024 * 1024)?;
        let (total, free) = super::policy::space(&self.directory)?;
        self.space_pressure
            .store(free <= policy.reserve(total), Ordering::Release);
        let reason = if self.fault.load(Ordering::SeqCst) {
            Some("integrity")
        } else if self.pressure.load(Ordering::SeqCst) {
            Some("disk-space")
        } else {
            // Without a dirty timestamp, the policy only reports disk space.
            policy.pause_reason(total, free, None, crate::config::now())
        };
        status["mode"] = "on-demand".into();
        status["waitingFor"] = reason.into();
        status["freeBytes"] = free.into();
        status["reserveBytes"] = policy.reserve(total).into();
        status["activeSince"] = self.active_since.load(Ordering::Acquire).into();
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
    /// Every workload is fenced. Fold journal history to release task headroom
    /// while accepted access drains and the filesystem flushes. If backing cannot
    /// admit its pending writes, fail them so syncfs can report a crash-prefix
    /// close instead of retaining ownership forever. No allowance is enlarged.
    pub(crate) async fn finish_fenced<T>(
        self: &Arc<Self>,
        flush: impl std::future::Future<Output = T>,
    ) -> T {
        tokio::pin!(flush);
        let mut timer = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            tokio::select! {
                biased;
                result = &mut flush => return result,
                _ = timer.tick(), if !self.flush_budget_failed.load(Ordering::Acquire) => {
                    let incoming = self.task_waiting.load(Ordering::Acquire);
                    if incoming == 0 {
                        continue;
                    }
                    if self.task_limit.load(Ordering::Acquire) == 0 {
                        self.flush_budget_failed.store(true, Ordering::Release);
                        continue;
                    }
                    let permit = if ISOLATED_NATIVE.get() {
                        None
                    } else {
                        let Ok(permit) = CONSOLIDATIONS.try_acquire() else { continue; };
                        Some(permit)
                    };
                    let volume = self.clone();
                    let result = tokio::task::spawn_blocking(move || -> io::Result<bool> {
                        {
                            let _admission = super::cache::admission()?;
                            if volume.task_has_room(incoming)? {
                                return Ok(false);
                            }
                        }
                        if volume.disk.journal_bytes() > 0 && !volume.disk.sealed_pending() {
                            volume.disk.seal()?;
                        }
                        volume.disk.consolidate()?;
                        let Some(_settled) = volume.disk.try_consolidation_guard()? else {
                            return Ok(false);
                        };
                        // Exclude new admissions from the final decision; an
                        // in-flight or newer journal prefix can still make room.
                        let _admission = super::cache::admission()?;
                        Ok(volume.task_pending.bytes() == 0
                            && volume.disk.journal_bytes() == 0
                            && !volume.task_has_room(incoming)?)
                    })
                    .await;
                    drop(permit);
                    if !matches!(result, Ok(Ok(false))) {
                        self.flush_budget_failed.store(true, Ordering::Release);
                    }
                }
            }
        }
    }

    /// Bound complete retained backing. Native supervision subtracts its managed
    /// /tmp from the issued allowance; zero remaining bytes blocks new writes.
    pub(crate) fn limit_backing(&self, bytes: u64) -> Result<()> {
        self.task_limit.store(bytes, Ordering::Release);
        *self.task_usage.lock().map_err(Error::internal)? = None;
        Ok(())
    }

    fn task_has_room(&self, incoming: u64) -> io::Result<bool> {
        let limit = self.task_limit.load(Ordering::Acquire);
        if limit == u64::MAX {
            return Ok(true);
        }
        let mut sample = self
            .task_usage
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?;
        let revision = self.disk.backing_revision();
        let refresh = sample.as_ref().is_none_or(|usage| {
            usage.sampled.elapsed() >= std::time::Duration::from_secs(1)
                || usage.revision != revision
        });
        if refresh {
            let (bytes, revision) = self.disk.retained_backing(&self.directory)?;
            *sample = Some(TaskUsage {
                sampled: std::time::Instant::now(),
                bytes: bytes.saturating_add(self.task_pending.bytes()),
                revision,
            });
        }
        Ok(sample.as_ref().unwrap().bytes.saturating_add(incoming) <= limit)
    }

    fn charge_task(&self, incoming: u64) -> io::Result<()> {
        if let Some(usage) = self
            .task_usage
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?
            .as_mut()
        {
            usage.bytes = usage.bytes.saturating_add(incoming);
        }
        Ok(())
    }

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
            if self.flush_budget_failed.load(Ordering::Acquire) {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            if self.stop.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "Disk stopped"));
            }
            let guard = super::cache::admission()?;
            let (total, free) = super::policy::space(&self.directory)?;
            let incoming = bytes as u64 * 4 + frames as u64 * 4096;
            let task_has_room = self.task_has_room(incoming)?;
            if task_has_room {
                self.task_waiting.store(0, Ordering::Release);
            } else if matches!(admission, WriteAdmission::Wait) {
                self.task_waiting.fetch_max(incoming, Ordering::Release);
            }
            if task_has_room
                && free
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
                crate::microvm::budget::charge_disk(state, incoming)?;
                let _task_pending = self.task_pending.reserve(incoming)?;
                self.charge_task(incoming)?;
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
#[path = "runtime_tests.rs"]
mod tests;
