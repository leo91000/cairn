//! Bounded execution slots and an evictable pool of account-free prepared VMs.
mod ready;
mod retained;

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
    collections::VecDeque,
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
    ready: VecDeque<Reservation>,
    retained: VecDeque<retained::Retained>,
    preparing: Option<Preparing>,
    retry_after: Option<Instant>,
    warm_disabled: bool,
    warm_failures: u32,
    warm_circuit_open: bool,
}

struct Preparing {
    slot: usize,
    stop: CancellationToken,
    done: watch::Receiver<bool>,
}

const WARM_FAILURE_LIMIT: u32 = 6;
const WARM_RETRY_MAX: Duration = Duration::from_secs(160);
const WARM_CIRCUIT_COOLDOWN: Duration = Duration::from_secs(3600);

impl Slots {
    fn warming_allowed(&mut self, now: Instant) -> bool {
        if self.warm_disabled || self.retry_after.is_some_and(|at| now < at) {
            return false;
        }

        if self.warm_circuit_open {
            self.warm_failures = 0;
            self.warm_circuit_open = false;
            tracing::info!(target: "cairn_performance", operation = "vm_pool", event = "circuit_rearmed");
        }

        self.retry_after = None;
        true
    }

    fn warm_failure(&mut self, now: Instant) {
        self.warm_failures = self.warm_failures.saturating_add(1).min(WARM_FAILURE_LIMIT);
        self.warm_circuit_open = self.warm_failures >= WARM_FAILURE_LIMIT;

        let delay = if self.warm_circuit_open {
            WARM_CIRCUIT_COOLDOWN
        } else {
            Duration::from_secs(10 * (1 << (self.warm_failures - 1))).min(WARM_RETRY_MAX)
        };
        self.retry_after = Some(now + delay);

        tracing::warn!(
            target: "cairn_performance",
            operation = "vm_pool",
            event = if self.warm_circuit_open { "circuit_open" } else { "retry" },
            consecutive_errors = self.warm_failures,
            retry_seconds = delay.as_secs(),
            "Anonymous VM preparation suspended"
        );
    }

    fn ready_bytes(&self) -> Option<u64> {
        self.ready.iter().try_fold(0u64, |bytes, reservation| {
            bytes.checked_add(reservation.vm.as_ref()?.resident_bytes()?)
        })
    }

    fn available(&self, capacity: usize) -> bool {
        self.occupied.iter().filter(|used| **used).count() < capacity
    }

