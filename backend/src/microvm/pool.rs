//! Bounded execution slots, with one account-free VM prepared for a fresh chat.
mod ready;

use super::{
    budget::{self, Budget},
    host::Vm,
    plan::Plan,
};
use crate::{
    error::{Error, Result},
    skills::private_dir,
};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, OnceCell, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Default)]
struct Slots {
    occupied: Vec<bool>,
    ready: Option<Reservation>,
    preparing: Option<Preparing>,
    retry_after: Option<Instant>,
    warm_disabled: bool,
}

struct Preparing {
    slot: usize,
    stop: CancellationToken,
    done: watch::Receiver<bool>,
}

impl Slots {
    fn reserve(&mut self, capacity: usize) -> Option<usize> {
        if self.occupied.iter().filter(|used| **used).count() >= capacity {
            return None;
        }
        let index = match self.occupied.iter().position(|v| !v) {
            Some(index) => index,
            None if self.occupied.len() < capacity => {
                self.occupied.push(false);
                self.occupied.len() - 1
            }
            None => return None,
        };
        self.occupied[index] = true;
        Some(index + 1)
    }
}

/// Slot usage reported by the controller health endpoint.
#[derive(Debug, Serialize)]
pub struct Health {
    pub capacity: usize,
    pub occupied: usize,
    /// Included in occupied; this slot can be claimed by a compatible chat.
    pub ready: usize,
    pub preparing: bool,
}

/// Disk size used when a plan carries no placement resources.
const DEFAULT_DISK_MIB: u64 = 32768;

pub struct Reservation {
    pool: Arc<Pool>,
    released: bool,
    slot: usize,
    vm: Option<Vm>,
    owner: Option<crate::storage::environment::OwnerLease>,
    anonymous: Option<PathBuf>,
    prepared: Option<ready::Prepared>,
    completion: Option<watch::Sender<bool>>,
}

pub struct Pool {
    capacity: AtomicUsize,
    budget: Mutex<Option<Budget>>,
    cgroup: OnceCell<PathBuf>,
    hardware: OnceCell<serde_json::Value>,
    state: PathBuf,
    image: PathBuf,
    warm_enabled: bool,
    slots: Mutex<Slots>,
    admission: Mutex<()>,
    stop: CancellationToken,
    cleanup: TaskTracker,
    _block_cache: crate::storage::NodeBlockCache,
}

impl Pool {
    pub async fn new(
        state: PathBuf,
        image: PathBuf,
        stop: CancellationToken,
        capacity: usize,
    ) -> Result<Arc<Self>> {
        if capacity == 0 {
            return Err(Error::bad("VM concurrency must be a positive integer."));
        }
        // A cold control uses exactly the same image, budget and admission path.
        // This also lets operators disable speculative CPU/RAM on small nodes.
        let warm_enabled = match std::env::var("LEO_READY_VM_POOL").as_deref() {
            Ok("false") => false,
            Ok("true") | Err(std::env::VarError::NotPresent) => true,
            _ => return Err(Error::bad("LEO_READY_VM_POOL must be true or false.")),
        };
        // The controller lock and PID namespace fence all previous owners before this cleanup.
        let prepared = state.join("prepared");
        if prepared.exists() {
            tokio::fs::remove_dir_all(&prepared).await?;
        }
        crate::storage::environment::recover(&state).await?;
        // Retain the guest OS and toolchains, but never pin the chat adapter to
        // an obsolete image. Copy once per controller, then import into tmpfs.
        let entrypoint = state.join("entrypoint");
        private_dir(&entrypoint).await?;
        tokio::fs::copy(std::env::current_exe()?, entrypoint.join("leo")).await?;
        let block_cache = crate::storage::NodeBlockCache::new(&state)?;
        Ok(Arc::new(Self {
            capacity: AtomicUsize::new(capacity),
            budget: Mutex::new(None),
            cgroup: OnceCell::new(),
            hardware: OnceCell::new(),
            state,
            image,
            warm_enabled,
            slots: Mutex::new(Slots::default()),
            admission: Mutex::new(()),
            stop,
            cleanup: TaskTracker::new(),
            _block_cache: block_cache,
        }))
    }

