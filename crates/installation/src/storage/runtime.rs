//! Controller-owned mounted disks. One live journal per conversation directory.
use super::{Disk, DiskWrite, LazyDisk, policy::Policy, source::Source};
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
    let source = Arc::new(
        if unassigned {
            Source::prepared(tokio::runtime::Handle::current(), stop.clone())
        } else {
            Source::new(&context, tokio::runtime::Handle::current(), stop.clone())?
        }
        .persistent(&root)?,
    );
    let disk = Arc::new(LazyDisk::open(&root, source.clone())?);
    if unassigned && disk.has_base_blocks()? {
        return Err(Error::conflict(
            "Unassigned disk must remain entirely journal-backed.",
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
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
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
            permit = CONSOLIDATIONS.acquire() => match permit {
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

/// A durable fence excludes detached acknowledgements even after transport loss
/// or controller restart. It never needs the journal publication lock.
pub async fn fence_publication(state: &Path, run: &str, publication: &str) -> Result<()> {
    crate::validation::uuid(run)?;
    crate::validation::uuid(publication)?;
    let state = state.to_owned();
    let publication = publication.to_owned();
    tokio::task::spawn_blocking(move || super::reports::fences(&state)?.fence(&publication))
        .await
        .map_err(Error::internal)?
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

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    static IO: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);
    let admission = crate::performance::publication::start(
        crate::performance::publication::Stage::NodeIoAdmission,
    );
    let permit = IO.acquire().await.map_err(Error::internal)?;
    admission.finish(0);
    crate::performance::publication::blocking(move || {
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
        let waiting = crate::performance::publication::start(
            crate::performance::publication::Stage::NodeOpenRetry,
        );
        tokio::time::sleep(remaining.min(std::time::Duration::from_millis(20))).await;
        waiting.finish(0);
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
        let source = Arc::new(
            if prepared {
                Source::prepared(tokio::runtime::Handle::current(), CancellationToken::new())
            } else {
                Source::new(
                    &context,
                    tokio::runtime::Handle::current(),
                    CancellationToken::new(),
                )?
            }
            .persistent(&directory)?,
        );
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
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(consolidate(self.disk.clone()));
        }
        Ok(generation)
    }

    /// The VMM and block backend have stopped. Fold their final acknowledged
    /// metadata writes without moving the last completed-turn boundary.
    pub(crate) async fn seal_stopped(self: &Arc<Self>) -> Result<()> {
        if !self.consolidates() {
            return Ok(());
        }
        if self.disk.journal_bytes() > 0 {
            self.seal().await?;
        }
        if self.disk.sealed_pending() {
            tokio::spawn(consolidate(self.disk.clone()));
        }
        Ok(())
    }

    /// Prepared disks stay entirely journal-backed until a conversation claims them.
    fn consolidates(&self) -> bool {
        self.source
            .authorization()
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
        if self.source.waiting() {
            return Ok(Some("storage-unavailable"));
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

    /// CPU protection must not wait for filesystem accounting or remote reads.
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
        } else if self.source.waiting() {
            Some("storage-unavailable")
        } else {
            // Without a dirty timestamp, the policy only reports disk space.
            policy.pause_reason(total, free, None, crate::config::now())
        };
        status["mode"] = "on-demand".into();
        status["grantId"] = self.source.authorization()?.into();
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
                    let Ok(permit) = CONSOLIDATIONS.try_acquire() else {
                        continue;
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
        assert!(
            create(&physical.join("lazy"), &remote, &authorized)
                .await
                .is_err()
        );
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
        let before = crate::config::now();
        volume.activate_attempt("native-first").unwrap();
        let first = volume.active_since.load(Ordering::Acquire);
        assert!(first >= before);
        volume.activate_attempt("native-first").unwrap();
        assert_eq!(volume.active_since.load(Ordering::Acquire), first);
        // Retrying the same attempt keeps its original activation.
        volume.active_since.store(1, Ordering::Release);
        volume.activate_attempt("native-first").unwrap();
        assert_eq!(volume.active_since.load(Ordering::Acquire), 1);
        volume.activate_attempt("native-resumed").unwrap();
        assert!(volume.active_since.load(Ordering::Acquire) >= first);
        volume.fault.store(true, Ordering::Release);
        assert_eq!(volume.cpu_pause_reason().unwrap(), Some("integrity"));
        volume.fault.store(false, Ordering::Release);
        volume.space_pressure.store(true, Ordering::Release);
        assert_eq!(volume.cpu_pause_reason().unwrap(), Some("disk-space"));
    }

    #[tokio::test]
    async fn local_journal_backlog_never_pauses_work_but_disk_faults_still_do() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        volume.disk.write_at(0, b"unconsolidated work").unwrap();
        assert_eq!(volume.control_reason().unwrap(), None);
        let status = volume.inspect().await.unwrap();
        assert!(status["dirtyBytes"].as_u64().unwrap() > 0);
        assert_eq!(status["waitingFor"], Value::Null);
        for removed in [
            "backupDegraded",
            "backupUrgent",
            "backupSeconds",
            "maxDirtySeconds",
        ] {
            assert!(status.get(removed).is_none(), "{removed} is still reported");
        }
        volume.pressure.store(true, Ordering::SeqCst);
        assert_eq!(volume.control_reason().unwrap(), Some("disk-space"));
        volume.pressure.store(false, Ordering::SeqCst);
        volume.fault.store(true, Ordering::SeqCst);
        assert_eq!(volume.control_reason().unwrap(), Some("integrity"));
        volume.fault.store(false, Ordering::SeqCst);
        assert_eq!(volume.control_reason().unwrap(), None);
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
    async fn corrupt_local_base_pauses_io_and_preserves_newer_journal_writes() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        volume.disk.write_at(0, &[7; 4096]).unwrap();
        volume.disk.seal().unwrap();
        volume.disk.consolidate().unwrap();
        volume.disk.write_at(0, b"local work").unwrap();
        let cache = volume.directory.join("lazy/cache");
        let block = std::fs::read_dir(&cache)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::write(block, [8; 4096]).unwrap();
        assert_eq!(
            volume.read_at(1024, &mut [0; 8]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(volume.status().unwrap()["waitingFor"], "integrity");
        assert!(volume.needs_pause().await.unwrap());
        let mut saved = [0; 10];
        volume.read_at(0, &mut saved).unwrap();
        assert_eq!(&saved, b"local work");
        assert_eq!(volume.status().unwrap()["waitingFor"], "integrity");
    }

    #[tokio::test]
    async fn running_journal_pressure_consolidates_without_guest_control_or_turn_completion() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        let mut initial = [0; 4096];
        initial[..7].copy_from_slice(b"running");
        volume.disk.write_at(0, &initial).unwrap();
        monitor_consolidation(&volume, 4096);
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while volume.disk.accounting().unwrap()["dirtyBytes"] != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!volume.paused());
        assert!(!volume.stop.is_cancelled());
        assert!(volume.disk.completed().unwrap().is_none());
        volume.write_at(7, b"new").unwrap();
        let mut bytes = [0; 10];
        volume.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"runningnew");
    }

    #[tokio::test]
    async fn small_task_consolidates_journal_history_before_its_allowance_is_full() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        volume.limit_backing(2 * 1024 * 1024).unwrap();
        let bytes = [7; 4096];
        let writes: Vec<_> = (0..128)
            .map(|_| DiskWrite {
                offset: 0,
                bytes: &bytes,
            })
            .collect();
        volume.disk.write_batch(&writes).unwrap();
        assert!(volume.task_has_room(4096).unwrap());
        assert!(!volume.pressure.load(Ordering::Acquire));
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while volume.disk.accounting().unwrap()["dirtyBytes"] != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!volume.pressure.load(Ordering::Acquire));
        assert!(!volume.paused());
        assert_eq!(volume.disk.completed().unwrap(), None);
    }

    #[tokio::test]
    async fn quota_admission_tolerates_segments_reclaimed_during_accounting() {
        use std::sync::atomic::AtomicBool;
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        volume.limit_backing(1024 * 1024 * 1024).unwrap();
        let directory = volume.directory.join("lazy");
        for index in 0..128 {
            std::fs::write(
                directory.join(format!("reclaimed-{index}.segment")),
                b"retired",
            )
            .unwrap();
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let reclaimer = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                for index in 0..128 {
                    let path = directory.join(format!("reclaimed-{index}.segment"));
                    std::fs::remove_file(&path).unwrap();
                    std::fs::write(path, b"retired").unwrap();
                }
            }
        });
        let mut errors = Vec::new();
        for index in 0..2048 {
            *volume.task_usage.lock().unwrap() = None;
            if let Err(error) = volume.task_has_room(4096) {
                errors.push(error.kind());
            }
            if index % 128 == 0 {
                *volume.task_usage.lock().unwrap() = None;
                if let Err(error) = volume.write_at(0, b"x") {
                    errors.push(error.kind());
                }
            }
        }
        stopped.store(true, Ordering::Release);
        reclaimer.join().unwrap();
        assert!(
            errors.is_empty(),
            "reclamation failed healthy admission: {errors:?}"
        );
        assert!(!volume.fault.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn consolidation_scratch_is_excluded_for_vm_and_native_but_work_and_tmp_stay_capped() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        let block = 4 * 1024 * 1024u64;
        let manifest = json!({
            "version": 1, "size": 2 * block, "blockSize": block,
            "blocks": (0..2).map(|index| json!({ "offset": index * block, "size": block, "hash": null })).collect::<Vec<_>>()
        });
        let disk = create(
            &directory.join("lazy"),
            &manifest,
            &json!({
                "master": "http://127.0.0.1:1/", "grant": "bounded-scratch",
                "policy": { "reserveMiB": 64, "reservePercent": 1 }
            }),
        )
        .await
        .unwrap();
        disk.write_at(0, &vec![7; block as usize]).unwrap();
        disk.write_at(block, &vec![9; block as usize]).unwrap();
        drop(disk);
        let volume = load(&directory).await.unwrap();
        volume.disk.seal().unwrap();
        let old_segments: Vec<_> = std::fs::read_dir(directory.join("lazy"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "segment")
            })
            .collect();
        assert!(!old_segments.is_empty());
        let before = allocated(&directory).unwrap();
        let limit = before + 2 * 1024 * 1024;
        volume.limit_backing(limit).unwrap();
        let copied = Arc::new(AtomicBool::new(false));
        let observed = copied.clone();
        let writer = volume.clone();
        let state = root.path().to_owned();
        volume.disk.on_consolidation_copy_ready(Box::new(move || {
            let physical = allocated(&writer.directory).unwrap();
            assert!(
                physical > limit,
                "fixture must cross the task limit with its copy"
            );
            assert!(
                crate::microvm::budget::disk_bytes(&state).unwrap() >= physical,
                "the shared node budget includes the complete internal copy"
            );
            let retained = retained_backing_bytes(&writer.directory).unwrap();
            assert!(retained <= before + 64 * 1024);
            assert!(physical - retained <= 2 * block);
            *writer.task_usage.lock().unwrap() = None;
            assert!(
                writer.task_has_room(4096).unwrap(),
                "VM admission counts only retained data"
            );
            let temporary = writer.directory.join("tmp");
            std::fs::create_dir_all(&temporary).unwrap();
            let user_file = temporary.join("working-files");
            std::fs::write(&user_file, vec![3; 4 * 1024 * 1024]).unwrap();
            assert!(
                retained_backing_bytes(&writer.directory).unwrap() > limit,
                "native accounting includes /tmp"
            );
            *writer.task_usage.lock().unwrap() = None;
            assert!(
                !writer.task_has_room(4096).unwrap(),
                "copy credit must not hide user files"
            );
            std::fs::remove_file(user_file).unwrap();
            *writer.task_usage.lock().unwrap() = None;
            writer.write_at(0, b"next").unwrap();
            observed.store(true, Ordering::Release);
        }));
        let committed = Arc::new(AtomicBool::new(false));
        let observed = committed.clone();
        let writer = volume.clone();
        volume.disk.on_consolidation_committed(Box::new(move || {
            // Simulate reclamation before the maintenance future returns. A
            // newly created file at an old name must never inherit its credit.
            for path in old_segments {
                let held = path.with_extension("retired-test");
                std::fs::rename(&path, &held).unwrap();
                std::fs::write(&path, vec![1; 4096]).unwrap();
                std::fs::remove_file(held).unwrap();
            }
            assert_eq!(
                retained_backing_bytes(&writer.directory).unwrap(),
                allocated(&writer.directory).unwrap()
            );
            *writer.task_usage.lock().unwrap() = None;
            assert!(
                !writer.task_has_room(4 * 1024 * 1024).unwrap(),
                "the new base counts immediately"
            );
            observed.store(true, Ordering::Release);
        }));
        volume.disk.consolidate().unwrap().unwrap();
        assert!(copied.load(Ordering::Acquire));
        assert!(committed.load(Ordering::Acquire));
        assert_eq!(
            retained_backing_bytes(&directory).unwrap(),
            allocated(&directory).unwrap()
        );
        let mut bytes = [0; 4];
        volume.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"next");
        assert!(
            volume.disk.accounting().unwrap()["dirtyBytes"]
                .as_u64()
                .unwrap()
                > 0
        );
    }

    #[tokio::test]
    async fn exhausted_backing_allowance_blocks_writes_until_capacity_returns() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        volume.limit_backing(0).unwrap();
        let writing = volume.clone();
        let mut writer = tokio::task::spawn_blocking(move || writing.write_at(0, b"retained"));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let blocked = !writer.is_finished();
        volume.limit_backing(64 * 1024 * 1024).unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), &mut writer).await;
        if result.is_err() {
            volume.stop.cancel();
            let _ = writer.await;
        }
        assert!(
            blocked,
            "managed /tmp can consume all remaining task headroom"
        );
        result.unwrap().unwrap().unwrap();
        let mut bytes = [0; 8];
        volume.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"retained");
    }

    #[tokio::test]
    async fn task_backing_pressure_counts_journal_history_and_consolidates_before_resuming_writes()
    {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("disks/conversation");
        let block = 4 * 1024 * 1024u64;
        let manifest = json!({
            "version": 1, "size": 4 * block, "blockSize": block,
            "blocks": (0..4).map(|index| json!({ "offset": index * block, "size": block, "hash": null })).collect::<Vec<_>>()
        });
        let disk = create(
            &directory.join("lazy"),
            &manifest,
            &json!({
                "master": "http://127.0.0.1:1/", "grant": "local-task",
                "policy": { "reserveMiB": 64, "reservePercent": 1 }
            }),
        )
        .await
        .unwrap();
        for value in 1..=3 {
            disk.write_at(0, &vec![value; block as usize]).unwrap();
        }
        drop(disk);
        let volume = load(&directory).await.unwrap();
        volume.limit_backing(8 * 1024 * 1024).unwrap();
        let writer = volume.clone();
        let writer = tokio::task::spawn_blocking(move || writer.write_at(0, b"new"));
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !volume.pressure.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!writer.is_finished());
        assert!(volume.disk.journal_bytes() < CONSOLIDATE_JOURNAL_BYTES);
        tokio::time::timeout(std::time::Duration::from_secs(15), writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(allocated(&directory).unwrap() < 8 * 1024 * 1024);
        let mut bytes = [0; 3];
        volume.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"new");
    }

    fn hold_consolidation(
        disk: &LazyDisk,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered, wait) = tokio::sync::oneshot::channel();
        let (release, held) = std::sync::mpsc::channel();
        disk.on_consolidation_start(Box::new(move || {
            let _ = entered.send(());
            held.recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
        }));
        (wait, release)
    }

    #[tokio::test]
    async fn stopped_vm_consolidates_shutdown_writes_after_its_completed_seal() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        let disk = volume.disk.clone();
        volume.write_at(0, b"completed").unwrap();
        let completed = volume.seal_completed().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while disk.accounting().unwrap()["dirtyBytes"] != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Guest shutdown can write superblock/journal metadata after the
        // completed response was sealed, even though no user turn remains.
        volume.write_at(16, b"shutdown").unwrap();
        volume.stop.cancel();
        volume.seal_stopped().await.unwrap();
        drop(volume);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while disk.accounting().unwrap()["dirtyBytes"] != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("stopped VM journal tail was never consolidated");
        assert_eq!(disk.completed().unwrap().unwrap()["generation"], completed);
        assert!(
            disk.accounting().unwrap()["published"]["generation"]
                .as_i64()
                .unwrap()
                > completed
        );
        let mut bytes = [0; 8];
        disk.read_at(16, &mut bytes).unwrap();
        assert_eq!(&bytes, b"shutdown");
    }

    #[tokio::test]
    async fn stopped_prepared_volume_remains_journal_backed() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("environments").join(crate::config::id());
        drop(
            create_prepared(
                &directory.join("lazy"),
                &json!({
                    "version": 1, "size": 4096, "blockSize": 4194304,
                    "blocks": [{ "offset": 0, "size": 4096, "hash": null }]
                }),
                &json!({
                    "unassigned": true,
                    "policy": { "reserveMiB": 64, "reservePercent": 1 }
                }),
            )
            .await
            .unwrap(),
        );
        let volume = load(&directory).await.unwrap();
        volume.write_at(0, b"seed").unwrap();
        volume.stop.cancel();
        volume.seal_stopped().await.unwrap();
        assert!(!volume.disk.sealed_pending());
        assert!(!volume.disk.has_base_blocks().unwrap());
        assert!(volume.disk.completed().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_completed_seal_during_consolidation_is_drained_after_volume_release() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        let disk = volume.disk.clone();
        disk.write_at(0, b"first").unwrap();
        disk.seal().unwrap();
        let (entered, release) = hold_consolidation(&disk);
        let active = tokio::spawn(consolidate(disk.clone()));
        entered.await.unwrap();
        disk.write_at(0, b"completed").unwrap();
        let generation = volume.seal_completed().await.unwrap();
        drop(volume);
        let mut queued = tokio::spawn(consolidate(disk.clone()));
        let completed_early =
            tokio::time::timeout(std::time::Duration::from_millis(500), &mut queued)
                .await
                .is_ok();
        release.send(()).unwrap();
        active.await.unwrap();
        if !completed_early {
            queued.await.unwrap();
        }
        assert!(
            !completed_early,
            "a busy journal was incorrectly reported consolidated"
        );
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while disk.sealed_pending() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("completed generation was abandoned after contention");
        assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 0);
        assert!(
            disk.accounting().unwrap()["published"]["generation"]
                .as_i64()
                .unwrap()
                >= generation
        );
    }

    #[tokio::test]
    async fn status_inspection_never_cancels_detached_consolidation() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        let directory = volume.directory.clone();
        let disk = volume.disk.clone();
        disk.write_at(0, b"completed").unwrap();
        let (entered, release) = hold_consolidation(&disk);
        volume.seal_completed().await.unwrap();
        entered.await.unwrap();
        drop(volume);
        let inspected = try_inspect(&directory).unwrap();
        let cancelled = disk.consolidation_stop().is_cancelled();
        release.send(()).unwrap();
        assert!(
            inspected.is_none(),
            "inspection must wait for the detached owner"
        );
        assert!(
            !cancelled,
            "status observation cancelled local consolidation"
        );
        drop(disk);
        let volume = load_for_inspection(&directory).await.unwrap();
        assert_eq!(volume.inspect().await.unwrap()["dirtyBytes"], 0);
    }

    #[tokio::test]
    async fn completed_turn_consolidates_after_the_volume_is_released() {
        let root = tempfile::tempdir().unwrap();
        let (volume, _, _) = controller_fixture(root.path()).await;
        volume.disk.write_at(0, b"completed").unwrap();
        let disk = volume.disk.clone();
        let generation = volume.seal_completed().await.unwrap();
        drop(volume);
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while disk.sealed_pending() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(disk.accounting().unwrap()["dirtyBytes"], 0);
        assert_eq!(
            disk.accounting().unwrap()["published"]["generation"],
            generation
        );
        let mut saved = [0; 9];
        disk.read_at(0, &mut saved).unwrap();
        assert_eq!(&saved, b"completed");
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
        // This fixture tests the shared quota, not the production 10 GiB
        // physical reserve, which can exceed the free space on a test tmpfs.
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
            "policy": policy
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
