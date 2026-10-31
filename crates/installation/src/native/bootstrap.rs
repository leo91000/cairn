//! Technical observations only. Nothing here authorizes launch or establishes
//! coding-engine progress, physical completion, or filesystem durability.
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::time::Instant;

const RETAINED: usize = 256;
const EXPORT_QUEUE: usize = 32;
const MAX_EXPORT_BYTES: usize = 8192;

struct Export {
    snapshot: Snapshot,
    dropped: Arc<AtomicU64>,
    forgotten: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    AdmissionWait,
    Plan,
    StorageAdmission,
    WorkspaceOwnership,
    Mount,
    IdentityProvision,
    FilesystemInitialize,
    TemporaryPrepare,
    IdentityReconcile,
    HomeRefresh,
    StagingOwnership,
    RuntimePrepare,
    PoolAccounting,
    WorkloadLaunch,
}

impl Phase {
    const ALL: [Self; 14] = [
        Self::AdmissionWait,
        Self::Plan,
        Self::StorageAdmission,
        Self::WorkspaceOwnership,
        Self::Mount,
        Self::IdentityProvision,
        Self::FilesystemInitialize,
        Self::TemporaryPrepare,
        Self::IdentityReconcile,
        Self::HomeRefresh,
        Self::StagingOwnership,
        Self::RuntimePrepare,
        Self::PoolAccounting,
        Self::WorkloadLaunch,
    ];
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Counter {
    MarkerMatch,
    NewTransition,
    ReplayTransition,
    EntriesSeen,
    EntriesChanged,
    BatchesPublished,
    EntriesApplied,
    PendingBytesWritten,
    PendingBytesRead,
    SyncfsCompleted,
    PendingRetired,
    DirectorySyncsCompleted,
}

impl Counter {
    const ALL: [Self; 12] = [
        Self::MarkerMatch,
        Self::NewTransition,
        Self::ReplayTransition,
        Self::EntriesSeen,
        Self::EntriesChanged,
        Self::BatchesPublished,
        Self::EntriesApplied,
        Self::PendingBytesWritten,
        Self::PendingBytesRead,
        Self::SyncfsCompleted,
        Self::PendingRetired,
        Self::DirectorySyncsCompleted,
    ];
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Cost {
    Scan,
    Capture,
    Apply,
    JournalPublication,
    PendingReadback,
    Syncfs,
    PendingRetire,
}

impl Cost {
    const ALL: [Self; 7] = [
        Self::Scan,
        Self::Capture,
        Self::Apply,
        Self::JournalPublication,
        Self::PendingReadback,
        Self::Syncfs,
        Self::PendingRetire,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Preparing,
    Submitted,
    Rejected,
    Incomplete,
    AlreadyRunning,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Snapshot {
    schema: u8,
    attempt_id: String,
    run_id: Option<String>,
    controller_runtime_id: String,
    controller_session_id: String,
    pinned_runtime_id: Option<String>,
    boot_id: Option<String>,
    invocation: u64,
    started_at: i64,
    observed_at: i64,
    status: Status,
    phase: Phase,
    elapsed_ms: u64,
    phases: Vec<PhaseCost>,
    counters: Vec<Count>,
    costs: Vec<OperationCost>,
    export_drops: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PhaseCost {
    phase: Phase,
    elapsed_ms: u64,
}

#[derive(Clone, Serialize)]
struct Count {
    counter: Counter,
    value: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationCost {
    operation: Cost,
    calls: u64,
    elapsed_micros: u64,
}

struct State {
    run: Option<String>,
    pinned_runtime: Option<String>,
    status: Status,
    started: Instant,
    started_at: i64,
    observed_at: i64,
    phase: Phase,
    phase_started: Instant,
    elapsed: Duration,
    phases: [Duration; Phase::ALL.len()],
    exported_phases: u64,
    last_export: Instant,
}

struct Observation {
    attempt: String,
    runtime: String,
    session: String,
    boot: Option<String>,
    invocation: u64,
    state: Mutex<State>,
    counters: [AtomicU64; Counter::ALL.len()],
    costs: [(AtomicU64, AtomicU64); Cost::ALL.len()],
    forgotten: Arc<AtomicBool>,
    preparing: AtomicBool,
    export_drops: Arc<AtomicU64>,
    exporter: Option<mpsc::SyncSender<Export>>,
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX.into()) as u64
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX.into()) as u64
}

fn add(counter: &AtomicU64, value: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |previous| {
        Some(previous.saturating_add(value))
    });
}

impl Observation {
    fn snapshot(&self) -> Snapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut phases = state.phases;
        let preparing = state.status == Status::Preparing;
        if preparing {
            phases[state.phase as usize] += state.phase_started.elapsed();
        }
        Snapshot {
            schema: 1,
            attempt_id: self.attempt.clone(),
            run_id: state.run.clone(),
            controller_runtime_id: self.runtime.clone(),
            controller_session_id: self.session.clone(),
            pinned_runtime_id: state.pinned_runtime.clone(),
            boot_id: self.boot.clone(),
            invocation: self.invocation,
            started_at: state.started_at,
            observed_at: if preparing {
                crate::config::now()
            } else {
                state.observed_at
            },
            status: state.status,
            phase: state.phase,
            elapsed_ms: millis(if preparing {
                state.started.elapsed()
            } else {
                state.elapsed
            }),
            phases: Phase::ALL
                .iter()
                .map(|phase| PhaseCost {
                    phase: *phase,
                    elapsed_ms: millis(phases[*phase as usize]),
                })
                .collect(),
            counters: Counter::ALL
                .iter()
                .map(|counter| Count {
                    counter: *counter,
                    value: self.counters[*counter as usize].load(Ordering::Relaxed),
                })
                .collect(),
            costs: Cost::ALL
                .iter()
                .map(|operation| OperationCost {
                    operation: *operation,
                    calls: self.costs[*operation as usize].0.load(Ordering::Relaxed),
                    elapsed_micros: self.costs[*operation as usize].1.load(Ordering::Relaxed),
                })
                .collect(),
            export_drops: self.export_drops.load(Ordering::Relaxed),
        }
    }