    pub async fn initialize(self: &Arc<Self>, cgroup: PathBuf) -> Result<()> {
        // Detect the container's current envelope on every restart. A previous
        // shared leaf limit must not become the hardware ceiling after a resize.
        let detected = crate::nodes::connector::capabilities(&self.state)?;
        let limits = crate::nodes::Resources {
            cpu: detected["cpu"].as_u64().unwrap_or(1) as u32,
            memory_mi_b: detected["memoryMiB"].as_u64().unwrap_or(128),
            disk_mi_b: detected["diskMiB"].as_u64().unwrap_or(128),
        };
        self.hardware
            .set(detected)
            .map_err(|_| Error::conflict("Controller already initialized."))?;
        self.cgroup
            .set(cgroup)
            .map_err(|_| Error::conflict("Controller already initialized."))?;
        let persisted = self.state.join(budget::FILE);
        let mut initial: Budget = if persisted.exists() {
            serde_json::from_slice(&tokio::fs::read(persisted).await?)?
        } else {
            Budget {
                slots: self.capacity.load(Ordering::SeqCst),
                limits: limits.clone(),
            }
        };
        // The parent cgroup may have shrunk while the controller was stopped.
        initial.limits.cpu = initial.limits.cpu.min(limits.cpu);
        initial.limits.memory_mi_b = initial.limits.memory_mi_b.min(limits.memory_mi_b);
        self.configure(initial).await
    }

    pub fn hardware(&self) -> Option<&serde_json::Value> {
        self.hardware.get()
    }

    pub async fn budget(&self) -> Option<Budget> {
        self.budget.lock().await.clone()
    }

    pub async fn configure(self: &Arc<Self>, next: Budget) -> Result<()> {
        next.validate()?;
        let _admission = self.admission.lock().await;
        let hardware = self
            .hardware
            .get()
            .ok_or_else(|| Error::unavailable("Controller budgets are not initialized."))?;
        if u64::from(next.limits.cpu) > hardware["cpu"].as_u64().unwrap_or(0)
            || next.limits.memory_mi_b > hardware["memoryMiB"].as_u64().unwrap_or(0)
        {
            return Err(Error::bad("Shared budgets exceed controller capacity."));
        }
        if self.budget().await.as_ref() == Some(&next) {
            return Ok(());
        }
        // Release speculative RAM before applying a smaller cgroup envelope.
        // Health and stop never take the admission lock or await VM teardown.
        self.retire_idle().await;
        let _slots = self.slots.lock().await;
        let mut current = self.budget.lock().await;
        let cgroup = self.cgroup.get().unwrap();
        next.apply(cgroup)?;
        let write = crate::skills::atomic_write(
            &self.state.join(budget::FILE),
            &serde_json::to_vec(&next)?,
        )
        .await;
        if let Err(error) = write {
            if let Some(previous) = current.as_ref() {
                previous.apply(cgroup)?;
            }
            return Err(error);
        }
        self.capacity.store(next.slots, Ordering::SeqCst);
        *current = Some(next);
        Ok(())
    }

    pub async fn usage(&self) -> Result<(serde_json::Value, Option<&'static str>)> {
        let Some(budget) = self.budget().await else {
            return Ok((serde_json::Value::Null, None));
        };
        // Moving the controller into its delegated leaf does not move existing
        // memory charges. Include those charges and health-check processes in
        // the container envelope instead of measuring only the leaf.
        let envelope = self
            .cgroup
            .get()
            .unwrap()
            .parent()
            .ok_or_else(|| Error::unavailable("Controller memory envelope is unavailable."))?;
        let memory = budget::memory_usage(envelope)? / 1_048_576;
        let disk = budget::disk_bytes(&self.state)?;
        let (total, free) = crate::storage::policy::space(&self.state)?;
        let policy = match std::fs::read(self.state.join("storage-policy.json")) {
            Ok(bytes) => serde_json::from_slice::<crate::storage::policy::Policy>(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::storage::policy::Policy::default()
            }
            Err(error) => return Err(error.into()),
        };
        let reserve = policy.reserve(total);
        let pressure = budget::pressure(memory, budget.limits.memory_mi_b, free, reserve);
        Ok((
            serde_json::json!({ "memoryMiB": memory, "diskMiB": disk / 1_048_576 }),
            pressure,
        ))
    }