    fn reserve(&mut self, capacity: usize) -> Option<usize> {
        if !self.available(capacity) {
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
    pub ready_memory_mi_b: Option<u64>,
    pub preparing: bool,
    pub retained: usize,
    pub retained_memory_mi_b: Option<u64>,
}

/// Disk size used when a plan carries no placement resources.
const DEFAULT_DISK_MIB: u64 = 32768;
/// Longest wait for an interrupted attempt's VMM to release a conversation disk.
const DISK_RELEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

pub struct Reservation {
    pool: Arc<Pool>,
    released: bool,
    slot: usize,
    vm: Option<Vm>,
    owner: Option<crate::storage::environment::OwnerLease>,
    anonymous: Option<PathBuf>,
    prepared: Option<ready::Prepared>,
    completion: Option<watch::Sender<bool>>,
    resumed: bool,
    retained_budget: Option<Budget>,
    idle_control: Arc<Mutex<()>>,
    idle_stop: CancellationToken,
}

pub struct Pool {
    capacity: AtomicUsize,
    budget: Mutex<Option<Budget>>,
    cgroup: OnceCell<PathBuf>,
    hardware: OnceCell<serde_json::Value>,
    state: PathBuf,
    image: PathBuf,
    warm_enabled: bool,
    warm_capacity: usize,
    retention: Duration,
    slots: Mutex<Slots>,
    admission: Mutex<()>,
    stop: CancellationToken,
    cleanup: TaskTracker,
    _block_cache: crate::storage::NodeBlockCache,
    templates: super::vm::snapshots::Templates,
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
        let warm_enabled = match std::env::var("CAIRN_READY_VM_POOL").as_deref() {
            Ok("false") => false,
            Ok("true") | Err(std::env::VarError::NotPresent) => true,
            _ => return Err(Error::bad("CAIRN_READY_VM_POOL must be true or false.")),
        };
        let warm_capacity = match std::env::var("CAIRN_READY_VM_POOL_SIZE") {
            Ok(value) => ready::capacity(Some(&value))?,
            Err(std::env::VarError::NotPresent) => ready::capacity(None)?,
            Err(_) => return Err(Error::bad("Invalid VM pool size configuration.")),
        };
        // The controller lock and PID namespace fence all previous owners before this cleanup.
        let prepared = state.join("prepared");
        if prepared.exists() {
            tokio::fs::remove_dir_all(&prepared).await?;
        }
        crate::storage::environment::recover(&state).await?;
        if let Err(error) = super::images::collect(&state, &image).await {
            tracing::warn!(message = %error.message, "Runtime image collection skipped");
        }
        // Retain the guest OS and toolchains, but never pin the chat adapter to
        // an obsolete image. Copy once per controller, then import into tmpfs.
        let entrypoint = state.join("entrypoint");
        private_dir(&entrypoint).await?;
        tokio::fs::copy(std::env::current_exe()?, entrypoint.join("cairn")).await?;
        let block_cache = crate::storage::NodeBlockCache::new(&state)?;
        let templates = super::vm::snapshots::Templates::new(&state, &image).await?;
        Ok(Arc::new(Self {
            capacity: AtomicUsize::new(capacity),
            budget: Mutex::new(None),
            cgroup: OnceCell::new(),
            hardware: OnceCell::new(),
            state,
            image,
            warm_enabled,
            warm_capacity,
            retention: retained::lifetime()?,
            slots: Mutex::new(Slots::default()),
            admission: Mutex::new(()),
            stop,
            cleanup: TaskTracker::new(),
            _block_cache: block_cache,
            templates,
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
        self.retire_idle(false).await;
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
        let mut next_image_gc = Instant::now() + Duration::from_secs(600);
        loop {
            tokio::select! {
                () = self.stop.cancelled() => {
                    self.retire_idle(true).await;
                    return;
                },
                () = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            }

            if Instant::now() >= next_image_gc {
                next_image_gc = Instant::now() + Duration::from_secs(600);
                if let Err(error) = super::images::collect(&self.state, &self.image).await {
                    tracing::warn!(message = %error.message, "Runtime image collection skipped");
                }
            }

            if let Err(error) = self.maintain_retained().await {
                tracing::warn!(message = %error.message, "Could not maintain idle conversation VMs");
            }
            if let Err(error) = self.maintain_ready(Instant::now()).await {
                tracing::warn!(message = %error.message, "Could not maintain prepared VM");
            }
            if let Ok((usage, _)) = self.usage().await
                && let Some(budget) = self.budget().await
                && let Err(error) = budget::rebalance(
                    &self.state,
                    usage["memoryMiB"].as_u64().unwrap_or(0),
                    budget.limits.memory_mi_b,
                )
                .await
            {
                tracing::warn!(message = %error.message, "Could not rebalance shared VM memory");
            }
        }
    }

    pub async fn health(&self) -> Health {
        let slots = self.slots.lock().await;
        Health {
            capacity: self.capacity.load(Ordering::SeqCst),
            occupied: slots.occupied.iter().filter(|v| **v).count(),
            ready: slots.ready.len(),
            ready_memory_mi_b: slots.ready_bytes().map(|bytes| bytes / 1_048_576),
            preparing: slots.preparing.is_some(),
            retained: slots.retained.len(),
            retained_memory_mi_b: slots
                .retained
                .iter()
                .try_fold(0u64, |total, idle| total.checked_add(idle.bytes()?))
                .map(|bytes| bytes / 1_048_576),
        }
    }

    pub async fn reserve(self: &Arc<Self>, plan: &Plan) -> Result<Reservation> {
        let mut timing =
            crate::performance::Operation::new("vm_admission", plan.run_id(), "admission_lock");
        let _admission = self.admission.lock().await;
        if self.stop.is_cancelled() {
            return Err(Error::unavailable("VM controller is stopping."));
        }
        // A live request preempts background preparation, but never reuses its
        // physical slot until its VMM and backend have both stopped.
        timing.next("background_preemption");
        self.cancel_preparing().await;
        timing.next("reclaim_idle");
        self.make_room(false).await?;
        let retained = {
            let mut slots = self.slots.lock().await;
            let index = slots
                .retained
                .iter()
                .position(|idle| idle.run == plan.run_id());
            index.and_then(|index| slots.retained.remove(index))
        };
        if let Some(mut idle) = retained {
            if self
                .budget()
                .await
                .as_ref()
                .is_some_and(|budget| idle.compatible(plan, budget, Instant::now()))
                && self.usage().await?.1.is_none()
            {
                idle.reservation.resumed = true;
                idle.reservation.retained_budget = Some(idle.budget);
                timing.finish();
                return Ok(idle.reservation);
            }
            if idle.publication_pending() {
                self.slots.lock().await.retained.push_back(idle);
                return Err(Error::new(
                    425,
                    "Waiting for disk publication before replacing this VM.",
                ));
            }
            idle.reservation.finish().await;
        }
        let has_ready = !self.slots.lock().await.ready.is_empty();
        timing.next("claim_spare");
        let disk_mib = plan.as_value()["resources"]["diskMiB"]
            .as_u64()
            .unwrap_or(DEFAULT_DISK_MIB);
        let can_claim = has_ready
            && disk_mib == DEFAULT_DISK_MIB
            && ready::eligible(plan).await?
            && ready::fresh_disk(&self.state, plan.run_id()).await?;
        if can_claim {
            let prepared = self.slots.lock().await.ready.pop_front();
            if let Some(mut reservation) = prepared {
                if self.usage().await?.1.is_none() {
                    timing.finish();
                    return Ok(reservation);
                }
                reservation.finish().await;
            }
        }
        // Resumes and custom plans cannot use anonymous native state. Keep the
        // healthy spare when another slot is available, rather than making
        // these runs wait for its teardown and another background warmup.
        timing.next("reserve_slot");
        self.make_room(true).await?;
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
        timing.finish();
        Ok(Reservation {
            pool: self.clone(),
            released: false,
            slot,
            vm: None,
            owner: None,
            anonymous: None,
            prepared: None,
            completion: None,
            resumed: false,
            retained_budget: None,
            idle_control: Arc::default(),
            idle_stop: CancellationToken::new(),
        })
    }

    pub async fn drain(self: &Arc<Self>) {
        self.stop.cancel();
        self.retire_idle(true).await;
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

    async fn retire_idle(&self, shutdown: bool) {
        self.cancel_preparing().await;
        loop {
            let idle = {
                let mut slots = self.slots.lock().await;
                let index = slots
                    .retained
                    .iter()
                    .position(|idle| shutdown || !idle.publication_pending());
                index.and_then(|index| slots.retained.remove(index))
            };
            let Some(mut idle) = idle else { break };
            idle.reservation.finish().await;
        }
        loop {
            let ready = self.slots.lock().await.ready.pop_front();
            let Some(mut reservation) = ready else { break };
            reservation.finish().await;
        }
    }

    /// Active work preempts the oldest conversations first, then the anonymous
    /// spare. Keep other healthy retained guests when one eviction is enough.
    async fn make_room(&self, needs_slot: bool) -> Result<()> {
        loop {
            let pressure = self.usage().await?.1.is_some();
            let mut slots = self.slots.lock().await;
            if !pressure && (!needs_slot || slots.available(self.capacity.load(Ordering::SeqCst))) {
                return Ok(());
            }
            let index = slots
                .retained
                .iter()
                .position(|idle| !idle.publication_pending());
            let idle = index
                .and_then(|index| slots.retained.remove(index))
                .map(|idle| idle.reservation)
                .or_else(|| slots.ready.pop_front());
            drop(slots);
            let Some(mut reservation) = idle else {
                return Ok(());
            };
            reservation.finish().await;
        }
    }

    /// Deletion, movement and pruning must reap the physical VM before taking its disk lock.
    pub async fn evict_conversation(&self, run: &str) -> Result<()> {
        let _admission = self.admission.lock().await;
        let idle = {
            let mut slots = self.slots.lock().await;
            let index = slots.retained.iter().position(|idle| idle.run == run);
            if index.is_some_and(|index| slots.retained[index].publication_pending()) {
                return Err(Error::new(
                    425,
                    "Waiting for disk publication before eviction.",
                ));
            }
            index.and_then(|index| slots.retained.remove(index))
        };
        if let Some(mut idle) = idle {
            idle.reservation.finish().await;
        }
        Ok(())
    }

    pub async fn capture_retained(&self, run: &str) -> Result<Option<serde_json::Value>> {
        let _admission = self.admission.lock().await;
        let retained = self
            .slots
            .lock()
            .await
            .retained
            .iter()
            .find(|idle| idle.run == run)
            .and_then(|idle| {
                Some((
                    idle.reservation.owner.clone()?,
                    idle.reservation.idle_control.clone(),
                    idle.reservation.idle_stop.child_token(),
                ))
            });
        let Some((owner, control, stop)) = retained else {
            return Ok(None);
        };
        let capture = control
            .try_lock_owned()
            .map_err(|_| Error::conflict("A retained disk capture is already in progress."))?;
        drop(_admission);
        let snapshot =
            crate::nodes::checkpoint::capture_paused(&self.state, run, &owner, stop, capture)
                .await?;
        Ok(Some(snapshot))
    }

    async fn retention_budget(&self, expected: Option<&Budget>) -> Option<Budget> {
        let budget = self.budget().await?;
        if self.stop.is_cancelled() || expected != Some(&budget) {
            return None;
        }
        let (usage, pressure) = self.usage().await.ok()?;
        if pressure.is_some()
            || usage["memoryMiB"].as_u64().unwrap_or(u64::MAX)
                > budget.limits.memory_mi_b * 75 / 100
        {
            return None;
        }
        Some(budget)
    }

    async fn maintain_retained(&self) -> Result<()> {
        let _admission = self.admission.lock().await;
        let Some(budget) = self.budget().await else {
            return Ok(());
        };
        loop {
            let (usage, pressure) = self.usage().await?;
            let memory_mi_b = usage["memoryMiB"].as_u64().unwrap_or(u64::MAX);
            let (idle, reason, retained_bytes, retained_count) = {
                let mut slots = self.slots.lock().await;
                let bytes = slots
                    .retained
                    .iter()
                    .try_fold(0u64, |total, idle| total.checked_add(idle.bytes()?));
                let exited = slots
                    .retained
                    .iter_mut()
                    .position(|idle| idle.reservation.vm.as_mut().is_some_and(Vm::exited));
                let expired = slots
                    .retained
                    .front()
                    .is_some_and(|idle| Instant::now() >= idle.expires);
                let memory_pressure = memory_mi_b > budget.limits.memory_mi_b * 75 / 100;
                let under_pressure = pressure.is_some() || memory_pressure;
                if exited.is_none()
                    && !expired
                    && !under_pressure
                    && bytes.is_some_and(|bytes| {
                        retained::within_budget(slots.retained.len(), bytes, &budget)
                    })
                {
                    return Ok(());
                }
                let reason = if exited.is_some() {
                    "process_exited"
                } else if expired {
                    "expired"
                } else if pressure.is_some() {
                    "node_pressure"
                } else if memory_pressure {
                    "node_memory"
                } else if bytes.is_none() {
                    "memory_unavailable"
                } else {
                    "retained_budget"
                };
                let count = slots.retained.len();
                // An already-dead VMM cannot be retained. Drain its backend
                // and ownership, keeping the durable source journal intact so
                // this node can recover it without waiting for remote upload.
                let index = exited.or_else(|| {
                    slots
                        .retained
                        .iter()
                        .position(|idle| !idle.publication_pending())
                });
                (
                    index.and_then(|index| slots.retained.remove(index)),
                    reason,
                    bytes,
                    count,
                )
            };
            let Some(mut idle) = idle else { return Ok(()) };
            tracing::info!(target: "cairn_performance", operation = "vm_retention", event = "evicted", run_id = crate::performance::identity(&idle.run), reason, pressure = pressure.unwrap_or("none"), node_memory_mi_b = memory_mi_b, budget_memory_mi_b = budget.limits.memory_mi_b, retained_bytes, retained_count);
            idle.reservation.finish().await;
        }
    }

    async fn maintain_ready(self: &Arc<Self>, now: Instant) -> Result<()> {
        let _admission = self.admission.lock().await;
        if self.stop.is_cancelled() || !self.warm_enabled {
            return Ok(());
        }
        let Some(budget) = self.budget().await else {
            return Ok(());
        };
        let (usage, pressure) = self.usage().await?;
        let memory = usage["memoryMiB"].as_u64().unwrap_or(u64::MAX);
        let incompatible = self.slots.lock().await.ready.iter().any(|reservation| {
            !reservation
                .prepared
                .as_ref()
                .unwrap()
                .compatible(&budget, DEFAULT_DISK_MIB)
        });
        if incompatible || pressure.is_some() || memory > budget.limits.memory_mi_b * 75 / 100 {
            self.retire_idle(false).await;
            return Ok(());
        }
        if memory.saturating_add(ready::WARM_HEADROOM_MIB) > budget.limits.memory_mi_b {
            return Ok(());
        }
        // Recheck actual RAM after preparation too: admission never relies on
        // the estimate, and an enlarged pool can shrink without touching work.
        loop {
            let retire = {
                let mut slots = self.slots.lock().await;
                let bytes = slots.ready_bytes();
                if slots.ready.len() <= 1
                    || bytes.is_some_and(|bytes| bytes <= ready::memory_allowance(&budget))
                {
                    None
                } else {
                    slots.ready.pop_front()
                }
            };
            let Some(mut reservation) = retire else { break };
            reservation.finish().await;
        }
        let slots = self.slots.lock().await;
        let can_prepare = ready::can_prepare(slots.ready.len(), slots.ready_bytes(), &budget);
        drop(slots);
        if !can_prepare {
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
            || slots.ready.len() >= self.warm_capacity
            || slots.preparing.is_some()
            || !slots.warming_allowed(now)
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
            resumed: false,
            retained_budget: None,
            idle_control: Arc::default(),
            idle_stop: CancellationToken::new(),
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
            if let Some(vm) = self
                .pool
                .templates
                .prepare(&directory, self.slot, &stop, &resources, &policy)
                .await?
            {
                self.vm = Some(vm);
                return Ok(true);
            }
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
                slots.warm_failures = 0;
                slots.warm_circuit_open = false;
                slots.retry_after = None;
                self.prepared = Some(ready::Prepared { budget });
                let completion = self.completion.take().unwrap();
                slots.preparing = None;
                slots.ready.push_back(self);
                let _ = completion.send(true);
                tracing::info!(target: "cairn_performance", operation = "vm_pool", event = "ready");
                return;
            }
            if !stop.is_cancelled() && !pool.stop.is_cancelled() {
                if matches!(operation, Ok(false)) {
                    slots.warm_disabled = true;
                } else if operation.is_err() {
                    slots.warm_failure(Instant::now());
                }
            }
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
        control: Arc<Mutex<()>>,
        stop: CancellationToken,
    ) -> Result<i32> {
        if stop.is_cancelled() {
            self.finish().await;
            return Ok(143);
        }
        let execution_budget = self.pool.budget().await;
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
            let budget = execution_budget.clone();
            if let Some(budget) = &budget {
                plan.set_vm_limits(budget.limits.cpu.min(32), budget.vm_memory_mib());
            }
            let disk_mib = plan.as_value()["resources"]["diskMiB"]
                .as_u64()
                .unwrap_or(DEFAULT_DISK_MIB);
            let size = disk_mib * 1024 * 1024;
            if self.resumed && self.retained_budget != budget {
                self.idle_stop.cancel();
                let control = self.idle_control.clone();
                let _capture = control.lock().await;
                self.retire_anonymous().await;
                self.owner.take();
                self.resumed = false;
                self.retained_budget = None;
            }
            // A retained VM already owns this exact conversation's physical disk.
            // Admission has compared its immutable mounts, privilege policy and budget.
            if self.resumed {
                tracing::info!(target: "cairn_performance", operation = "vm_retention", event = "claimed", run_id = crate::performance::identity(plan.run_id()));
                let vm = self.vm.as_mut().unwrap();
                let _capture = self.idle_control.lock().await;
                vm.resume_idle().await?;
                let _ = socket.set(vm.socket.clone());
                timing.finish();
                return vm.execute(&plan, &self.pool.state, stop.clone()).await;
            }
            if self.prepared.is_none()
                && disk_mib == DEFAULT_DISK_MIB
                && let Some(budget) = &budget
                && ready::eligible(&plan).await?
                && ready::fresh_disk(&self.pool.state, plan.run_id()).await?
            {
                let directory = self
                    .pool
                    .state
                    .join("environments")
                    .join(crate::config::id());
                let policy = crate::storage::policy::Policy::for_node(&plan.storage()["policy"])?;
                self.anonymous = Some(directory.clone());
                if let Some(vm) = self
                    .pool
                    .templates
                    .restore_cached(
                        &directory,
                        self.slot,
                        &stop,
                        plan.resources().unwrap(),
                        &policy,
                    )
                    .await?
                {
                    self.vm = Some(vm);
                    self.prepared = Some(ready::Prepared {
                        budget: budget.clone(),
                    });
                } else {
                    self.anonymous = None;
                }
            }
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
                // A resumed attempt can arrive while the interrupted one's VMM
                // still holds this disk; wait for it instead of failing the run.
                self.owner = Some(
                    crate::storage::environment::ownership_after_release(
                        &self.pool.state,
                        plan.run_id(),
                        "Conversation disk is in use.",
                        DISK_RELEASE_WAIT,
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
            tracing::info!(target: "cairn_performance", operation = "vm_pool", event = "claim", run_id = crate::performance::identity(plan.run_id()), ready = compatible);
            let vm = self.vm.as_mut().unwrap();
            // Only this reservation initializes the attempt socket.
            let _ = socket.set(vm.socket.clone());
            timing.finish();
            vm.execute(&plan, &self.pool.state, stop.clone()).await
        };
        // Do not drop boot or cleanup futures on cancellation: their resource ownership must drain.
        let result = operation.await;
        if matches!(result, Ok(0)) {
            let volume =
                crate::storage::runtime::load(&self.owner.as_ref().unwrap().directory).await?;
            // Seal the synced response before releasing its execution lease.
            // The publisher can reconstruct this prefix without pausing a
            // following turn, including when optional VM retention is skipped.
            volume.seal_completed().await?;
        }
        // Retention is optional. A capture can wait for attempt cancellation
        // to settle an uncertain pause/thaw, so finalization must not wait for
        // its control lock indefinitely before signalling that cancellation.
        let _control = if matches!(result, Ok(0)) && !stop.is_cancelled() {
            tokio::select! {
                () = stop.cancelled() => None,
                result = tokio::time::timeout(Duration::from_millis(250), control.lock()) => result.ok(),
            }
        } else {
            None
        };
        // Success includes guest sync and closure of the per-attempt auth relay.
        // Only an acknowledged CPU pause may survive release of the attempt lease.
        let retain = if matches!(result, Ok(0))
            && _control.is_some()
            && !stop.is_cancelled()
            && !self.pool.stop.is_cancelled()
            && retained::eligible(&plan)
            && !self.pool.retention.is_zero()
            && self
                .pool
                .retention_budget(execution_budget.as_ref())
                .await
                .is_some()
        {
            match self.vm.as_mut().unwrap().suspend_idle().await {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(target: "cairn_performance", operation = "vm_retention", event = "reclamation_failed", run_id = crate::performance::identity(plan.run_id()), vm_id = self.vm.as_ref().unwrap().id(), message = %error.message);
                    false
                }
            }
        } else {
            false
        };
        if retain {
            let pool = self.pool.clone();
            let directory = &self.owner.as_ref().unwrap().directory;
            let volume = crate::storage::runtime::load(directory).await?;
            let _admission = pool.admission.lock().await;
            if let Some(budget) = pool.retention_budget(execution_budget.as_ref()).await
                && let Some(bytes) = self.vm.as_ref().and_then(Vm::resident_bytes)
                && retained::within_budget(1, bytes, &budget)
                && !pool.stop.is_cancelled()
                && !stop.is_cancelled()
            {
                loop {
                    let old = {
                        let mut slots = pool.slots.lock().await;
                        let total = slots
                            .retained
                            .iter()
                            .try_fold(bytes, |total, idle| total.checked_add(idle.bytes()?));
                        if total.is_some_and(|total| {
                            retained::within_budget(slots.retained.len() + 1, total, &budget)
                        }) {
                            None
                        } else {
                            let index = slots
                                .retained
                                .iter()
                                .position(|idle| !idle.publication_pending());
                            index.and_then(|index| slots.retained.remove(index))
                        }
                    };
                    let Some(mut old) = old else { break };
                    old.reservation.finish().await;
                }
                let fits = {
                    let slots = pool.slots.lock().await;
                    slots
                        .retained
                        .iter()
                        .try_fold(bytes, |total, idle| total.checked_add(idle.bytes()?))
                        .is_some_and(|total| {
                            retained::within_budget(slots.retained.len() + 1, total, &budget)
                        })
                };
                if !fits {
                    stop.cancel();
                    self.finish().await;
                    return result;
                }
                // Anonymous eligibility ends permanently on assignment.
                self.prepared = None;
                self.resumed = false;
                self.idle_stop = CancellationToken::new();
                let idle = retained::Retained {
                    run: plan.run_id().to_owned(),
                    budget,
                    key: retained::key(&plan),
                    expires: Instant::now() + pool.retention,
                    reservation: self,
                    volume: Some(volume),
                };
                pool.slots.lock().await.retained.push_back(idle);
                stop.cancel();
                return result;
            }
        }
        // Captures share this attempt's lifetime. Release pending thaw retries
        // before shutdown removes the guest socket, including normal completion.
        stop.cancel();
        self.finish().await;
        result
    }

    async fn finish(&mut self) {
        self.idle_stop.cancel();
        let control = self.idle_control.clone();
        let _capture = control.lock().await;
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
        self.idle_stop.cancel();
        let control = self.idle_control.clone();
        self.pool.cleanup.spawn(async move {
            let _capture = control.lock().await;
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

    fn cold_plan(run: &str) -> Plan {
        Plan::new(serde_json::json!({ "runId": run }))
    }

    #[tokio::test]
    async fn repeated_preparation_errors_back_off_then_open_the_circuit() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let cgroup = root.path().join("cgroup");
        std::fs::create_dir(&state).unwrap();
        std::fs::create_dir(&cgroup).unwrap();
        std::fs::write(root.path().join("memory.current"), "0").unwrap();
        std::fs::write(root.path().join("memory.stat"), "inactive_file 0\n").unwrap();
        std::fs::write(cgroup.join("memory.current"), "0").unwrap();
        std::fs::write(cgroup.join("memory.max"), "max").unwrap();
        std::fs::write(cgroup.join("cpu.max"), "max 100000").unwrap();
        let budget = Budget {
            slots: 1,
            limits: crate::nodes::Resources {
                cpu: 1,
                memory_mi_b: 2048,
                disk_mi_b: 32768,
            },
        };
        std::fs::write(
            state.join(budget::FILE),
            serde_json::to_vec(&budget).unwrap(),
        )
        .unwrap();
        // Invalid storage limits make each real background preparation fail
        // deterministically before formatting a disk or starting a guest.
        std::fs::write(state.join("storage-policy.json"), br#"{"reserveMiB":0}"#).unwrap();

        let pool = Pool::new(state, root.path().into(), CancellationToken::new(), 1)
            .await
            .unwrap();
        pool.initialize(cgroup).await.unwrap();
        let mut now = Instant::now();
        for seconds in [10, 20, 40, 80, 160, 3600] {
            pool.maintain_ready(now).await.unwrap();
            let mut done = pool
                .slots
                .lock()
                .await
                .preparing
                .as_ref()
                .expect("retry must start preparation")
                .done
                .clone();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !*done.borrow() {
                    done.changed().await.unwrap();
                }
            })
            .await
            .unwrap();

            let retry_after = pool.slots.lock().await.retry_after.unwrap();
            let remaining = retry_after.saturating_duration_since(Instant::now());
            assert!(
                remaining >= Duration::from_secs(seconds - 1),
                "expected {seconds}s, got {remaining:?}"
            );
            assert_eq!(pool.health().await.occupied, 0);
            pool.maintain_ready(retry_after - Duration::from_nanos(1))
                .await
                .unwrap();
            assert!(
                !pool.health().await.preparing,
                "no anonymous start before {seconds}s deadline"
            );
            assert_eq!(pool.health().await.occupied, 0);
            now = retry_after;
        }

        // Foreground admission remains available while the circuit is open.
        assert!(pool.slots.lock().await.warm_circuit_open);
        let mut live = pool.reserve(&cold_plan("live")).await.unwrap();
        live.finish().await;
        assert!(pool.slots.lock().await.warm_circuit_open);

        // Re-enter through the production gate at the cooldown deadline.
        pool.maintain_ready(now).await.unwrap();
        let mut done = pool
            .slots
            .lock()
            .await
            .preparing
            .as_ref()
            .unwrap()
            .done
            .clone();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !*done.borrow() {
                done.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let slots = pool.slots.lock().await;
        assert_eq!(slots.warm_failures, 1);
        assert!(!slots.warm_circuit_open);
        assert!(
            slots
                .retry_after
                .unwrap()
                .saturating_duration_since(Instant::now())
                >= Duration::from_secs(9)
        );
        drop(slots);
        pool.drain().await;
    }

    #[tokio::test]
    async fn admission_and_explicit_eviction_preserve_unacknowledged_journals() {
        use crate::storage::Disk;
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            3,
        )
        .await
        .unwrap();
        let run = crate::config::id();
        let owner = crate::storage::environment::ownership(root.path(), &run, "busy")
            .await
            .unwrap();
        let manifest = serde_json::json!({ "version": 1, "size": 4096, "blockSize": crate::nodes::snapshots::BLOCK, "blocks": [{ "offset": 0, "size": 4096, "hash": null }] });
        let source = serde_json::json!({ "master": "http://127.0.0.1:9/", "grant": "synthetic-pin", "policy": { "reserveMiB": 64, "reservePercent": 1 } });
        let disk =
            crate::storage::runtime::create(&owner.directory.join("lazy"), &manifest, &source)
                .await
                .unwrap();
        disk.write_at(0, b"durable but not published").unwrap();
        drop(disk);
        let volume = crate::storage::runtime::load(&owner.directory)
            .await
            .unwrap();
        let pinned = pool.reserve(&cold_plan(&run)).await.unwrap();
        let disposable = pool.reserve(&cold_plan("disposable")).await.unwrap();
        let ready = pool.reserve(&cold_plan("anonymous")).await.unwrap();
        let budget = Budget {
            slots: 3,
            limits: crate::nodes::Resources {
                cpu: 3,
                memory_mi_b: 8192,
                disk_mi_b: 32768,
            },
        };
        {
            let mut slots = pool.slots.lock().await;
            for (run, reservation, volume) in [
                (run.clone(), pinned, Some(volume.clone())),
                ("disposable".into(), disposable, None),
            ] {
                slots.retained.push_back(retained::Retained {
                    run,
                    reservation,
                    volume,
                    budget: budget.clone(),
                    key: serde_json::Value::Null,
                    expires: Instant::now() - Duration::from_secs(1),
                });
            }
            slots.ready.push_back(ready);
        }
        assert_eq!(pool.evict_conversation(&run).await.unwrap_err().status, 425);
        let first = pool.reserve(&cold_plan("first-active")).await.unwrap();
        assert_eq!(
            first.slot, 2,
            "Even expired retained VMs stay pinned until publication"
        );
        let second = pool.reserve(&cold_plan("second-active")).await.unwrap();
        assert_eq!(
            second.slot, 3,
            "Evict the anonymous pool before the unpublished conversation"
        );
        assert!(pool.reserve(&cold_plan("third-active")).await.is_err());
        let generation = volume.seal().await.unwrap();
        volume.disk.capture(generation).unwrap();
        volume
            .disk
            .commit_published(generation, &crate::config::id())
            .unwrap();
        pool.evict_conversation(&run).await.unwrap();
        assert!(pool.slots.lock().await.retained.is_empty());
        drop((first, second));
        pool.drain().await;
    }

    #[tokio::test]
    async fn active_admission_evicts_oldest_retained_before_the_ready_pool() {
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            4,
        )
        .await
        .unwrap();
        let oldest = pool.reserve(&cold_plan("oldest")).await.unwrap();
        let newest = pool.reserve(&cold_plan("newest")).await.unwrap();
        let ready = pool.reserve(&cold_plan("anonymous")).await.unwrap();
        let second_ready = pool.reserve(&cold_plan("second-anonymous")).await.unwrap();
        let budget = Budget {
            slots: 4,
            limits: crate::nodes::Resources {
                cpu: 3,
                memory_mi_b: 8192,
                disk_mi_b: 32768,
            },
        };
        let mut slots = pool.slots.lock().await;
        for (run, reservation) in [("oldest", oldest), ("newest", newest)] {
            slots.retained.push_back(retained::Retained {
                run: run.into(),
                budget: budget.clone(),
                key: serde_json::Value::Null,
                expires: Instant::now() + Duration::from_secs(180),
                reservation,
                volume: None,
            });
        }
        slots.ready.push_back(ready);
        slots.ready.push_back(second_ready);
        drop(slots);
        let active = pool.reserve(&cold_plan("new-active")).await.unwrap();
        assert_eq!(active.slot, 1, "Oldest retained slot is reclaimed first");
        let slots = pool.slots.lock().await;
        assert_eq!(slots.retained.len(), 1);
        assert_eq!(slots.retained.front().unwrap().run, "newest");
        assert!(
            !slots.ready.is_empty(),
            "Anonymous spare survives when one eviction suffices"
        );
        drop(slots);
        let second = pool.reserve(&cold_plan("second-active")).await.unwrap();
        assert_eq!(second.slot, 2);
        assert!(!pool.slots.lock().await.ready.is_empty());
        let third = pool.reserve(&cold_plan("third-active")).await.unwrap();
        assert_eq!(third.slot, 3);
        assert_eq!(pool.slots.lock().await.ready.len(), 1);
        let fourth = pool.reserve(&cold_plan("fourth-active")).await.unwrap();
        assert_eq!(fourth.slot, 4, "Anonymous spares are evicted oldest first");
        assert!(pool.slots.lock().await.ready.is_empty());
        assert!(pool.reserve(&cold_plan("fifth-active")).await.is_err());
        assert_eq!(
            pool.health().await.occupied,
            4,
            "Active guests are never evicted"
        );
        drop((active, second, third, fourth));
        pool.drain().await;
    }

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
        let mut preparing = pool.reserve(&cold_plan("anonymous")).await.unwrap();
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
        let admission = tokio::spawn(async move { controller.reserve(&cold_plan("live")).await });
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
    async fn cold_work_keeps_a_ready_spare_unless_it_needs_the_slot() {
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(
            root.path().into(),
            root.path().into(),
            CancellationToken::new(),
            2,
        )
        .await
        .unwrap();
        let mut spare = pool.reserve(&cold_plan("anonymous")).await.unwrap();
        spare.prepared = Some(ready::Prepared {
            budget: Budget {
                slots: 2,
                limits: crate::nodes::Resources {
                    cpu: 3,
                    memory_mi_b: 6144,
                    disk_mi_b: 32768,
                },
            },
        });
        pool.slots.lock().await.ready.push_back(spare);

        let resume = Plan::new(
            serde_json::json!({ "runId": "resumed", "chat": { "provider": "codex", "sessionId": "existing-thread" } }),
        );
        let resumed = pool.reserve(&resume).await.unwrap();
        assert_eq!(resumed.slot, 2);
        assert_eq!(pool.health().await.ready, 1);
        assert_eq!(pool.health().await.occupied, 2);

        // Active work wins when the anonymous slot is the only capacity left.
        let other = pool.reserve(&cold_plan("other")).await.unwrap();
        assert_eq!(other.slot, 1);
        assert_eq!(pool.health().await.ready, 0);
        assert_eq!(pool.health().await.occupied, 2);
        drop((resumed, other));
        pool.drain().await;
        assert_eq!(pool.health().await.occupied, 0);
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
        let cgroup = envelope.join("cairn-shared");
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
        let reservation = pool.reserve(&cold_plan("run")).await.unwrap();

        // Rejecting an invalid plan reaches the same completion/cleanup path as
        // a guest returning its exit status, without requiring KVM in this test.
        let plan = Plan::new(serde_json::json!({
            "runId": "run"
        }));
        let control = Arc::new(Mutex::new(()));
        let held_capture = control.lock().await;
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            reservation.execute(plan, Arc::default(), control.clone(), stop.clone()),
        )
        .await
        .unwrap();
        drop(held_capture);
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
            reservations.push(pool.reserve(&cold_plan("run")).await.unwrap());
        }
        assert_eq!(pool.health().await.occupied, 12);
        assert!(pool.reserve(&cold_plan("run")).await.is_err());
        drop(reservations.pop());
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.health().await.occupied == 12 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        reservations.push(pool.reserve(&cold_plan("run")).await.unwrap());
        assert_eq!(pool.health().await.occupied, 12);
        stop.cancel();
        assert!(pool.reserve(&cold_plan("run")).await.is_err());
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
            let reservation = pool.reserve(&cold_plan("run")).await.unwrap();
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