    fn export(&self) {
        if self.forgotten.load(Ordering::Relaxed) {
            return;
        }
        if let Some(exporter) = &self.exporter
            && exporter
                .try_send(Export {
                    snapshot: self.snapshot(),
                    dropped: self.export_drops.clone(),
                    forgotten: self.forgotten.clone(),
                })
                .is_err()
        {
            add(&self.export_drops, 1);
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Trace(Option<Arc<Observation>>);

impl Trace {
    pub(super) fn phase(&self, phase: Phase) {
        let Some(observation) = &self.0 else { return };
        if observation.forgotten.load(Ordering::Relaxed) {
            return;
        }
        let export = {
            let mut state = observation
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.status != Status::Preparing || state.phase == phase {
                return;
            }
            let previous = state.phase as usize;
            let elapsed = state.phase_started.elapsed();
            state.phases[previous] += elapsed;
            state.phase = phase;
            state.phase_started = Instant::now();
            let first = state.exported_phases & (1 << phase as usize) == 0;
            state.exported_phases |= 1 << phase as usize;
            let export = first || state.last_export.elapsed() >= Duration::from_secs(1);
            if export {
                state.last_export = Instant::now();
            }
            export
        };
        if export {
            observation.export();
        }
    }

    pub(super) fn add(&self, counter: Counter, value: u64) {
        if let Some(observation) = &self.0
            && !observation.forgotten.load(Ordering::Relaxed)
            && observation.preparing.load(Ordering::Relaxed)
        {
            add(&observation.counters[counter as usize], value);
        }
    }

    pub(super) fn operation(&self, cost: Cost) -> Operation {
        Operation {
            trace: self.clone(),
            cost,
            started: self.0.as_ref().map(|_| Instant::now()),
        }
    }

    pub(super) async fn measure<T>(&self, cost: Cost, future: impl Future<Output = T>) -> T {
        let _operation = self.operation(cost);
        future.await
    }
}

pub(super) struct Operation {
    trace: Trace,
    cost: Cost,
    started: Option<Instant>,
}

impl Drop for Operation {
    fn drop(&mut self) {
        if let Some(started) = self.started
            && let Some(observation) = &self.trace.0
            && !observation.forgotten.load(Ordering::Relaxed)
            && observation.preparing.load(Ordering::Relaxed)
        {
            add(&observation.costs[self.cost as usize].0, 1);
            add(
                &observation.costs[self.cost as usize].1,
                micros(started.elapsed()),
            );
        }
    }
}

#[derive(Default)]
pub(super) struct Scope(Trace);

impl Scope {
    pub(super) fn trace(&self) -> Trace {
        self.0.clone()
    }

    pub(super) fn context(&self, run: &str, runtime: Option<&str>) {
        if let Some(observation) = &self.0.0 {
            let mut state = observation
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.status != Status::Preparing || state.run.is_some() {
                return;
            }
            state.run = crate::validation::uuid(run).is_ok().then(|| run.to_owned());
            state.pinned_runtime = runtime
                .filter(|runtime| crate::nodes::valid_runtime(runtime))
                .map(str::to_owned);
        }
    }

    fn finish(&self, status: Status) {
        let Some(observation) = &self.0.0 else { return };
        let changed = {
            let mut state = observation
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.status != Status::Preparing {
                false
            } else {
                let phase = state.phase as usize;
                let elapsed = state.phase_started.elapsed();
                state.phases[phase] += elapsed;
                state.elapsed = state.started.elapsed();
                state.observed_at = crate::config::now();
                state.status = status;
                observation.preparing.store(false, Ordering::Relaxed);
                true
            }
        };
        if changed {
            observation.export();
        }
    }

    pub(super) fn submitted(&self) {
        self.finish(Status::Submitted);
    }

    pub(super) fn rejected(&self) {
        self.finish(Status::Rejected);
    }

    pub(super) fn already_running(&self) {
        self.finish(Status::AlreadyRunning);
    }

    pub(super) fn disabled(&self) -> bool {
        self.0.0.is_none()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        self.finish(Status::Incomplete);
    }
}

#[derive(Default)]
struct History {
    reports: HashMap<String, Arc<Observation>>,
    order: u64,
    omitted: u64,
}

#[derive(Clone)]
pub(super) struct Registry {
    history: Arc<Mutex<History>>,
    runtime: String,
    session: String,
    boot: Option<String>,
    exporter: Option<mpsc::SyncSender<Export>>,
}

impl Registry {
    pub(super) fn new(runtime: String, boot: Option<String>) -> Self {
        Self::with_sink(runtime, boot, |snapshot| {
            send_journal(
                &snapshot,
                std::path::Path::new("/run/systemd/journal/socket"),
            )
        })
    }

    fn with_sink(
        runtime: String,
        boot: Option<String>,
        sink: impl Fn(Snapshot) -> bool + Send + 'static,
    ) -> Self {
        let runtime = if crate::nodes::valid_runtime(&runtime) {
            runtime
        } else {
            "unavailable".into()
        };
        let boot = boot.filter(|boot| crate::validation::uuid(boot).is_ok());
        let (sender, receiver) = mpsc::sync_channel::<Export>(EXPORT_QUEUE);
        // A detached, bounded observer must not retain a Tokio blocking task:
        // runtime shutdown waits for those tasks, even after cancellation.
        let exporter = std::thread::Builder::new()
            .name("bootstrap-observer".into())
            .spawn(move || {
                while let Ok(export) = receiver.recv() {
                    if !export.forgotten.load(Ordering::Relaxed) && !sink(export.snapshot) {
                        add(&export.dropped, 1);
                    }
                }
            })
            .ok()
            .map(|_| sender);
        Self {
            history: Arc::default(),
            runtime,
            session: crate::config::id(),
            boot,
            exporter,
        }
    }

    pub(super) fn begin(&self, id: &str) -> Scope {
        if crate::validation::uuid(id).is_err() {
            return Scope::default();
        }
        let mut history = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = history.reports.get(id) {
            let status = previous
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .status;
            if matches!(status, Status::Preparing | Status::Submitted) {
                return Scope::default();
            }
        }
        // This session-wide sequence survives report eviction. Reusing an
        // attempt after an admission rejection must never reuse its log identity.
        let Some(invocation) = history.order.checked_add(1) else {
            history.omitted = history.omitted.saturating_add(1);
            return Scope::default();
        };
        if !history.reports.contains_key(id) && history.reports.len() == RETAINED {
            let oldest = history
                .reports
                .iter()
                .filter(|(_, observation)| {
                    observation
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .status
                        != Status::Preparing
                })
                .min_by_key(|(_, observation)| observation.invocation)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                history.reports.remove(&oldest);
            } else {
                history.omitted = history.omitted.saturating_add(1);
                return Scope::default();
            }
        }
        history.order = invocation;
        let observation = Arc::new(Observation {
            attempt: id.into(),
            runtime: self.runtime.clone(),
            session: self.session.clone(),
            boot: self.boot.clone(),
            invocation,
            state: Mutex::new(State {
                run: None,
                pinned_runtime: None,
                status: Status::Preparing,
                started: Instant::now(),
                started_at: crate::config::now(),
                observed_at: crate::config::now(),
                phase: Phase::AdmissionWait,
                phase_started: Instant::now(),
                elapsed: Duration::ZERO,
                phases: [Duration::ZERO; Phase::ALL.len()],
                exported_phases: 1,
                last_export: Instant::now(),
            }),
            counters: std::array::from_fn(|_| AtomicU64::new(0)),
            costs: std::array::from_fn(|_| (AtomicU64::new(0), AtomicU64::new(0))),
            forgotten: Arc::new(AtomicBool::new(false)),
            preparing: AtomicBool::new(true),
            export_drops: Arc::new(AtomicU64::new(0)),
            exporter: self.exporter.clone(),
        });
        history.reports.insert(id.into(), observation.clone());
        drop(history);
        observation.export();
        Scope(Trace(Some(observation)))
    }

    pub(super) fn get(&self, id: &str) -> Option<Snapshot> {
        let observation = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reports
            .get(id)
            .cloned()?;
        Some(observation.snapshot())
    }

    pub(super) fn health(&self) -> serde_json::Value {
        let Ok(history) = self.history.try_lock() else {
            return serde_json::json!({ "available": false });
        };
        let observations: Vec<_> = history.reports.values().cloned().collect();
        let omitted = history.omitted;
        drop(history);
        let mut phases = BTreeMap::new();
        let mut unavailable = 0u64;
        let mut export_drops = 0u64;
        for observation in &observations {
            let Ok(state) = observation.state.try_lock() else {
                unavailable += 1;
                continue;
            };
            if state.status == Status::Preparing {
                *phases.entry(state.phase).or_insert(0u64) += 1;
            }
            export_drops =
                export_drops.saturating_add(observation.export_drops.load(Ordering::Relaxed));
        }
        serde_json::json!({
            "available": true,
            "controllerSessionId": self.session,
            "retained": observations.len(),
            "preparingByPhase": phases,
            "omitted": omitted,
            "busy": unavailable,
            "exportDrops": export_drops,
            "exportConfigured": self.exporter.is_some(),
        })
    }

    pub(super) fn forget(&self, run: &str) {
        self.history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reports
            .retain(|_, observation| {
                let owned = observation
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .run
                    .as_deref()
                    == Some(run);
                if owned {
                    observation.forgotten.store(true, Ordering::Relaxed);
                }
                !owned
            });
    }
}

fn journal_entry(snapshot: &Snapshot) -> Option<Vec<u8>> {
    let json = serde_json::to_string(snapshot).ok()?;
    // Every value is either a fixed label, validated identity or JSON-escaped.
    // A datagram is one atomic entry; never split an oversize report or share
    // stdout's logging lock with an execution. No memfd/fallback/retry is needed.
    let entry = format!(
        "PRIORITY=6\nSYSLOG_IDENTIFIER=cairn-bootstrap\nCAIRN_OPERATION=native_bootstrap\nCAIRN_ATTEMPT_ID={}\nCAIRN_CONTROLLER_SESSION={}\nMESSAGE=Native bootstrap technical observation\nCAIRN_BOOTSTRAP_JSON={}\n",
        snapshot.attempt_id, snapshot.controller_session_id, json,
    );
    (entry.len() <= MAX_EXPORT_BYTES).then(|| entry.into_bytes())
}

fn send_journal(snapshot: &Snapshot, destination: &std::path::Path) -> bool {
    let Some(bytes) = journal_entry(snapshot) else {
        return false;
    };
    let Ok(socket) = std::os::unix::net::UnixDatagram::unbound() else {
        return false;
    };
    socket.set_nonblocking(true).is_ok() && socket.send_to(&bytes, destination).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        Registry {
            history: Arc::default(),
            runtime: "fixture-runtime".into(),
            session: crate::config::id(),
            boot: Some(crate::config::id()),
            exporter: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn snapshots_track_held_work_and_freeze_after_submission() {
        let registry = registry();
        let id = crate::config::id();
        let run = crate::config::id();
        let scope = registry.begin(&id);
        scope.context(&run, Some("pinned-tools"));
        let trace = scope.trace();
        trace.phase(Phase::IdentityReconcile);
        let operation = trace.operation(Cost::Capture);
        tokio::time::advance(Duration::from_secs(7)).await;
        trace.add(Counter::EntriesSeen, 4);
        drop(operation);
        let held = registry.get(&id).unwrap();
        assert_eq!(held.phase, Phase::IdentityReconcile);
        assert_eq!(held.elapsed_ms, 7000);
        assert_eq!(held.status, Status::Preparing);
        assert_eq!(held.run_id.as_deref(), Some(run.as_str()));
        assert_eq!(held.pinned_runtime_id.as_deref(), Some("pinned-tools"));
        assert_eq!(held.costs[Cost::Capture as usize].calls, 1);
        assert_eq!(held.costs[Cost::Capture as usize].elapsed_micros, 7_000_000);

        trace.phase(Phase::WorkloadLaunch);
        scope.submitted();
        let final_report = serde_json::to_value(registry.get(&id).unwrap()).unwrap();
        tokio::time::advance(Duration::from_secs(20)).await;
        trace.add(Counter::EntriesSeen, 100);
        trace.phase(Phase::Mount);
        drop(trace.operation(Cost::Scan));
        drop(scope);
        assert_eq!(
            serde_json::to_value(registry.get(&id).unwrap()).unwrap(),
            final_report
        );
        assert_eq!(final_report["status"], "submitted");
        assert!(!final_report.to_string().contains("providerStarted"));
    }

    #[test]
    fn closed_or_saturated_export_is_optional_and_never_waited_for() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut registry = registry();
        registry.exporter = Some(sender);
        let id = crate::config::id();
        let scope = registry.begin(&id);
        let (finished, completion) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            scope.trace().phase(Phase::Mount);
            scope.trace().phase(Phase::IdentityReconcile);
            scope.submitted();
            finished.send(()).unwrap();
        });
        let result = completion.recv_timeout(Duration::from_secs(1));
        drop(receiver);
        worker.join().unwrap();
        result.expect("a full observer channel must not block an execution");
        assert!(registry.get(&id).unwrap().export_drops >= 3);
        let another = registry.begin(&crate::config::id());
        another.submitted();
    }