    pub async fn monitor(self: Arc<Self>) {
        loop {
            tokio::select! {
                () = self.stop.cancelled() => {
                    self.retire_idle().await;
                    return;
                },
                () = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            }
            if let Err(error) = self.maintain_ready().await {
                tracing::warn!(message = %error.message, "Could not maintain prepared VM");
            }
            if let Ok((usage, _)) = self.usage().await
                && let Some(budget) = self.budget().await
                && usage["memoryMiB"].as_u64().unwrap_or(0) > budget.limits.memory_mi_b * 85 / 100
                && let Err(error) = budget::reclaim(&self.state).await
            {
                tracing::warn!(message = %error.message, "Could not reclaim shared VM memory");
            }
        }
    }

    pub async fn health(&self) -> Health {
        let slots = self.slots.lock().await;
        Health {
            capacity: self.capacity.load(Ordering::SeqCst),
            occupied: slots.occupied.iter().filter(|v| **v).count(),
            ready: usize::from(slots.ready.is_some()),
            preparing: slots.preparing.is_some(),
        }
    }

    pub async fn reserve(self: &Arc<Self>, _run_id: &str) -> Result<Reservation> {
        let _admission = self.admission.lock().await;
        if self.stop.is_cancelled() {
            return Err(Error::unavailable("VM controller is stopping."));
        }
        // A live request preempts background preparation, but never reuses its
        // physical slot until its VMM and backend have both stopped.
        self.cancel_preparing().await;
        let prepared = self.slots.lock().await.ready.take();
        if let Some(mut reservation) = prepared {
            if self.usage().await?.1.is_none() {
                return Ok(reservation);
            }
            reservation.finish().await;
        }
        if self.usage().await?.1.is_some() {
            return Err(Error::unavailable(
                "Shared node resources are under pressure.",
            ));
        }
        let slot = self
            .slots
            .lock()
            .await
            .reserve(self.capacity.load(Ordering::SeqCst))
            .ok_or_else(|| Error::unavailable("All VM slots are occupied."))?;
        Ok(Reservation {
            pool: self.clone(),
            released: false,
            slot,
            vm: None,
            owner: None,
            anonymous: None,
            prepared: None,
            completion: None,
        })
    }

    pub async fn drain(self: &Arc<Self>) {
        self.stop.cancel();
        self.retire_idle().await;
        self.cleanup.close();
        self.cleanup.wait().await;
    }

    async fn cancel_preparing(&self) {
        let mut done = {
            let slots = self.slots.lock().await;
            let Some(preparing) = &slots.preparing else {
                return;
            };
            preparing.stop.cancel();
            preparing.done.clone()
        };
        // Completion is sent by the reservation's final cleanup, including its
        // Drop path. Closing a worker task alone never frees a physical slot.
        let _ = done.wait_for(|finished| *finished).await;
    }

    async fn retire_idle(&self) {
        self.cancel_preparing().await;
        let ready = self.slots.lock().await.ready.take();
        if let Some(mut reservation) = ready {
            reservation.finish().await;
        }
    }

    async fn maintain_ready(self: &Arc<Self>) -> Result<()> {
        let _admission = self.admission.lock().await;
        if self.stop.is_cancelled() || !self.warm_enabled {
            return Ok(());
        }
        let Some(budget) = self.budget().await else {
            return Ok(());
        };
        let (usage, pressure) = self.usage().await?;
        let memory = usage["memoryMiB"].as_u64().unwrap_or(u64::MAX);
        let expired = self
            .slots
            .lock()
            .await
            .ready
            .as_ref()
            .is_some_and(|reservation| {
                !reservation
                    .prepared
                    .as_ref()
                    .unwrap()
                    .compatible(&budget, DEFAULT_DISK_MIB)
            });
        if expired || pressure.is_some() || memory > budget.limits.memory_mi_b * 75 / 100 {
            self.retire_idle().await;
            return Ok(());
        }
        if memory.saturating_add(ready::WARM_HEADROOM_MIB) > budget.limits.memory_mi_b {
            return Ok(());
        }
        let policy = match tokio::fs::read(self.state.join("storage-policy.json")).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::storage::policy::Policy::default()
            }
            Err(error) => return Err(error.into()),
        };
        let mut slots = self.slots.lock().await;
        if self.stop.is_cancelled()
            || slots.ready.is_some()
            || slots.preparing.is_some()
            || slots.warm_disabled
            || slots.retry_after.is_some_and(|at| Instant::now() < at)
        {
            return Ok(());
        }
        let Some(slot) = slots.reserve(self.capacity.load(Ordering::SeqCst)) else {
            return Ok(());
        };
        let stop = self.stop.child_token();
        let (completion, done) = watch::channel(false);
        slots.preparing = Some(Preparing {
            slot,
            stop: stop.clone(),
            done,
        });
        let reservation = Reservation {
            pool: self.clone(),
            released: false,
            slot,
            vm: None,
            owner: None,
            anonymous: Some(self.state.join("environments").join(crate::config::id())),
            prepared: None,
            completion: Some(completion),
        };
        self.cleanup
            .spawn(reservation.prepare(budget, policy, stop));
        Ok(())
    }
}

impl Reservation {
    async fn prepare(
        mut self,
        budget: Budget,
        policy: crate::storage::policy::Policy,
        stop: CancellationToken,
    ) {
        let directory = self.anonymous.as_ref().unwrap().clone();
        let resources = serde_json::json!({
            "cpu": budget.limits.cpu.min(32),
            "memoryMiB": budget.vm_memory_mib(),
            "diskMiB": DEFAULT_DISK_MIB,
        });
        let operation = async {
            crate::storage::bootstrap::prepare_unassigned(
                &directory,
                DEFAULT_DISK_MIB * 1_048_576,
                &policy,
                &stop,
            )
            .await?;
            self.vm = Some(
                Vm::boot(
                    &self.pool.state,
                    &self.pool.image,
                    directory,
                    self.slot,
                    &stop,
                    Some(&resources),
                )
                .await?,
            );
            self.vm
                .as_mut()
                .unwrap()
                .warm_codex(&self.pool.state, &stop)
                .await
        }
        .await;
        let pool = self.pool.clone();
        {
            let mut slots = pool.slots.lock().await;
            if matches!(operation, Ok(true)) && !stop.is_cancelled() && !pool.stop.is_cancelled() {
                self.prepared = Some(ready::Prepared {
                    budget,
                    created: Instant::now(),
                });
                let completion = self.completion.take().unwrap();
                slots.preparing = None;
                slots.ready = Some(self);
                let _ = completion.send(true);
                tracing::info!(target: "leo_performance", operation = "vm_pool", event = "ready");
                return;
            }
            if matches!(operation, Ok(false)) {
                slots.warm_disabled = true;
            }
            slots.retry_after = Some(Instant::now() + Duration::from_secs(10));
        }
        if let Err(error) = operation
            && !stop.is_cancelled()
        {
            tracing::warn!(message = %error.message, "Anonymous VM preparation failed");
        }
        self.finish().await;
    }