    #[test]
    fn a_blocked_sink_does_not_retain_the_controller_runtime_on_shutdown() {
        let (entered, receiving) = mpsc::channel();
        let (release, blocked) = mpsc::sync_channel(1);
        let (done, finished) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let first = AtomicBool::new(true);
                let registry = Registry::with_sink("fixture-runtime".into(), None, move |_| {
                    if first.swap(false, Ordering::Relaxed) {
                        entered.send(()).unwrap();
                        let _ = blocked.recv_timeout(Duration::from_secs(5));
                    }
                    true
                });
                let scope = registry.begin(&crate::config::id());
                scope.trace().phase(Phase::Mount);
                scope.submitted();
            });
            drop(runtime);
            done.send(()).unwrap();
        });
        receiving
            .recv_timeout(Duration::from_secs(2))
            .expect("production exporter must reach its sink");
        let shutdown = finished.recv_timeout(Duration::from_secs(1));
        let _ = release.send(());
        worker.join().unwrap();
        shutdown.expect("sink must not retain runtime shutdown, even if it never drains");
    }

    #[test]
    fn retention_is_bounded_and_forgetting_cannot_resurrect_a_summary() {
        let registry = registry();
        let run = crate::config::id();
        let mut scopes = Vec::new();
        for _ in 0..RETAINED {
            let scope = registry.begin(&crate::config::id());
            scope.context(&run, None);
            scopes.push(scope);
        }
        assert!(registry.begin(&crate::config::id()).disabled());
        assert_eq!(registry.health()["omitted"], 1);
        scopes.pop().unwrap().submitted();
        let id = crate::config::id();
        let replacement = registry.begin(&id);
        replacement.context(&run, None);
        let trace = replacement.trace();
        assert_eq!(registry.health()["retained"], RETAINED);
        registry.forget(&run);
        trace.add(Counter::EntriesSeen, 5);
        trace.phase(Phase::Mount);
        replacement.submitted();
        drop(replacement);
        assert!(registry.get(&id).is_none());
        assert_eq!(registry.health()["retained"], 0);
        drop(scopes);
    }

    #[test]
    fn retries_have_distinct_invocations_and_submitted_attempts_stay_idempotent() {
        let registry = registry();
        let id = crate::config::id();
        let first = registry.begin(&id);
        assert!(registry.begin(&id).disabled());
        first.rejected();
        let second = registry.begin(&id);
        assert_eq!(registry.get(&id).unwrap().invocation, 2);
        drop(first);
        assert_eq!(registry.get(&id).unwrap().status, Status::Preparing);
        second.submitted();
        assert!(registry.begin(&id).disabled());
        assert_eq!(registry.get(&id).unwrap().status, Status::Submitted);
    }

    #[test]
    fn invocation_identity_survives_eviction_and_never_wraps() {
        let registry = registry();
        let id = crate::config::id();
        let first = registry.begin(&id);
        first.rejected();
        let previous = registry.get(&id).unwrap();
        for _ in 0..RETAINED {
            registry.begin(&crate::config::id()).submitted();
        }
        assert!(registry.get(&id).is_none());
        let repeated = registry.begin(&id);
        let current = registry.get(&id).unwrap();
        assert_eq!(
            current.controller_session_id,
            previous.controller_session_id
        );
        assert_eq!(current.boot_id, previous.boot_id);
        assert_eq!(current.attempt_id, previous.attempt_id);
        assert!(current.invocation > previous.invocation);
        repeated.rejected();
        registry.history.lock().unwrap().order = u64::MAX;
        assert!(registry.begin(&crate::config::id()).disabled());
        assert_eq!(registry.get(&id).unwrap().invocation, current.invocation);
        assert_eq!(registry.health()["omitted"], 1);
    }

    #[test]
    fn report_shape_is_bounded_and_cannot_accept_paths_or_payload_as_identity() {
        let registry = registry();
        assert!(registry.begin("/secret/invalid-id").disabled());
        let id = crate::config::id();
        let scope = registry.begin(&id);
        scope.context("/private/run", Some("token\n/private/payload"));
        for counter in Counter::ALL {
            scope.trace().add(counter, u64::MAX);
            scope.trace().add(counter, 1);
        }
        let encoded = serde_json::to_string(&registry.get(&id).unwrap()).unwrap();
        assert!(encoded.len() < 8192);
        assert!(!encoded.contains("/private"));
        assert!(!encoded.contains("payload"));
        assert!(
            registry
                .get(&id)
                .unwrap()
                .counters
                .iter()
                .all(|counter| counter.value == u64::MAX)
        );
    }

    #[test]
    fn journal_adapter_sends_one_bounded_structured_datagram_and_tolerates_absence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("journal.sock");
        let receiver = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let registry = registry();
        let id = crate::config::id();
        let scope = registry.begin(&id);
        scope.context(&crate::config::id(), Some("pinned-runtime"));
        let snapshot = registry.get(&id).unwrap();
        assert!(send_journal(&snapshot, &path));
        let mut bytes = [0; MAX_EXPORT_BYTES];
        let count = receiver.recv(&mut bytes).unwrap();
        let text = std::str::from_utf8(&bytes[..count]).unwrap();
        assert!(text.contains(&format!("CAIRN_ATTEMPT_ID={id}\n")));
        assert!(text.contains("CAIRN_OPERATION=native_bootstrap\n"));
        let report = text
            .lines()
            .find_map(|line| line.strip_prefix("CAIRN_BOOTSTRAP_JSON="))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(report).unwrap(),
            serde_json::to_value(snapshot).unwrap()
        );
        assert_eq!(text.lines().count(), 7);
        assert!(!send_journal(
            &registry.get(&id).unwrap(),
            &root.path().join("absent")
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn observation_hot_path_measurement() {
        fn cpu_micros() -> u64 {
            let mut time = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            assert_eq!(
                unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) },
                0
            );
            (time.tv_sec as u64)
                .saturating_mul(1_000_000)
                .saturating_add(time.tv_nsec as u64 / 1000)
        }
        let registry = registry();
        let id = crate::config::id();
        let scope = registry.begin(&id);
        let disabled = Trace::default();
        let observed = scope.trace();
        let mut measurements = Vec::new();
        for (mode, trace) in [("disabled", &disabled), ("observed", &observed)] {
            let cpu = cpu_micros();
            let started = std::time::Instant::now();
            for _ in 0..100_000 {
                std::hint::black_box(trace).add(Counter::EntriesSeen, 1);
                drop(std::hint::black_box(trace).operation(Cost::Capture));
            }
            measurements.push(serde_json::json!({
                "mode": mode,
                "cpuMicros": cpu_micros().saturating_sub(cpu),
                "wallMicros": micros(started.elapsed()),
            }));
        }
        let snapshot = registry.get(&id).unwrap();
        assert_eq!(
            snapshot.counters[Counter::EntriesSeen as usize].value,
            100_000
        );
        assert_eq!(snapshot.costs[Cost::Capture as usize].calls, 100_000);
        println!(
            "CAIRN_BOOTSTRAP_OBSERVER_MEASUREMENT {}",
            serde_json::json!({
                "workload": "100000 counter increments and capture timers; no filesystem or export",
                "debugAssertions": cfg!(debug_assertions),
                "parallelism": std::thread::available_parallelism().map(std::num::NonZeroUsize::get).ok(),
                "snapshotBytes": serde_json::to_vec(&snapshot).unwrap().len(),
                "measurements": measurements,
            })
        );
    }
}