    pub async fn execute(
        mut self,
        mut plan: Plan,
        socket: Arc<OnceCell<PathBuf>>,
        stop: CancellationToken,
    ) -> Result<i32> {
        if stop.is_cancelled() {
            self.finish().await;
            return Ok(143);
        }
        let operation = async {
            let mut timing = crate::performance::Operation::new(
                "conversation_start",
                plan.run_id(),
                "prepare_disk",
            );
            if !plan.storage().is_object() {
                return Err(Error::conflict(
                    "S3-backed storage is required for VM execution.",
                ));
            }
            let budget = self.pool.budget().await;
            if let Some(budget) = &budget {
                plan.set_vm_limits(budget.limits.cpu.min(32), budget.vm_memory_mib());
            }
            let disk_mib = plan.as_value()["resources"]["diskMiB"]
                .as_u64()
                .unwrap_or(DEFAULT_DISK_MIB);
            let size = disk_mib * 1024 * 1024;
            let compatible = self.prepared.as_ref().is_some_and(|prepared| {
                budget
                    .as_ref()
                    .is_some_and(|budget| prepared.compatible(budget, disk_mib))
            }) && ready::eligible(&plan).await?
                && ready::fresh_disk(&self.pool.state, plan.run_id()).await?
                && self.vm.as_ref().unwrap().codex_ready().await;
            if self.prepared.is_some() && !compatible {
                self.retire_anonymous().await;
                self.prepared = None;
            }
            if compatible {
                let directory = self.anonymous.take().unwrap();
                let environment = directory.file_name().unwrap().to_str().unwrap();
                // Assignment can partly persist before returning an error.
                // From this point onward, never delete this physical directory.
                self.owner = Some(
                    crate::storage::environment::assign(
                        &self.pool.state,
                        environment,
                        plan.run_id(),
                    )
                    .await?,
                );
                timing.next("authorize_prepared_disk");
            } else {
                self.owner = Some(
                    crate::storage::environment::ownership(
                        &self.pool.state,
                        plan.run_id(),
                        "Conversation disk is in use.",
                    )
                    .await?,
                );
            }
            let disk = self.owner.as_ref().unwrap().directory.clone();
            if !compatible {
                crate::storage::bootstrap::prepare(&disk, size, plan.storage(), &stop).await?;
            }
            let volume = crate::storage::runtime::load(&disk).await?;
            if volume.source.authorization()?.is_none() {
                // A controller crash may leave ownership durable before its
                // authorization was attached. Bind before any user import.
                volume
                    .authorize(self.owner.as_ref().unwrap(), plan.storage())
                    .await?;
            }
            if !compatible {
                timing.next("boot_vm");
                self.vm = Some(
                    Vm::boot(
                        &self.pool.state,
                        &self.pool.image,
                        disk,
                        self.slot,
                        &stop,
                        plan.resources(),
                    )
                    .await?,
                );
            }
            tracing::info!(target: "leo_performance", operation = "vm_pool", event = "claim", run_id = crate::performance::identity(plan.run_id()), ready = compatible);
            let vm = self.vm.as_mut().unwrap();
            // Only this reservation initializes the attempt socket.
            let _ = socket.set(vm.socket.clone());
            timing.finish();
            vm.execute(&plan, &self.pool.state, stop.clone()).await
        };
        // Do not drop boot or cleanup futures on cancellation: their resource ownership must drain.
        let result = operation.await;
        // Captures share this attempt's lifetime. Release pending thaw retries
        // before shutdown removes the guest socket, including normal completion.
        stop.cancel();
        self.finish().await;
        result
    }

    async fn finish(&mut self) {
        self.retire_anonymous().await;
        self.owner.take();
        release_slot(&self.pool, self.slot, self.completion.take()).await;
        self.released = true;
    }

    async fn retire_anonymous(&mut self) {
        if let Some(mut vm) = self.vm.take() {
            vm.shutdown().await;
        }
        discard_anonymous(self.anonymous.take()).await;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let pool = self.pool.clone();
        let mut vm = self.vm.take();
        let owner = self.owner.take();
        let slot = self.slot;
        let anonymous = self.anonymous.take();
        let completion = self.completion.take();
        self.pool.cleanup.spawn(async move {
            if let Some(vm) = &mut vm {
                vm.shutdown().await;
            }
            drop(owner);
            discard_anonymous(anonymous).await;
            release_slot(&pool, slot, completion).await;
        });
    }
}

async fn discard_anonymous(directory: Option<PathBuf>) {
    let Some(directory) = directory else {
        return;
    };
    // Shutdown reaps the VMM and backend first. Fence any remaining inspector
    // before deleting its journal, and remove this ephemeral registry entry.
    let _replacement = match crate::storage::runtime::replacement(&directory).await {
        Ok(guard) => guard,
        Err(error) => {
            tracing::warn!(message = %error.message, "Anonymous disk is still in use; retaining it");
            return;
        }
    };
    if let Err(error) = tokio::fs::remove_dir_all(directory).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "Could not remove retired anonymous disk");
    }
}

async fn release_slot(pool: &Pool, slot: usize, completion: Option<watch::Sender<bool>>) {
    let mut slots = pool.slots.lock().await;
    slots.occupied[slot - 1] = false;
    if completion.is_some()
        && slots
            .preparing
            .as_ref()
            .is_some_and(|preparing| preparing.slot == slot)
    {
        slots.preparing = None;
    }
    if let Some(completion) = completion {
        let _ = completion.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn anonymous_retirement_preserves_an_open_replacement() {
        let directory = tempfile::tempdir().unwrap().keep();
        let guard = crate::storage::runtime::replacement(&directory)
            .await
            .unwrap();
        discard_anonymous(Some(directory.clone())).await;
        assert!(directory.exists());
        drop(guard);
        discard_anonymous(Some(directory.clone())).await;
        assert!(!directory.exists());
    }

    #[tokio::test]
    async fn live_admission_waits_for_preparation_drop_cleanup_without_blocking_health() {
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            1,
        )
        .await
        .unwrap();
        let mut preparing = pool.reserve("anonymous").await.unwrap();
        let stop = CancellationToken::new();
        let (completion, done) = watch::channel(false);
        preparing.completion = Some(completion);
        pool.slots.lock().await.preparing = Some(Preparing {
            slot: preparing.slot,
            stop: stop.clone(),
            done,
        });
        let teardown = Arc::new(tokio::sync::Notify::new());
        let after_cancel = teardown.clone();
        let cancelled = stop.clone();
        let worker = tokio::spawn(async move {
            cancelled.cancelled().await;
            after_cancel.notified().await;
            drop(preparing);
        });
        let controller = pool.clone();
        let admission = tokio::spawn(async move { controller.reserve("live").await });
        tokio::time::timeout(Duration::from_secs(1), stop.cancelled())
            .await
            .unwrap();
        let health = tokio::time::timeout(Duration::from_millis(100), pool.health())
            .await
            .unwrap();
        assert_eq!(health.occupied, 1);
        assert!(health.preparing);
        assert!(
            !admission.is_finished(),
            "Physical slot remains owned until teardown completes"
        );
        teardown.notify_one();
        worker.await.unwrap();
        let reservation = tokio::time::timeout(Duration::from_secs(1), admission)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(reservation.slot, 1);
        assert!(!pool.health().await.preparing);
        drop(reservation);
        pool.drain().await;
        assert_eq!(pool.health().await.occupied, 0);
    }

    #[test]
    fn reducing_slots_keeps_occupied_work_and_blocks_reusing_a_free_index() {
        let mut slots = Slots::default();
        assert_eq!(slots.reserve(4), Some(1));
        assert_eq!(slots.reserve(4), Some(2));
        assert_eq!(slots.reserve(4), Some(3));
        slots.occupied[0] = false;
        assert_eq!(slots.reserve(2), None);
        assert!(slots.occupied[1] && slots.occupied[2]);
        slots.occupied[1] = false;
        assert_eq!(slots.reserve(2), Some(1));
        assert_eq!(slots.reserve(2), None);
    }

    #[tokio::test]
    async fn restart_ignores_stale_capabilities_and_clamps_persisted_budgets() {
        let root = tempfile::tempdir().unwrap();
        let cgroup = root.path().join("cgroup");
        std::fs::create_dir(&cgroup).unwrap();
        std::fs::write(cgroup.join("memory.current"), "0").unwrap();
        std::fs::write(cgroup.join("memory.max"), "max").unwrap();
        std::fs::write(cgroup.join("cpu.max"), "max 100000").unwrap();
        std::fs::write(
            root.path().join("node-capabilities.json"),
            b"{} garbage from a previous host",
        )
        .unwrap();
        let desired = Budget {
            slots: 12,
            limits: crate::nodes::Resources {
                cpu: 4096,
                memory_mi_b: 1_073_741_824,
                disk_mi_b: 32768,
            },
        };
        std::fs::write(
            root.path().join(budget::FILE),
            serde_json::to_vec(&desired).unwrap(),
        )
        .unwrap();
        let pool = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            4,
        )
        .await
        .unwrap();
        pool.initialize(cgroup.clone()).await.unwrap();
        let hardware = pool.hardware().unwrap();
        let applied = pool.budget().await.unwrap();
        assert_eq!(
            u64::from(applied.limits.cpu),
            hardware["cpu"].as_u64().unwrap()
        );
        assert_eq!(
            applied.limits.memory_mi_b,
            hardware["memoryMiB"].as_u64().unwrap()
        );
        assert_eq!(applied.slots, 12);
        let mut smaller = applied.clone();
        smaller.limits.cpu = 1;
        smaller.limits.memory_mi_b = 640;
        pool.configure(smaller.clone()).await.unwrap();
        drop(pool);
        let restarted = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            4,
        )
        .await
        .unwrap();
        restarted.initialize(cgroup).await.unwrap();
        assert_eq!(restarted.budget().await.unwrap(), smaller);
        assert_eq!(
            restarted.hardware().unwrap()["memoryMiB"],
            applied.limits.memory_mi_b
        );
        restarted.configure(applied).await.unwrap();
    }

    #[tokio::test]
    async fn memory_pressure_includes_charges_outside_the_delegated_leaf() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let envelope = root.path().join("envelope");
        let cgroup = envelope.join("leo-shared");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&cgroup).unwrap();
        std::fs::write(
            envelope.join("memory.current"),
            (1024 * 1_048_576u64).to_string(),
        )
        .unwrap();
        std::fs::write(envelope.join("memory.stat"), "inactive_file 536870912\n").unwrap();
        std::fs::write(
            cgroup.join("memory.current"),
            (128 * 1_048_576u64).to_string(),
        )
        .unwrap();
        std::fs::write(cgroup.join("memory.max"), "max").unwrap();
        std::fs::write(cgroup.join("cpu.max"), "max 100000").unwrap();
        let pool = Pool::new(state, root.path().into(), CancellationToken::new(), 4)
            .await
            .unwrap();
        pool.initialize(cgroup).await.unwrap();
        assert_eq!(pool.usage().await.unwrap().0["memoryMiB"], 512);
    }

    #[tokio::test]
    async fn finished_execution_cancels_captures_before_releasing_its_slot() {
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            1,
        )
        .await
        .unwrap();
        let stop = CancellationToken::new();
        let reservation = pool.reserve("run").await.unwrap();

        // Rejecting an invalid plan reaches the same completion/cleanup path as
        // a guest returning its exit status, without requiring KVM in this test.
        let plan = Plan::new(serde_json::json!({
            "runId": "run"
        }));
        let result = reservation
            .execute(plan, Arc::default(), stop.clone())
            .await;
        assert!(result.is_err());
        assert!(
            stop.is_cancelled(),
            "captures must not retry a finished VM's deleted socket"
        );
        assert_eq!(pool.health().await.occupied, 0);
    }

    #[tokio::test]
    async fn abandoned_reservations_release_capacity_and_shutdown_rejects_work() {
        let root = tempfile::tempdir().unwrap();
        let stop = CancellationToken::new();
        let pool = Pool::new(root.path().into(), root.path().into(), stop.clone(), 12)
            .await
            .unwrap();
        let mut reservations = Vec::new();
        assert_eq!(pool.health().await.capacity, 12);
        for _ in 0..12 {
            reservations.push(pool.reserve("run").await.unwrap());
        }
        assert_eq!(pool.health().await.occupied, 12);
        assert!(pool.reserve("run").await.is_err());
        drop(reservations.pop());
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.health().await.occupied == 12 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        reservations.push(pool.reserve("run").await.unwrap());
        assert_eq!(pool.health().await.occupied, 12);
        stop.cancel();
        assert!(pool.reserve("run").await.is_err());
        drop(reservations);
        pool.drain().await;
        assert_eq!(pool.health().await.occupied, 0);
    }

    #[tokio::test]
    async fn large_capacity_allocates_slots_on_demand_without_truncation() {
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            usize::MAX,
        )
        .await
        .unwrap();
        assert!(pool.slots.lock().await.occupied.is_empty());
        let mut reservations = Vec::new();
        for slot in 1..=300 {
            let reservation = pool.reserve("run").await.unwrap();
            assert_eq!(reservation.slot, slot);
            reservations.push(reservation);
        }
        drop(reservations);
        pool.drain().await;
        assert_eq!(pool.health().await.occupied, 0);
        assert!(
            Pool::new(
                root.path().into(),
                root.path().into(),
                CancellationToken::new(),
                0
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn restart_removes_only_unassigned_disks() {
        let root = tempfile::tempdir().unwrap();
        for directory in ["prepared/orphan", "disks/conversation"] {
            tokio::fs::create_dir_all(root.path().join(directory))
                .await
                .unwrap();
            tokio::fs::write(root.path().join(directory).join("sentinel"), b"saved")
                .await
                .unwrap();
        }
        let pool = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            4,
        )
        .await
        .unwrap();
        assert!(!root.path().join("prepared/orphan").exists());
        assert_eq!(
            tokio::fs::read(root.path().join("disks/conversation/sentinel"))
                .await
                .unwrap(),
            b"saved"
        );
        assert_eq!(pool.health().await.occupied, 0);
    }
}
