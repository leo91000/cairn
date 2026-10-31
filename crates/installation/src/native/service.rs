//! Native implementation of the existing authenticated node controller protocol.
mod exclusive;
#[cfg(test)]
mod exclusive_tests;
mod health;

use super::{atomic_write, block, bootstrap, disk, mode, systemd, workspace};
use crate::runner::exclusive::{Ledger, Reservations};
use crate::{
    error::{Error, Result},
    microvm::{plan::Plan, protocol::Event, wire},
    nodes::snapshots,
    skills::private_dir,
    storage,
    validation::{text, uuid},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{Request, State},
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bootstrap::Phase;
use bytes::Bytes;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{Mutex, Notify},
};
use tokio_util::sync::CancellationToken;

const MAX_PLAN: u64 = 8_000_000;
const MAX_LOG: u64 = 64 * 1024 * 1024;
const DISK_RELEASE_WAIT: Duration = Duration::from_secs(120);

const DISK_WAIT_TIMEOUT: &str =
    "Native disk ownership was not released before the preparation deadline.";

fn disk_admission_deadline(plan: &Value, wait: Duration) -> (tokio::time::Instant, bool) {
    let execution_wait = plan["expires"].as_i64().map(|until| {
        let remaining = until.saturating_sub(crate::config::now()).max(0) as u64;
        Duration::from_millis(remaining)
    });
    let time_limited = execution_wait.is_some_and(|remaining| remaining <= wait);
    let remaining = execution_wait.map_or(wait, |remaining| remaining.min(wait));
    let deadline = tokio::time::Instant::now() + remaining;
    (deadline, time_limited)
}

fn supervision_stop_reason(
    leased: bool,
    expired: bool,
    conversation_bytes: u64,
    conversation_limit: u64,
    node_bytes: u64,
    node_limit: u64,
) -> Option<&'static str> {
    if !leased {
        return Some("lease_expired");
    }
    if expired {
        return Some("time_limit");
    }
    if conversation_bytes > conversation_limit {
        return Some("conversation_disk_limit");
    }
    if node_bytes > node_limit {
        return Some("node_disk_limit");
    }
    None
}

pub(crate) fn native_budget(value: &Value) -> Result<crate::microvm::budget::Budget> {
    if value["nativeModeProtocol"] != 1 {
        return Err(Error::bad("Unknown native mode storage protocol."));
    }
    budget_request(value)
}

fn budget_request(value: &Value) -> Result<crate::microvm::budget::Budget> {
    let mut value = value.clone();
    if let Some(protocol) = value
        .as_object_mut()
        .and_then(|value| value.remove("nativeModeProtocol"))
        && protocol != 1
    {
        return Err(Error::bad("Unknown native mode storage protocol."));
    }
    let budget: crate::microvm::budget::Budget = serde_json::from_value(value)?;
    budget.limits.validate().map_err(Error::bad)?;
    if !(1..=4096).contains(&budget.slots) {
        return Err(Error::bad("Use between 1 and 4096 execution slots."));
    }
    Ok(budget)
}

fn budget_record(budget: &Value) -> Value {
    let mut record = budget.clone();
    record["nativeModeProtocol"] = 1.into();
    record
}

async fn load_budget(path: &Path) -> Result<crate::microvm::budget::Budget> {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)?;
            native_budget(&value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            budget_request(&json!({ "slots": 1, "limits": crate::nodes::placement::defaults() }))
        }
        Err(error) => Err(error.into()),
    }
}

struct Attempt {
    run: String,
    expires: Option<i64>,
    stop: CancellationToken,
    started: bool,
    protection: Option<tokio::task::JoinHandle<()>>,
    suspension: Arc<super::suspension::Suspension>,
    // External mounted-filesystem access; pool accounting remains global.
    live_access: Arc<Mutex<()>>,
}

#[cfg_attr(test, derive(Default))]
pub(super) struct StorageClose {
    live_access: Arc<Mutex<()>>,
    mutations_drained: bool,
}

impl StorageClose {
    fn new(live_access: Arc<Mutex<()>>) -> Self {
        Self {
            live_access,
            mutations_drained: false,
        }
    }

    pub(super) async fn finish(
        &mut self,
        closing: impl std::future::Future<Output = Result<block::Completion>>,
    ) -> Result<block::Completion> {
        if !self.mutations_drained {
            // Cancellation is already published and the workload is fenced.
            // Drain admitted mutations once; later ones observe cancellation.
            let admitted = tokio::time::timeout(Duration::from_secs(300), self.live_access.lock())
                .await
                .map_err(|_| {
                    Error::unavailable(
                        "Native filesystem mutations did not drain before their deadline.",
                    )
                })?;
            drop(admitted);
            self.mutations_drained = true;
        }
        // The clean backend namespace removes host mount-inventory coupling.
        closing.await
    }
}

fn completion_status(previous: i32, completion: block::Completion) -> (i32, bool) {
    (
        completion.exit_code(previous),
        completion == block::Completion::FailedFlush,
    )
}

fn exit_record(code: i32, finalization_failed: bool) -> String {
    if finalization_failed {
        json!({ "StatusCode": code, "FinalizationFailed": true }).to_string()
    } else {
        // This function is called for a healthy final write only after the
        // workload, mount ownership and suspension accounting have drained.
        json!({ "StatusCode": code, "PhysicalFinalized": true }).to_string()
    }
}

async fn preserve_interrupted_exit(marker: &std::path::Path) -> Result<()> {
    let exit = marker.with_extension("exit");
    if !exit.exists() {
        atomic_write(
            &exit,
            crate::runner::CONTROLLER_INTERRUPTED.to_string().as_bytes(),
        )
        .await?;
    }
    Ok(())
}

#[derive(Clone)]
struct Controller {
    data: PathBuf,
    state: PathBuf,
    active: Arc<Mutex<HashMap<String, Attempt>>>,
    capacity: Arc<Mutex<super::capacity::Capacity>>,
    mounted: Arc<Mutex<HashMap<String, Arc<block::Handle>>>>,
    leases: Arc<Mutex<HashMap<String, u64>>>,
    admission: Arc<Mutex<()>>,
    exclusive: Arc<Mutex<Ledger>>,
    base_warmer: Arc<super::bases_warmer::Warmer>,
    budget: Arc<Mutex<Value>>,
    storage_policy: crate::storage::policy::Control,
    mode: Arc<Mutex<Value>>,
    changed: Arc<Notify>,
    log_lock: Arc<Mutex<()>>,
    telemetry: health::Telemetry,
    bootstrap: bootstrap::Registry,
    stop: CancellationToken,
    identity: Arc<systemd::Identity>,
    backend_namespace: storage::ublk::BackendNamespace,
}

impl Controller {
    fn bases(&self) -> super::bases::Bases {
        super::bases::Bases {
            data: self.data.clone(),
            state: self.state.clone(),
            identity: (self.identity.uid, self.identity.gid),
        }
    }

    fn file(&self, id: &str, extension: &str) -> PathBuf {
        self.state.join(format!("{id}.{extension}"))
    }

    async fn plan(&self, id: &str) -> Result<Value> {
        uuid(id)?;
        let file = tokio::fs::File::open(self.data.join("runner-plans").join(format!("{id}.json")))
            .await?;
        let mut bytes = Vec::new();
        file.take(MAX_PLAN + 1).read_to_end(&mut bytes).await?;
        if bytes.len() as u64 > MAX_PLAN {
            return Err(Error::bad("Native plan exceeds limit."));
        }
        let plan = Plan::new(serde_json::from_slice(&bytes)?);
        let guest_data = guest_data(plan.as_value(), &self.data);
        let mut checked = plan.as_value().clone();
        for import in checked["imports"]
            .as_array_mut()
            .ok_or_else(|| Error::bad("Missing native imports."))?
        {
            let source = PathBuf::from(text(import, "source"));
            let relative = source
                .strip_prefix(&self.data)
                .map_err(|_| Error::bad("Native seed outside node storage."))?;
            import["source"] = guest_data
                .join(relative)
                .to_string_lossy()
                .into_owned()
                .into();
        }
        crate::runner::validate_plan(&Plan::new(checked), id, &guest_data)?;
        if plan.as_value()["backend"] != "native"
            || !plan.node_lease_required()
            || plan.as_value().get("command").is_some()
        {
            return Err(Error::bad(
                "Native execution requires an explicit managed plan and lease.",
            ));
        }
        let root = PathBuf::from(text(plan.as_value(), "projectRoot"));
        let run_root = guest_data.join("runs").join(plan.run_id());
        if !root.starts_with(&run_root) || root == run_root || !plan.cwd().starts_with(&root) {
            return Err(Error::bad("Invalid native project root."));
        }
        for import in plan.imports() {
            if tokio::fs::canonicalize(import.source).await? != import.source {
                return Err(Error::bad("Native import changed location."));
            }
        }
        Ok(plan.as_value().clone())
    }

    async fn leased(&self, id: &str) -> bool {
        self.leases
            .lock()
            .await
            .get(id)
            .is_some_and(|until| *until > crate::nodes::boot_ms())
    }

    async fn persist_mode(&self, state: &Value) -> Result<()> {
        let bytes = serde_json::to_vec(state)?;
        if let Err(error) = atomic_write(&self.state.join(mode::FILE), &bytes).await {
            // Keep the last confirmed mode and target, but report the actual
            // failure through health even when the disk cannot record it.
            let mut observed = self.mode.lock().await;
            observed["status"] = "failed".into();
            observed["error"] = error.message.clone().into();
            return Err(error);
        }
        Ok(())
    }

    async fn change_mode(&self, value: Value) -> Result<Value> {
        let request = mode::request(value)?;
        let expected = serde_json::to_value(&request)?;
        let _admission = self.admission.lock().await;
        let previous = self.mode.lock().await.clone();
        if mode::matches(&previous, &expected) {
            return Ok(previous);
        }
        let mut pending = previous.clone();
        pending["status"] = "pending".into();
        pending["target"] = expected.clone();
        pending["error"] = Value::Null;
        // Refuse old leased plans even if persisting intent fails. No sudo
        // mutation or applied acknowledgement may precede durable intent.
        *self.mode.lock().await = pending.clone();
        self.persist_mode(&pending).await?;
        let result = tokio::time::timeout(Duration::from_secs(270), async {
            // Signal every preparation/workload first; all process descendants
            // belong to the managed cgroup. Root can escape it: this is cooperative.
            let active: Vec<_> = self
                .active
                .lock()
                .await
                .iter()
                .map(|(id, attempt)| {
                    attempt.stop.cancel();
                    id.clone()
                })
                .collect();
            for id in active {
                self.stop_admitted(&id).await?;
            }
            if !self.active.lock().await.is_empty() {
                return Err(Error::conflict(
                    "Native executions have not finished stopping.",
                ));
            }
            mode::sudo(
                &self.identity,
                request.mode == crate::nodes::native_mode::Mode::Administrator,
            )
            .await
        })
        .await;
        let state = match result {
            Ok(Ok(())) => {
                json!({ "mode": request.mode, "generation": request.generation, "status": "applied" })
            }
            result => {
                pending["status"] = "failed".into();
                pending["error"] = match result {
                    Ok(Err(error)) => error.message,
                    _ => "Native privilege transition stop confirmation timed out.".into(),
                }
                .into();
                pending
            }
        };
        self.persist_mode(&state).await?;
        *self.mode.lock().await = state.clone();
        Ok(state)
    }

    /// Admission precedes the attempt's lease watcher. Keep its initial storage
    /// wait bounded too, and observe stop markers without blocking DELETE.
    async fn start_storage(&self, id: &str, plan: &Value) -> Result<disk::StorageGuard> {
        let (deadline, time_limited) = disk_admission_deadline(plan, DISK_RELEASE_WAIT);
        let waiting = disk::lock(&self.state);
        tokio::pin!(waiting);
        let mut acquired = None;
        loop {
            let rejected = if self.stop.is_cancelled() {
                Some((
                    "controller_shutdown",
                    Error::unavailable("Native controller is stopping."),
                ))
            } else if plan["expires"]
                .as_i64()
                .is_some_and(|until| crate::config::now() >= until)
            {
                Some((
                    "time_limit",
                    Error::timeout(
                        "Native execution exceeded its time limit before disk admission.",
                    ),
                ))
            } else if self.file(id, "stopped").exists() || self.file(id, "exit").exists() {
                Some(("cancelled", Error::conflict("Attempt has already stopped.")))
            } else if !self.leased(id).await {
                Some((
                    "lease_expired",
                    Error::conflict("Native execution lease is missing or expired."),
                ))
            } else {
                None
            };
            if let Some((reason, error)) = rejected {
                drop(acquired);
                self.record_stop(id, reason, Value::Null).await;
                return Err(error);
            }
            // Recheck the deadline/lease/stop state even after acquiring the lock.
            if let Some(owner) = acquired {
                return Ok(owner);
            }
            tokio::select! {
                biased;
                () = self.stop.cancelled() => {},
                () = tokio::time::sleep_until(deadline) => {
                    return Err(self.disk_admission_timeout(id, time_limited).await);
                }
                result = &mut waiting => acquired = Some(result?),
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
    }

    async fn start(&self, id: &str) -> Result<()> {
        uuid(id)?;
        let waiting = self.bootstrap.begin(id);
        let _admission = self.admission.lock().await;
        // Drop this scope before releasing admission, including on cancellation.
        // A queued duplicate may then observe the preceding rejected invocation.
        let mut observation = waiting;
        if observation.disabled() {
            observation = self.bootstrap.begin(id);
        }
        let result = self.start_observed(id, &mut observation).await;
        if result.is_err() {
            observation.rejected();
        }
        result
    }

    async fn start_observed(&self, id: &str, observation: &mut bootstrap::Scope) -> Result<()> {
        self.allow_exclusive_admission(id).await?;
        if self.active.lock().await.contains_key(id) {
            observation.already_running();
            return Ok(());
        }
        if self.file(id, "exit").exists() || self.file(id, "stopped").exists() {
            return Err(Error::conflict("Attempt has already stopped."));
        }
        if !self.leased(id).await {
            return Err(Error::conflict(
                "Native execution lease is missing or expired.",
            ));
        }
        if self.active.lock().await.len() as u64
            >= self.budget.lock().await["slots"].as_u64().unwrap_or(0)
        {
            return Err(Error::unavailable("The native execution slot is occupied."));
        }
        let trace = observation.trace();
        trace.phase(Phase::Plan);
        let plan = self.plan(id).await?;
        observation.context(text(&plan, "runId"), plan["runtimeId"].as_str());
        mode::check_plan(&*self.mode.lock().await, &plan)?;
        let resources: crate::nodes::Resources = serde_json::from_value(plan["resources"].clone())?;
        resources.validate().map_err(Error::bad)?;
        let budget: crate::nodes::Resources =
            serde_json::from_value(self.budget.lock().await["limits"].clone())?;
        if resources.cpu > budget.cpu
            || resources.memory_mi_b > budget.memory_mi_b
            || resources.disk_mi_b > budget.disk_mi_b
        {
            return Err(Error::conflict(
                "Native plan exceeds the node owner's configured resource budget.",
            ));
        }
        trace.phase(Phase::StorageAdmission);
        let _disk = self.start_storage(id, &plan).await?;
        trace.phase(Phase::PoolAccounting);
        self.check_preparation_space(&plan).await?;
        let root = workspace::directory(&self.state, text(&plan, "runId"))?;
        trace.phase(Phase::WorkspaceOwnership);
        let owner = workspace::execution_ownership(&self.state, text(&plan, "runId")).await?;
        let stop = self.stop.child_token();
        let live_access = Arc::new(Mutex::new(()));
        atomic_write(&self.file(id, "run"), text(&plan, "runId").as_bytes()).await?;
        atomic_write(&self.file(id, "active"), text(&plan, "runId").as_bytes()).await?;
        atomic_write(&self.file(id, "log"), b"").await?;
        self.active.lock().await.insert(
            id.into(),
            Attempt {
                run: text(&plan, "runId").into(),
                expires: plan["expires"].as_i64(),
                stop: stop.clone(),
                started: false,
                protection: None,
                suspension: Arc::new(super::suspension::Suspension::new(&self.state, id)),
                live_access: live_access.clone(),
            },
        );
        self.capacity.lock().await.register(
            id,
            text(&plan, "runId"),
            resources.disk_mi_b.saturating_mul(disk::MIB),
        );
        let (controller, id) = (self.clone(), id.to_owned());
        let observation = std::mem::take(observation);
        let (lease_controller, lease_id, lease_stop) =
            (controller.clone(), id.clone(), stop.clone());
        tokio::spawn(async move {
            lease_controller.watch_lease(&lease_id, lease_stop).await;
        });
        tokio::spawn(async move {
            let result = controller
                .execute(&id, &plan, stop.clone(), &observation)
                .await;
            if result.is_err() {
                observation.rejected();
            }
            // The observation ends at submission, or at an interrupted bootstrap;
            // it never waits for fencing, filesystem close, or a telemetry sink.
            drop(observation);
            // Reject in-flight file mutations before flushing/unmounting. Any
            // already admitted access retains both its pool and attempt guards.
            stop.cancel();
            // The physical owner lock and active marker survive any uncertain stop.
            while let Err(error) = systemd::stop(&id).await {
                let _ = controller.abort_disk(&id).await;
                tracing::error!(attempt = id, message = %error.message, "Native workload fencing must be retried");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let protection = controller
                .active
                .lock()
                .await
                .get_mut(&id)
                .and_then(|attempt| attempt.protection.take());
            if let Some(protection) = protection {
                // A late freeze/thaw must finish before this mount's leases or
                // volume can be released to a following attempt.
                let _ = protection.await;
            }
            let mut code = match result {
                Ok(code) => code,
                Err(error) => {
                    let message = format!("Native execution failed: {}\n", error.message);
                    let _ = controller.append(&id, true, message.as_bytes()).await;
                    crate::runner::CONTROLLER_FAILED
                }
            };
            let mut finalization_failed = false;
            // Deliver the result path consumed by the node connector.
            let result = root.join("data/output/result.md");
            if let Ok(file) = crate::artifacts::file::open_export(&result, &root.join("data")).await
            {
                let file = tokio::fs::File::from_std(file);
                let mut bytes = Vec::new();
                if file.take(1_000_001).read_to_end(&mut bytes).await.is_ok()
                    && bytes.len() <= 1_000_000
                {
                    let target = controller
                        .data
                        .join("runs")
                        .join(text(&plan, "runId"))
                        .join("output/result.md");
                    let _ = atomic_write(&target, &bytes).await;
                }
            }
            {
                let mounted = {
                    controller
                        .mounted
                        .lock()
                        .await
                        .get(text(&plan, "runId"))
                        .cloned()
                };
                let had_mount = mounted.is_some();
                if let Some(mounted) = mounted {
                    let mut storage_close = StorageClose::new(live_access);
                    loop {
                        let result = storage_close
                            .finish(async { mounted.mount.lock().await.close().await })
                            .await;
                        match result {
                            Ok(completion) => {
                                (code, finalization_failed) = completion_status(code, completion);
                                if finalization_failed {
                                    tracing::error!(
                                        attempt = id,
                                        "Native filesystem could not be flushed; the durable crash prefix was retained"
                                    );
                                    let exit = exit_record(code, true);
                                    if !matches!(
                                        tokio::time::timeout(
                                            Duration::from_secs(1),
                                            atomic_write(
                                                &controller.file(&id, "exit"),
                                                exit.as_bytes()
                                            )
                                        )
                                        .await,
                                        Ok(Ok(()))
                                    ) {
                                        tracing::error!(
                                            attempt = id,
                                            "Native flush evidence could not be saved before physical cleanup"
                                        );
                                    }
                                }
                                break;
                            }
                            Err(error) => {
                                tracing::error!(attempt = id, %error, "Native filesystem must finish flushing and unmounting before ownership release");
                                tokio::time::sleep(Duration::from_secs(2)).await;
                            }
                        }
                    }
                }
                controller.mounted.lock().await.remove(text(&plan, "runId"));
                while !had_mount
                    && let Err(error) =
                        block::recover_completed(&controller.state, text(&plan, "runId"), &id).await
                {
                    tracing::error!(attempt = id, %error, "Native completed generation must be recovered before ownership release");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
            // Release physical ownership before reporting completion to a replacement.
            drop(owner);
            let suspension = controller
                .active
                .lock()
                .await
                .get(&id)
                .map(|attempt| attempt.suspension.clone());
            if let Some(suspension) = suspension {
                while let Err(error) = suspension.finish().await {
                    tracing::error!(%error, "Native suspension accounting must finish before reporting completion");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
            // The active marker must survive until suspension accounting is
            // finalized, so recovery can close it after any controller crash.
            let exit = exit_record(code, finalization_failed);
            let mut retry_delay = Duration::from_secs(1);
            loop {
                match crate::runner::write_final_result(
                    &controller.file(&id, "exit"),
                    exit.as_bytes(),
                    finalization_failed,
                )
                .await
                {
                    Ok(()) => break,
                    Err(error) if finalization_failed => {
                        tracing::error!(attempt = id, %error, "Native completion must be saved before releasing the attempt");
                        tokio::time::sleep(retry_delay).await;
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                    }
                    Err(_) => break,
                }
            }
            let _ = tokio::fs::remove_file(controller.file(&id, "active")).await;
            controller.active.lock().await.remove(&id);
            controller.capacity.lock().await.attempts.remove(&id);
            controller.leases.lock().await.remove(&id);
            controller.changed.notify_waiters();
        });
        Ok(())
    }

    async fn watch_lease(&self, id: &str, stop: CancellationToken) {
        loop {
            tokio::select! {
                () = stop.cancelled() => {},
                () = tokio::time::sleep(Duration::from_millis(100)) => {
                    if self.leased(id).await {
                        continue;
                    }
                    self.record_stop(id, "lease_expired", Value::Null).await;
                    stop.cancel();
                }
            }
            let preparation = self
                .active
                .lock()
                .await
                .get(id)
                .is_some_and(|attempt| !attempt.started);
            if preparation {
                let _ = self.abort_disk(id).await;
            }
            break;
        }
    }

    async fn append(&self, id: &str, stderr: bool, bytes: &[u8]) -> Result<()> {
        let _log = self.log_lock.lock().await;
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(self.file(id, "log"))
            .await?;
        if file.metadata().await?.len() > MAX_LOG {
            return Err(Error::bad("Native output exceeded its bounded log budget."));
        }
        wire::write(
            &mut file,
            &serde_json::to_value(Event::Output {
                stderr,
                data: STANDARD.encode(bytes),
            })?,
        )
        .await?;
        file.flush().await?;
        self.changed.notify_waiters();
        Ok(())
    }

    async fn output(
        &self,
        id: &str,
        stderr: bool,
        mut reader: impl AsyncRead + Unpin,
    ) -> Result<()> {
        let mut buffer = vec![0u8; 16384];
        loop {
            let count = reader.read(&mut buffer).await?;
            if count == 0 {
                return Ok(());
            }
            self.append(id, stderr, &buffer[..count]).await?;
        }
    }

    /// Busy journal/physical leases can outlive the previous systemd workload
    /// while a capture finishes. Wait within this attempt rather than consuming
    /// controller recovery attempts. Never hold the node-wide storage lock while
    /// sleeping: the previous owner may need it to complete shutdown.
    #[cfg(test)]
    async fn mount_after_release(
        &self,
        id: &str,
        plan: &Value,
        stop: &CancellationToken,
        wait: Duration,
    ) -> Result<(disk::StorageGuard, bool, block::Mounted)> {
        self.mount_after_release_observed(id, plan, stop, wait, &bootstrap::Trace::default())
            .await
    }

    async fn mount_after_release_observed(
        &self,
        id: &str,
        plan: &Value,
        stop: &CancellationToken,
        wait: Duration,
        trace: &bootstrap::Trace,
    ) -> Result<(disk::StorageGuard, bool, block::Mounted)> {
        let (deadline, time_limited) = disk_admission_deadline(plan, wait);
        let mut reported = false;
        loop {
            if stop.is_cancelled() || !self.leased(id).await {
                return Err(self.preparation_stopped(id, plan).await);
            }
            if plan["expires"]
                .as_i64()
                .is_some_and(|until| crate::config::now() >= until)
            {
                return Err(self.disk_admission_timeout(id, true).await);
            }
            // Cancel only lock acquisition, never an in-flight mount or clone.
            trace.phase(Phase::StorageAdmission);
            let owner = tokio::select! {
                biased;
                () = stop.cancelled() => {
                    return Err(self.preparation_stopped(id, plan).await);
                }
                result = tokio::time::timeout_at(deadline, disk::lock(&self.state)) => {
                    match result {
                        Ok(result) => result?,
                        Err(_) => return Err(self.disk_admission_timeout(id, time_limited).await),
                    }
                }
            };
            if stop.is_cancelled() || !self.leased(id).await {
                drop(owner);
                return Err(self.preparation_stopped(id, plan).await);
            }
            // The global lock may have become available as the deadline elapsed.
            if plan["expires"]
                .as_i64()
                .is_some_and(|until| crate::config::now() >= until)
            {
                drop(owner);
                return Err(self.disk_admission_timeout(id, true).await);
            }
            let result: Result<(bool, block::Mounted)> = async {
                trace.phase(Phase::PoolAccounting);
                self.check_preparation_space(plan).await?;
                trace.phase(Phase::FilesystemInitialize);
                let from_base = self.bases().initialize(plan, stop).await?;
                trace.phase(Phase::Mount);
                let mounted = block::mount(
                    &self.state,
                    text(plan, "runId"),
                    plan,
                    stop,
                    &self.backend_namespace,
                )
                .await?;
                Ok((from_base, mounted))
            }
            .await;
            match result {
                Ok((from_base, mounted)) => return Ok((owner, from_base, mounted)),
                Err(error) if error.is_conflict() && error.message == block::DISK_BUSY => {
                    drop(owner);
                    trace.phase(Phase::WorkspaceOwnership);
                    if !reported {
                        tracing::info!(
                            attempt = id,
                            run = text(plan, "runId"),
                            "Waiting for previous native disk ownership to be released"
                        );
                        let _ = self
                            .append(
                                id,
                                true,
                                b"Waiting for previous native disk ownership to be released.\n",
                            )
                            .await;
                        reported = true;
                    }
                }
                Err(error) => return Err(error),
            }
            tokio::select! {
                biased;
                () = stop.cancelled() => {
                    return Err(self.preparation_stopped(id, plan).await);
                }
                () = tokio::time::sleep_until(deadline) => {
                    return Err(self.disk_admission_timeout(id, time_limited).await);
                }
                () = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
        }
    }

    async fn expired(&self, id: &str, expires: Option<i64>) -> bool {
        if expires.is_none() {
            return false;
        }
        let paused = self.administrative_ms(id).await.unwrap_or(0);
        expires.is_some_and(|until| {
            crate::config::now() >= until.saturating_add(i64::try_from(paused).unwrap_or(i64::MAX))
        })
    }

    async fn cancellation_reason(&self, id: &str, plan: &Value) -> &'static str {
        if self.stop.is_cancelled() {
            return "controller_shutdown";
        }
        if self.expired(id, plan["expires"].as_i64()).await {
            return "time_limit";
        }
        if !self.leased(id).await {
            return "lease_expired";
        }
        "cancelled"
    }

    async fn preparation_stopped(&self, id: &str, plan: &Value) -> Error {
        let reason = self.cancellation_reason(id, plan).await;
        self.record_stop(id, reason, Value::Null).await;
        Error::unavailable("Native disk preparation stopped.")
    }

    async fn disk_admission_timeout(&self, id: &str, time_limit: bool) -> Error {
        if time_limit {
            self.record_stop(id, "time_limit", Value::Null).await;
            return Error::timeout(
                "Native execution exceeded its time limit before disk admission.",
            );
        }
        self.record_stop(id, "disk_ownership_timeout", Value::Null)
            .await;
        Error::unavailable(DISK_WAIT_TIMEOUT)
    }

    /// Diagnostic persistence must never prevent workload fencing.
    async fn record_stop(&self, id: &str, reason: &str, usage: Value) {
        let details = json!({
            "reason": reason,
            "at": crate::config::now(),
            "usage": usage,
        });
        tracing::warn!(attempt = id, diagnostics = %details, "Native controller stopping workload");
        let recorded = tokio::time::timeout(
            Duration::from_secs(1),
            self.write_stop_diagnostics(id, &details),
        )
        .await;
        if recorded.is_err() {
            tracing::warn!(
                attempt = id,
                "Native stop diagnostics timed out; proceeding with workload fencing"
            );
        }
    }

    async fn write_stop_diagnostics(&self, id: &str, details: &Value) {
        if let Err(error) =
            super::record_stop_once(&self.file(id, "stop-reason.json"), details).await
        {
            tracing::warn!(attempt = id, %error, "Could not persist native stop reason");
        }
        let message = format!("Native controller stopping workload: {details}\n");
        let _ = self.append(id, true, message.as_bytes()).await;
    }

    async fn execute(
        &self,
        id: &str,
        plan: &Value,
        stop: CancellationToken,
        observation: &bootstrap::Scope,
    ) -> Result<i32> {
        let trace = observation.trace();
        let root = workspace::directory(&self.state, text(plan, "runId"))?;
        atomic_write(&root.join("attempt"), id.as_bytes()).await?;
        atomic_write(
            &root.join("resources.json"),
            &serde_json::to_vec(&plan["resources"])?,
        )
        .await?;
        let data = root.join("data");
        let runtime = plan["runtimeId"]
            .as_str()
            .ok_or_else(|| Error::bad("Missing pinned native runtime."))?;
        super::systemd::tools(runtime)?;
        let (disk_owner, from_base, mounted) = self
            .mount_after_release_observed(id, plan, &stop, DISK_RELEASE_WAIT, &trace)
            .await?;
        self.mounted.lock().await.insert(
            text(plan, "runId").to_owned(),
            Arc::new(block::Handle::new(mounted)),
        );
        if stop.is_cancelled() {
            return Ok(143);
        }
        let administrator = plan["nativeMode"]["mode"] == "administrator";
        trace.phase(Phase::IdentityProvision);
        let mut identity = super::identity::provision(&self.state, text(plan, "runId")).await?;
        if administrator {
            identity.host_engine = Some((self.identity.uid, self.identity.gid));
        }
        trace.phase(Phase::FilesystemInitialize);
        block::initialize(
            &self.state,
            text(plan, "runId"),
            plan,
            self.disk_budget(plan).min(self.disk_growth().await?),
        )
        .await?;
        super::bases::release_pin(
            &self.data.join("runs").join(text(plan, "runId")),
            text(plan, "id"),
        )
        .await?;
        if from_base || plan.get("nativeProjectBase").is_some() {
            self.bases()
                .trim(super::bases::cache_limit(self.disk_limit().await?), 0, None)
                .await?;
        }
        trace.phase(Phase::TemporaryPrepare);
        let temporary = root.join("tmp");
        if temporary.exists() {
            tokio::fs::remove_dir_all(&temporary).await?;
        }
        private_dir(&temporary).await?;
        workspace::own_tree(&temporary, identity.uid(), identity.gid()).await?;
        let stage = self.data.join("runs").join(text(plan, "runId"));
        let bindings =
            super::home::prepare_for_identity(&stage.join("home"), &data, &identity, &trace)
                .await?;
        // Keep the existing prepared-base admission marker. The authoritative
        // ownership mapping now travels inside the published filesystem.
        atomic_write(
            &root.join("file-owner.json"),
            &serde_json::to_vec(&identity)?,
        )
        .await?;
        trace.phase(Phase::RuntimePrepare);
        let docker_data = data.join("docker");
        // Only namespace root may access raw daemon metadata. Engine access is
        // exclusively through the mandatory authorization plugin.
        tokio::fs::create_dir_all(&docker_data).await?;
        std::os::unix::fs::lchown(
            &docker_data,
            Some(identity.root_uid),
            Some(identity.root_gid),
        )?;
        tokio::fs::set_permissions(&docker_data, std::fs::Permissions::from_mode(0o700)).await?;
        let runtime = root.join("docker-runtime");
        if runtime.exists() {
            tokio::fs::remove_dir_all(&runtime).await?;
        }
        for name in ["runtime", "rootlesskit", "plugins"] {
            let path = runtime.join(name);
            private_dir(&path).await?;
            std::os::unix::fs::lchown(&path, Some(identity.root_uid), Some(identity.root_gid))?;
        }
        trace.phase(Phase::PoolAccounting);
        if disk::used(&self.data, &self.state).await? > self.disk_limit().await? {
            return Err(Error::conflict(
                "Native preparation exceeds the shared node disk budget; local work is retained.",
            ));
        }
        trace.phase(Phase::RuntimePrepare);
        let container = self.state.join("roots").join(id);
        tokio::fs::create_dir_all(&container).await?;
        tokio::fs::set_permissions(&container, std::fs::Permissions::from_mode(0o755)).await?;
        for dir in [
            "etc",
            "dev",
            "proc",
            "sys",
            "run",
            "tmp",
            "var",
            "var/tmp",
            "home",
            "home/node",
        ] {
            let path = container.join(dir);
            tokio::fs::create_dir_all(&path).await?;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).await?;
        }
        let mut plan = plan.clone();
        plan["guestDataRoot"] = guest_data(&plan, &self.data)
            .to_string_lossy()
            .into_owned()
            .into();
        if !administrator {
            plan["nativeIdentity"] = serde_json::to_value(&identity)?;
        }
        plan["nativeHomeBindings"] = serde_json::to_value(bindings)?;
        if plan["chat"]["provider"] == "claude" {
            plan["chat"]["claudeCredentialsDirectory"] = super::home::CLAUDE_AUTH_DIRECTORY.into();
        }
        let mcp_stop = stop.child_token();
        let _mcp_guard = mcp_stop.clone().drop_guard();
        if administrator {
            // Preserve administrator's host execution and existing local relay;
            // it does not enter the protected Docker namespace or receive its socket.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let origin = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
            for field in ["codexConfig", "claudeMcps", "opencodeConfig", "args"] {
                if let Some(value) = plan["chat"].get_mut(field) {
                    replace_origin(value, &origin);
                }
            }
            if let Some(value) = plan.get_mut("args") {
                replace_origin(value, &origin);
            }
            plan["mcpEnv"][crate::plugins::cli::ORIGIN_ENV] = origin.into();
            tokio::spawn(relay(
                listener,
                stage.join("home").join(crate::microvm::mcp::SOCKET),
                mcp_stop,
            ));
        } else {
            plan["mcpEnv"][crate::plugins::cli::ORIGIN_ENV] = crate::microvm::mcp::ORIGIN.into();
        }
        let auth = stage
            .join("home")
            .join(crate::nodes::workspace::provider_home(&plan))
            .join("cairn-auth.sock");
        let channel = stage.join("home").join(crate::microvm::mcp::SOCKET);
        if plan["chat"].is_object() {
            let _ = tokio::time::timeout(Duration::from_secs(3), async {
                while !auth.exists() || !channel.exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
        }
        if !administrator && channel.exists() {
            use std::os::unix::fs::FileTypeExt;
            if !tokio::fs::symlink_metadata(&channel)
                .await?
                .file_type()
                .is_socket()
            {
                return Err(Error::unavailable(
                    "Native conversation MCP channel is not a socket.",
                ));
            }
            std::os::unix::fs::lchown(&channel, Some(identity.root_uid), Some(identity.root_gid))?;
        } else if !administrator && plan["chat"].is_object() {
            return Err(Error::unavailable(
                "Native conversation MCP channel did not become available.",
            ));
        }
        let auth = if auth.exists() {
            std::os::unix::fs::lchown(&auth, Some(identity.uid()), Some(identity.gid()))?;
            Some(auth)
        } else {
            None
        };
        let plan_file = container.with_extension("plan.json");
        let encoded_plan = serde_json::to_vec(&plan)?;
        if encoded_plan.len() as u64 > self.disk_growth().await? {
            return Err(Error::conflict(
                "Insufficient shared native disk budget for the execution plan.",
            ));
        }
        atomic_write(&plan_file, &encoded_plan).await?;
        tokio::fs::set_permissions(&plan_file, std::fs::Permissions::from_mode(0o444)).await?;
        // The staging inbox is readonly in the workload's mount namespace.
        private_dir(&stage.join("chat-input")).await?;
        crate::nodes::workspace::prepare_native_inbox(&stage.join("chat-input")).await?;
        let launch = systemd::Launch {
            id,
            plan: &plan,
            data: &self.data,
            root: &container,
            home: &data.join("home"),
            workspace: &data.join("workspaces"),
            output: &data.join("output"),
            identity: &self.identity,
            conversation: (!administrator).then_some(&identity),
            docker_data: (!administrator).then_some(docker_data.as_path()),
            docker_runtime: (!administrator).then_some(runtime.as_path()),
            temporary: &temporary,
            auth: auth.as_deref(),
        };
        drop(disk_owner);
        if !self.leased(id).await || stop.is_cancelled() {
            return Ok(143);
        }
        if let Some(attempt) = self.active.lock().await.get_mut(id) {
            attempt.started = true;
        }
        trace.phase(Phase::WorkloadLaunch);
        systemd::start_guardian(id, &self.file(id, "lease")).await?;
        let mut child = systemd::spawn(&launch)?;
        // This only proves submission to systemd-run, not unit readiness or a
        // coding-engine turn. Those retain their existing protocol evidence.
        observation.submitted();
        let storage_stop = stop.child_token();
        let _storage_guard = storage_stop.clone().drop_guard();
        let mounted = self
            .mounted
            .lock()
            .await
            .get(text(&plan, "runId"))
            .cloned()
            .ok_or_else(|| Error::internal("Native disk ownership missing."))?;
        let suspension = self
            .active
            .lock()
            .await
            .get(id)
            .ok_or_else(|| Error::conflict("Native execution stopped during launch."))?
            .suspension
            .clone();
        let (protection_id, protection_stop) = (id.to_owned(), stop.clone());
        let protection_controller = self.clone();
        let protection = tokio::spawn(async move {
            if let Err(error) = super::protection::monitor(
                mounted.volume.clone(),
                &protection_id,
                suspension,
                storage_stop,
            )
            .await
            {
                tracing::error!(attempt = protection_id, %error, "Native storage CPU protection failed; fencing workload");
                protection_controller
                    .record_stop(&protection_id, "storage_protection_failed", Value::Null)
                    .await;
                protection_stop.cancel();
                if systemd::stop(&protection_id).await.is_err() {
                    let _ = mounted.abort_io().await;
                }
            }
        });
        if let Some(attempt) = self.active.lock().await.get_mut(id) {
            attempt.protection = Some(protection);
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::internal("Missing native output."))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::internal("Missing native diagnostics."))?;
        let (out_self, err_self, out_id, err_id) =
            (self.clone(), self.clone(), id.to_owned(), id.to_owned());
        let mut out = tokio::spawn(async move { out_self.output(&out_id, false, stdout).await });
        let mut err = tokio::spawn(async move { err_self.output(&err_id, true, stderr).await });
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        let mut readers = 0;
        let code = loop {
            tokio::select! {
                result = child.wait() => break result?.code().unwrap_or(143),
                () = stop.cancelled() => {
                    let reason = self.cancellation_reason(id, &plan).await;
                    self.record_stop(id, reason, Value::Null).await;
                    systemd::stop(id).await?;
                    break 143;
                }
                _ = timer.tick() => {
                    let leased = self.leased(id).await;
                    let expired = self.expired(id, plan["expires"].as_i64()).await;
                    let workspace_bytes = block::used_bytes(&data)?;
                    let temporary_bytes = disk::bytes_if_present(&temporary).await?;
                    let conversation_bytes = workspace_bytes.saturating_add(temporary_bytes);
                    let conversation_limit = self.monitor_capacity(id, &plan, workspace_bytes, temporary_bytes).await?;
                    let node_bytes = disk::used(&self.data, &self.state).await?;
                    let node_limit = self.disk_limit().await?;
                    let reason = supervision_stop_reason(
                        leased,
                        expired,
                        conversation_bytes,
                        conversation_limit,
                        node_bytes,
                        node_limit,
                    );
                    if let Some(reason) = reason {
                        let usage = json!({
                            "workspaceBytes": workspace_bytes,
                            "temporaryBytes": temporary_bytes,
                            "conversationBytes": conversation_bytes,
                            "conversationLimitBytes": conversation_limit,
                            "nodeBytes": node_bytes,
                            "nodeLimitBytes": node_limit,
                        });
                        self.record_stop(id, reason, usage).await;
                        systemd::stop(id).await?;
                        break 143;
                    }
                }
                result = &mut out, if readers & 1 == 0 => {
                    result.map_err(Error::internal)??;
                    readers |= 1;
                }
                result = &mut err, if readers & 2 == 0 => {
                    result.map_err(Error::internal)??;
                    readers |= 2;
                }
            }
        };
        systemd::stop(id).await?;
        let _ = child.wait().await;
        if readers & 1 == 0 {
            out.await.map_err(Error::internal)??;
        }
        if readers & 2 == 0 {
            err.await.map_err(Error::internal)??;
        }
        let _ = tokio::fs::remove_file(plan_file).await;
        let _ = tokio::fs::remove_dir_all(container).await;
        Ok(code)
    }

    async fn monitor_capacity(
        &self,
        id: &str,
        plan: &Value,
        workspace: u64,
        temporary: u64,
    ) -> Result<u64> {
        let run = text(plan, "runId");
        let mounted = self.mounted.lock().await.get(run).cloned();
        let filesystem = mounted.as_ref().map_or(self.disk_budget(plan), |handle| {
            handle.filesystem.load(std::sync::atomic::Ordering::Acquire)
        });
        let (allowance, notice, negotiated) = {
            let mut capacity = self.capacity.lock().await;
            let (allowance, notice) = capacity
                .observe(id, workspace, temporary, filesystem)
                .unwrap_or((self.disk_budget(plan), None));
            (allowance, notice, capacity.negotiated)
        };
        if let Some(notice) = notice {
            let directory = self.data.join("runs").join(run).join("chat-input");
            crate::nodes::workspace::publish_native_notice(
                &directory,
                &serde_json::to_vec(&notice)?,
            )
            .await?;
            let event = json!({
                "type": "runtime.disk_notice", "text": notice["text"],
                "capacity": { "capturedAt": crate::config::now(), "workspaceBytes": workspace, "temporaryBytes": temporary,
                    "allowanceBytes": allowance, "filesystemBytes": filesystem },
            });
            self.append(id, false, format!("{event}\n").as_bytes())
                .await?;
        }
        if negotiated
            && allowance > filesystem
            && let Some(mounted) = mounted
        {
            let attempt = id.to_owned();
            // The lifecycle lock pins the mount through the metadata commit and
            // resize, while supervision remains responsive to lease/cancellation.
            tokio::spawn(async move {
                let Ok(mut mount) = mounted.mount.try_lock() else {
                    return;
                };
                if let Err(error) = mount.grow(allowance).await {
                    tracing::warn!(
                        attempt,
                        status = error.status,
                        "Native disk growth deferred; data retained"
                    );
                }
            });
        }
        Ok(allowance)
    }

    fn disk_budget(&self, plan: &Value) -> u64 {
        plan["resources"]["diskMiB"]
            .as_u64()
            .unwrap_or(32768)
            .saturating_mul(1_048_576)
    }

    async fn disk_limit(&self) -> Result<u64> {
        self.budget.lock().await["limits"]["diskMiB"]
            .as_u64()
            .map(|limit| limit.saturating_mul(disk::MIB))
            .ok_or_else(|| Error::unavailable("Native node disk budget is unavailable."))
    }

    async fn disk_growth(&self) -> Result<u64> {
        Ok(disk::available(
            self.disk_limit().await?,
            disk::used(&self.data, &self.state).await?,
        ))
    }

    async fn usage(&self) -> Result<Value> {
        let used = disk::used(&self.data, &self.state).await?;
        Ok(json!({
            "memoryMiB": 0,
            "diskMiB": used.div_ceil(disk::MIB),
            "diskBytes": used,
        }))
    }

    /// Ceilings are not preallocated reservations. Charge actual retained
    /// payloads, then bound every additional copy by the remaining shared pool.
    async fn check_preparation_space(&self, plan: &Value) -> Result<()> {
        let run = text(plan, "runId");
        let root = workspace::directory(&self.state, run)?;
        let current = if block::exists(&self.state, run)? {
            0
        } else {
            disk::bytes_if_present(&root.join("data")).await?
        };
        let assigned_base =
            block::exists(&self.state, run)? && root.join("file-owner.json").exists();
        let seed = if assigned_base {
            0
        } else if !root.join("initialized").exists()
            && !root.join("data/.cairn-seed-complete").exists()
        {
            let source = Plan::new(plan.clone())
                .imports()
                .find(|import| import.target == text(plan, "projectRoot"))
                .map(|import| import.source.to_owned())
                .ok_or_else(|| Error::bad("Missing native workspace seed."))?;
            if let Some(archives) = plan["nativeProjectArchives"].as_array() {
                let descriptors: Vec<crate::project_archives::Archive> = archives
                    .iter()
                    .cloned()
                    .map(serde_json::from_value)
                    .collect::<std::result::Result<_, _>>()?;
                let size = plan["resources"]["diskMiB"]
                    .as_u64()
                    .ok_or_else(|| Error::bad("Missing native disk size."))?;
                if let Some(base) = self.bases().lookup(&descriptors, size).await? {
                    base.metadata_bytes
                } else {
                    archives.iter().try_fold(0u64, |bytes, archive| {
                        bytes
                            .checked_add(
                                archive["logicalBytes"]
                                    .as_u64()
                                    .ok_or_else(|| Error::bad("Invalid project archive size."))?,
                            )
                            .ok_or_else(|| Error::too_large("Project archives exceed disk budget."))
                    })?
                }
            } else {
                workspace::bytes(&source).await?
            }
        } else {
            0
        };
        let home = disk::bytes_if_present(&self.data.join("runs").join(run).join("home")).await?;
        let incoming = seed.saturating_add(home);
        let growth = self.disk_growth().await?;
        if incoming > growth {
            crate::project_archives::reclaim_for_foreground(&self.data, incoming - growth, &[])
                .await?;
        }
        if current.saturating_add(incoming) > self.disk_budget(plan)
            || incoming > self.disk_growth().await?
            || disk::used(&self.data, &self.state).await? > self.disk_limit().await?
        {
            return Err(Error::conflict(
                "Retained native work leaves insufficient shared disk budget for this preparation.",
            ));
        }
        Ok(())
    }

    /// Admission is global before it is attempt-scoped: a queued pool waiter
    /// must never retain the guard needed by an unrelated physical close.
    async fn with_live_access<T, F, Fut>(
        &self,
        id: &str,
        requested_run: Value,
        forbidden: &'static str,
        operation: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Self, String, CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        uuid(id)?;
        let pool = disk::lock(&self.state).await?;
        let live_access = {
            let active = self.active.lock().await;
            let attempt = active
                .get(id)
                .ok_or_else(|| Error::conflict("Native attempt is not active."))?;
            if requested_run != attempt.run {
                return Err(Error::forbidden(forbidden));
            }
            attempt.live_access.clone()
        };
        let admitted = live_access.clone().lock_owned().await;
        let (run, stop) = {
            let active = self.active.lock().await;
            let attempt = active
                .get(id)
                .ok_or_else(|| Error::conflict("Native attempt is not active."))?;
            if requested_run != attempt.run
                || !Arc::ptr_eq(&live_access, &attempt.live_access)
                || attempt.stop.is_cancelled()
            {
                return Err(Error::forbidden(forbidden));
            }
            (attempt.run.clone(), attempt.stop.clone())
        };
        let controller = self.clone();
        // A dropped HTTP request must not detach a spawn_blocking filesystem
        // open or buffered write from the guards that close will drain. The
        // handle is never aborted; dropping it only detaches the caller.
        tokio::spawn(async move {
            let _pool = pool;
            let _admitted = admitted;
            if stop.is_cancelled() {
                return Err(Error::forbidden(forbidden));
            }
            operation(controller, run, stop).await
        })
        .await
        .map_err(Error::internal)?
    }

    async fn artifact_snapshot(
        &self,
        id: &str,
        value: Value,
    ) -> Result<(tempfile::NamedTempFile, u64)> {
        let attempt = id.to_owned();
        self.with_live_access(
            id,
            value["runId"].clone(),
            "Wrong native artifact scope.",
            move |controller, run, stop| async move {
                let plan = controller.plan(&attempt).await?;
                let environment = workspace::directory(&controller.state, &run)?;
                let root = environment.join("data");
                let path = Path::new(text(&value, "path"));
                let project_root = Path::new(text(&plan, "projectRoot"));
                let (source, allowed) = if let Ok(relative) = path.strip_prefix(project_root) {
                    (root.join("workspaces").join(relative), root.clone())
                } else if let Ok(relative) = path.strip_prefix(
                    guest_data(&plan, &controller.data)
                        .join("runs")
                        .join(&run)
                        .join("output"),
                ) {
                    (root.join("output").join(relative), root.clone())
                } else if let Ok(relative) = path.strip_prefix("/tmp") {
                    let temporary = environment.join("tmp");
                    (temporary.join(relative), temporary)
                } else {
                    return Err(Error::forbidden(
                        "Native artifacts must be in this conversation's workspace, output directory or /tmp.",
                    ));
                };
                let exports = controller.state.join("exports");
                private_dir(&exports).await?;
                let (snapshot, size) = crate::artifacts::file::snapshot_in(
                    &source,
                    &allowed,
                    &exports,
                    controller.disk_growth().await?,
                )
                .await?;
                if stop.is_cancelled() {
                    return Err(Error::forbidden("Native artifact attempt stopped."));
                }

                Ok((snapshot, size))
            },
        )
        .await
    }

    async fn import_project(&self, id: &str, project: &str, value: Value) -> Result<Value> {
        uuid(project)?;
        let attempt = id.to_owned();
        let project = project.to_owned();
        self.with_live_access(
            id,
            value["runId"].clone(),
            "Wrong native project scope.",
            move |controller, run, stop| async move {
                let plan = controller.plan(&attempt).await?;
                if stop.is_cancelled() {
                    return Err(Error::conflict("Native project import stopped."));
                }
                let source = controller
                    .data
                    .join("runs")
                    .join(&run)
                    .join("workspace")
                    .join(&project);
                let expected = Path::new(text(&plan, "projectRoot")).join(&project);
                if value["source"]
                    != guest_data(&plan, &controller.data)
                        .join("runs")
                        .join(&run)
                        .join("workspace")
                        .join(&project)
                        .to_string_lossy()
                        .as_ref()
                    || value["target"] != expected.to_string_lossy().as_ref()
                {
                    return Err(Error::forbidden("Invalid native project path."));
                }
                let target = workspace::directory(&controller.state, &run)?
                    .join("data/workspaces")
                    .join(&project);
                if !target.exists() {
                    let data = workspace::directory(&controller.state, &run)?.join("data");
                    // Stage on the mounted filesystem so installation is an atomic
                    // rename, and do not walk all lazy files to estimate free space.
                    let staged = tempfile::tempdir_in(&data)?;
                    let received = staged.path().join("project");
                    let remaining = block::free_bytes(&data)?;
                    workspace::copy(
                        &source,
                        &received,
                        remaining.min(controller.disk_growth().await?),
                    )
                    .await?;
                    if stop.is_cancelled() {
                        return Err(Error::conflict("Native project import stopped."));
                    }
                    let mut identity = super::identity::provision(&controller.state, &run).await?;
                    if plan["nativeMode"]["mode"] == "administrator" {
                        identity.host_engine =
                            Some((controller.identity.uid, controller.identity.gid));
                    }
                    workspace::own_tree(&received, identity.uid(), identity.gid()).await?;
                    if stop.is_cancelled() {
                        return Err(Error::conflict("Native project import stopped."));
                    }
                    tokio::fs::rename(received, &target).await?;
                }
                Ok(json!({ "ok": true }))
            },
        )
        .await
    }

    async fn stopped_snapshot(&self, run: &str, force: bool) -> Result<Value> {
        let _disk = disk::lock(&self.state).await?;
        if block::exists(&self.state, run)? {
            let root = workspace::directory(&self.state, run)?;
            if !root.join("block-initialized").exists() {
                return Err(Error::conflict(
                    "Native block initialization must finish before publication.",
                ));
            }
            if !force {
                return crate::nodes::checkpoint::capture_completed(
                    &self.state,
                    run,
                    CancellationToken::new(),
                )
                .await;
            }
            return crate::nodes::checkpoint::capture(
                &self.state,
                run,
                None,
                Arc::default(),
                CancellationToken::new(),
                "",
                storage::checkpoint::Consistency::Filesystem,
            )
            .await;
        }
        Err(Error::conflict("Conversation has no native journal."))
    }

    async fn abort_disk(&self, id: &str) -> Result<()> {
        let run = self
            .active
            .lock()
            .await
            .get(id)
            .map(|attempt| attempt.run.clone());
        if let Some(run) = run {
            let mounted = self.mounted.lock().await.get(&run).cloned();
            if let Some(mounted) = mounted {
                mounted.abort_io().await?;
            }
        }
        Ok(())
    }

    async fn stop_attempt(&self, id: &str) -> Result<()> {
        uuid(id)?;
        // Publish cancellation before waiting for an admission that may itself
        // be waiting for storage. Its waiter observes this durable marker.
        let written = tokio::time::timeout(
            Duration::from_secs(1),
            atomic_write(&self.file(id, "stopped"), b"1"),
        )
        .await;
        if !matches!(written, Ok(Ok(()))) {
            if let Some(attempt) = self.active.lock().await.get(id) {
                attempt.stop.cancel();
            }
            return Err(Error::unavailable(
                "Could not persist the native stop marker.",
            ));
        }
        let _admission = self.admission.lock().await;
        self.stop_admitted(id).await
    }

    async fn stop_admitted(&self, id: &str) -> Result<()> {
        if self.stop.is_cancelled() {
            self.record_stop(id, "controller_shutdown", Value::Null)
                .await;
        }
        let written = tokio::time::timeout(
            Duration::from_secs(1),
            atomic_write(&self.file(id, "stopped"), b"1"),
        )
        .await;
        if !matches!(written, Ok(Ok(()))) {
            if let Some(attempt) = self.active.lock().await.get(id) {
                attempt.stop.cancel();
            }
            return Err(Error::unavailable(
                "Could not persist the native stop marker.",
            ));
        }
        let requested = self.active.lock().await.get(id).and_then(|attempt| {
            if attempt.stop.is_cancelled() {
                return None;
            }
            attempt.stop.cancel();
            Some(attempt.expires)
        });
        if let Some(expires) = requested {
            let reason = if self.expired(id, expires).await {
                "time_limit"
            } else {
                "cancelled"
            };
            self.record_stop(id, reason, Value::Null).await;
        }
        let preparing = self
            .active
            .lock()
            .await
            .get(id)
            .is_some_and(|attempt| !attempt.started);
        if preparing {
            self.abort_disk(id).await?;
        }
        if let Err(error) = systemd::stop(id).await {
            self.abort_disk(id).await?;
            return Err(error);
        }
        self.wait_stopped(id).await?;
        if let Err(error) = super::bases::release_attempt(&self.data, id).await {
            tracing::debug!(
                status = error.status,
                "Stopped native preparation pin cleanup deferred"
            );
        }
        Ok(())
    }

    async fn wait_stopped(&self, id: &str) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(300), async {
            while self.active.lock().await.contains_key(id) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| {
            Error::unavailable("Stopped native checkpoint did not finish before its deadline.")
        })
    }

    /// Match the VM wait protocol: headers and spaces keep relayed waits alive.
    fn wait(&self, id: &str) -> Response {
        let stream = futures_util::stream::unfold(
            (self.clone(), id.to_owned(), false),
            |(controller, id, done)| async move {
                if done {
                    return None;
                }
                // An exit marker alone does not release physical ownership.
                let released = controller
                    .active
                    .try_lock()
                    .is_ok_and(|active| !active.contains_key(&id));
                if released
                    && let Ok(code) = tokio::fs::read_to_string(controller.file(&id, "exit")).await
                {
                    let result = match code.parse::<i32>() {
                        Ok(code) => Ok(Bytes::from(json!({ "StatusCode": code }).to_string())),
                        Err(_) => serde_json::from_str::<Value>(&code)
                            .map(|value| Bytes::from(value.to_string()))
                            .map_err(std::io::Error::other),
                    };
                    return Some((result, (controller, id, true)));
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                let stopped = controller.stop.is_cancelled();
                Some((Ok(Bytes::from_static(b" ")), (controller, id, stopped)))
            },
        );
        (
            [("content-type", "application/json")],
            Body::from_stream(stream),
        )
            .into_response()
    }
}

fn guest_data(plan: &Value, data: &Path) -> PathBuf {
    plan["guestDataRoot"]
        .as_str()
        .map_or_else(|| data.to_owned(), PathBuf::from)
}

fn replace_origin(value: &mut Value, origin: &str) {
    match value {
        Value::String(text) => *text = text.replace(crate::microvm::mcp::ORIGIN, origin),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| replace_origin(value, origin)),
        Value::Object(values) => values
            .values_mut()
            .for_each(|value| replace_origin(value, origin)),
        _ => {}
    }
}

async fn relay(listener: tokio::net::TcpListener, channel: PathBuf, stop: CancellationToken) {
    let _ = crate::microvm::listener::serve(
        || listener.accept(),
        |(mut client, _)| {
            let channel = channel.clone();
            async move {
                if let Ok(mut server) = tokio::net::UnixStream::connect(channel).await {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                }
            }
        },
        32,
        stop,
    )
    .await;
}

#[cfg(test)]
async fn install_home(source: &Path, target: &Path) -> Result<()> {
    let agents = target.join(".agents");
    if let Ok(metadata) = tokio::fs::symlink_metadata(&agents).await {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::bad("Native skill root was redirected."));
        }
        let skills = agents.join("skills");
        if let Ok(metadata) = tokio::fs::symlink_metadata(&skills).await {
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                tokio::fs::remove_dir_all(skills).await?;
            } else {
                tokio::fs::remove_file(skills).await?;
            }
        }
    }
    // The source is generated by the manager. Refuse workload-controlled ancestors.
    let mut pending = vec![(source.to_owned(), target.to_owned())];
    while let Some((source, target)) = pending.pop() {
        let metadata = tokio::fs::symlink_metadata(&source).await?;
        if metadata.file_type().is_symlink() {
            return Err(Error::bad("Native runtime seed contains a symlink."));
        }
        if metadata.is_dir() {
            if let Ok(existing) = tokio::fs::symlink_metadata(&target).await {
                if !existing.is_dir() || existing.file_type().is_symlink() {
                    return Err(Error::bad("Native runtime home was redirected."));
                }
            } else {
                tokio::fs::create_dir(&target).await?;
            }
            let mut entries = tokio::fs::read_dir(source).await?;
            while let Some(entry) = entries.next_entry().await? {
                pending.push((entry.path(), target.join(entry.file_name())));
            }
        } else if metadata.is_file() {
            let bytes = tokio::fs::read(&source).await?;
            if let Ok(existing) = tokio::fs::symlink_metadata(&target).await {
                if existing.is_dir() {
                    return Err(Error::bad(
                        "Native runtime configuration was replaced by a directory.",
                    ));
                }
                tokio::fs::remove_file(&target).await?;
            }
            atomic_write(&target, &bytes).await?;
            let mode = if metadata.permissions().mode() & 0o111 != 0 {
                0o700
            } else {
                0o600
            };
            tokio::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode)).await?;
        }
    }
    Ok(())
}

async fn input(request: Request, limit: usize) -> Result<Value> {
    let bytes = axum::body::to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| Error::bad("Native request exceeds limit."))?;
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(&bytes).map_err(Error::from)
}

async fn handler(State(controller): State<Controller>, request: Request) -> Result<Response> {
    if request.method() == "GET" {
        return route(controller, request).await;
    }
    // A disconnected caller must not release physical ownership while a restore,
    // stop or filesystem mutation is still executing.
    tokio::spawn(route(controller, request))
        .await
        .map_err(Error::internal)?
}

async fn route(controller: Controller, request: Request) -> Result<Response> {
    let path = request.uri().path().to_owned();
    let method = request.method().as_str().to_owned();
    if path == "/health" && method == "GET" {
        return Ok(Json(controller.health().await).into_response());
    }

    // This read-only endpoint is local to the root-private controller socket.
    // Its caller retains storage ownership through the bounded staging write;
    // taking that same lock here would deadlock the caller.
    if path == "/staging-budget" && method == "GET" {
        let started = std::time::Instant::now();
        let available = controller.disk_growth().await;
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_secs(1) || available.is_err() {
            tracing::info!(
                target: "cairn_performance",
                operation = "native_staging_budget",
                elapsed_ms = elapsed.as_millis() as u64,
                status = available.as_ref().map_or_else(|error| error.status, |_| 200),
                "Authoritative native staging budget observation finished"
            );
        }
        return Ok(Json(json!({"availableBytes": available?})).into_response());
    }

    let secret = tokio::fs::read_to_string(controller.data.join("runner-secret")).await?;
    let bearer = crate::nodes::bearer(request.headers()).unwrap_or("");
    if !crate::auth::safe_equal(bearer, secret.trim()) {
        return Err(Error::unauthorized("Invalid native controller credential."));
    }
    let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let value = match (method.as_str(), parts.as_slice()) {
        ("POST", ["disk-growth"]) => {
            let value = input(request, 1_000_000).await?;
            controller
                .capacity
                .lock()
                .await
                .apply(&value, controller.disk_limit().await?);
            json!({ "ready": true })
        }
        ("POST", ["native-bases", "lookup"]) => {
            let value = input(request, 8_000_000).await?;
            let archives: Vec<crate::project_archives::Archive> =
                serde_json::from_value(value["archives"].clone())?;
            let size = value["diskMiB"]
                .as_u64()
                .ok_or_else(|| Error::bad("Missing native base size."))?;
            json!({ "base": controller.bases().lookup(&archives, size).await? })
        }
        ("POST", ["native-bases", "warm"]) => {
            let value = input(request, 8_000_000).await?;
            controller
                .base_warmer
                .enqueue(
                    controller.bases(),
                    value,
                    controller.budget.clone(),
                    controller.stop.clone(),
                )
                .await?;
            json!({ "queued": true })
        }
        ("POST", ["native-mode"]) => controller.change_mode(input(request, 16384).await?).await?,
        ("POST", ["node-budget"]) => {
            let value = input(request, 16384).await?;
            let budget = budget_request(&value)?;
            let _admission = controller.admission.lock().await;
            let previous = controller.budget.lock().await.clone();
            if value != previous {
                // Do not hold the budget mutex while waiting for storage:
                // foreground transfers query health with their disk lease held.
                let _disk = disk::lock(&controller.state).await?;
                let limit = budget.limits.disk_mi_b.saturating_mul(disk::MIB);
                let used = disk::used(&controller.data, &controller.state).await?;
                if let Err(error) = controller
                    .bases()
                    .trim(
                        super::bases::cache_limit(limit),
                        used.saturating_sub(limit),
                        None,
                    )
                    .await
                {
                    tracing::debug!(
                        status = error.status,
                        "Native cache reclamation deferred during budget update"
                    );
                }
                if value["limits"] != previous["limits"] {
                    systemd::apply_budget(&budget).await?;
                }
                atomic_write(
                    &controller.state.join(crate::microvm::budget::FILE),
                    &serde_json::to_vec(&budget_record(&value))?,
                )
                .await?;
                *controller.budget.lock().await = value;
            }
            json!({ "ready": true })
        }
        ("POST", ["storage-policy", "probe"]) => {
            let policy: crate::storage::policy::Policy =
                serde_json::from_value(input(request, 16384).await?)?;
            policy.validate().map_err(Error::bad)?;
            json!({ "ready": true })
        }
        ("POST", ["storage-policy"]) => {
            let configuration: crate::storage::policy::Configuration =
                serde_json::from_value(input(request, 16384).await?)?;
            if !controller
                .storage_policy
                .apply_relief(&controller.state, &configuration)
                .await?
            {
                let _disk = disk::lock(&controller.state).await?;
                controller
                    .storage_policy
                    .apply(&controller.state, configuration)
                    .await?;
            }
            json!({ "ready": true })
        }
        ("POST", ["runs", id, "lease"]) => {
            uuid(id)?;
            let lease = input(request, 1024).await?;
            let ms = lease["remainingMs"]
                .as_u64()
                .filter(|ms| (1..=300_000).contains(ms))
                .ok_or_else(|| Error::bad("Invalid native lease."))?;
            if controller.file(id, "exit").exists() || controller.file(id, "stopped").exists() {
                return Err(Error::conflict("Attempt stopped."));
            }
            let until = crate::nodes::boot_ms().saturating_add(ms);
            super::watchdog::grant(&controller.file(id, "lease"), until).await?;
            controller.leases.lock().await.insert((*id).into(), until);
            json!({ "ok": true })
        }
        ("POST", ["runs", id]) => {
            controller.start(id).await?;
            json!({})
        }
        ("GET", ["runs", id, "bootstrap"]) => {
            uuid(id)?;
            let report = controller.bootstrap.get(id).ok_or_else(|| {
                Error::not_found(
                    "Bootstrap observation is unavailable; inspect retained technical logs.",
                )
            })?;
            json!({ "source": "memory", "bootstrap": report })
        }
        ("DELETE", ["runs", id]) => {
            controller.stop_attempt(id).await?;
            json!({})
        }
        ("POST", ["runs", id, "wait"]) => {
            uuid(id)?;
            return Ok(controller.wait(id));
        }
        ("GET", ["exclusive"]) => controller.inspect_exclusive().await?,
        ("POST", ["exclusive", "acquire"]) => {
            controller
                .acquire_exclusive(serde_json::from_value(input(request, 16384).await?)?)
                .await?
        }
        ("POST", ["exclusive", "release"]) => {
            controller
                .release_exclusive(serde_json::from_value(input(request, 16384).await?)?)
                .await?
        }
        ("GET", ["runs", id, "administrative-pause"]) => {
            controller.administrative_pause(id).await?
        }
        ("GET", ["runs", id, "logs"]) => {
            uuid(id)?;
            if let Some(offset) = crate::runner::logs::offset(request.uri().query())? {
                let path = controller.file(id, "log");
                let (controller, id) = (controller.clone(), (*id).to_owned());
                return crate::runner::logs::response(path, offset, move || {
                    let (controller, id) = (controller.clone(), id.clone());
                    async move {
                        controller
                            .active
                            .try_lock()
                            .is_ok_and(|active| !active.contains_key(&id))
                    }
                })
                .await;
            }
            let (mut writer, reader) = tokio::io::duplex(65536);
            let (controller, id) = (controller.clone(), (*id).to_owned());
            tokio::spawn(async move {
                let Ok(mut file) = tokio::fs::File::open(controller.file(&id, "log")).await else {
                    return;
                };
                let mut offset = 0;
                let mut buffer = vec![0u8; 65536];
                let mut at_line_boundary = true;
                let mut attempt_finished = false;
                let mut last_progress = tokio::time::Instant::now();
                loop {
                    if file.seek(std::io::SeekFrom::Start(offset)).await.is_err() {
                        break;
                    }
                    match file.read(&mut buffer).await {
                        Ok(0) => {
                            if attempt_finished {
                                break;
                            }
                            if controller
                                .active
                                .try_lock()
                                .is_ok_and(|active| !active.contains_key(&id))
                            {
                                // The final append can land between the EOF read and
                                // retirement observation. Drain again after retirement.
                                attempt_finished = true;
                                continue;
                            }
                            // Idle transport frames must not split a log record
                            // whose remaining bytes have not been appended yet.
                            if at_line_boundary && last_progress.elapsed() >= Duration::from_secs(1)
                            {
                                if writer
                                    .write_all(b"{\"type\":\"heartbeat\"}\n")
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                last_progress = tokio::time::Instant::now();
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Ok(count) => {
                            if writer.write_all(&buffer[..count]).await.is_err() {
                                break;
                            }
                            at_line_boundary = buffer[count - 1] == b'\n';
                            last_progress = tokio::time::Instant::now();
                            offset += count as u64;
                        }
                        Err(_) => break,
                    }
                }
            });
            return Ok((
                [("content-type", "application/x-ndjson")],
                Body::from_stream(tokio_util::io::ReaderStream::new(reader)),
            )
                .into_response());
        }
        ("POST", ["disks", run, "storage-status"]) => {
            uuid(run)?;
            block::status(&controller.state, run).await?
        }
        ("POST", ["disks", run, "published"]) => {
            uuid(run)?;
            let receipt = input(request, 16384).await?;
            block::published(&controller.state, run, &receipt).await?
        }
        ("POST", ["disks", run, "publication-fence"]) => {
            uuid(run)?;
            let receipt = input(request, 16384).await?;
            let publication = text(&receipt, "backupId");
            uuid(publication)?;
            crate::storage::runtime::fence_publication(&controller.state, run, publication).await?;
            json!({"fenced":publication})
        }
        ("POST", ["disks", run, "snapshot-completed"]) => {
            uuid(run)?;
            controller.stopped_snapshot(run, false).await?
        }
        ("POST", ["disks", run, "snapshot"]) => {
            uuid(run)?;
            controller.stopped_snapshot(run, true).await?
        }
        ("POST", ["runs", id, "snapshot"]) => {
            uuid(id)?;
            let active = {
                controller
                    .active
                    .lock()
                    .await
                    .get(*id)
                    .map(|attempt| (attempt.run.clone(), attempt.stop.clone()))
            };
            if let Some((run, stop)) = active
                && block::exists(&controller.state, &run)?
            {
                let value = input(request, 16384).await?;
                let mounted = controller
                    .mounted
                    .lock()
                    .await
                    .get(&run)
                    .cloned()
                    .ok_or_else(|| Error::conflict("Native filesystem is not mounted."))?;
                return Ok(Json(
                    block::capture_active(
                        &controller.state,
                        &run,
                        mounted,
                        value["consistency"] != "crash",
                        if value["consistency"] == "crash" {
                            CancellationToken::new()
                        } else {
                            stop
                        },
                    )
                    .await?,
                )
                .into_response());
            }
            if controller.active.lock().await.contains_key(*id) {
                return Err(Error::conflict(
                    "Native checkpoints require the workload to stop.",
                ));
            }
            let run = tokio::fs::read_to_string(controller.file(id, "run")).await?;
            controller.stopped_snapshot(&run, false).await?
        }
        (_, ["snapshots", snapshot, operation]) => {
            uuid(snapshot)?;
            let directory = controller.state.join("snapshots").join(snapshot);
            match (method.as_str(), *operation) {
                ("POST", "direct-reader") => {
                    let value = input(request, 4096).await?;
                    return Ok(
                        snapshots::direct_reader(&directory, &serde_json::to_vec(&value)?)
                            .await?
                            .into_response(),
                    );
                }
                ("DELETE", "direct-reader") => {
                    snapshots::close_direct_reader(&directory).await?;
                    json!({ "closed": true })
                }
                ("POST", "direct-upload") => {
                    let value = input(request, 131_072).await?;
                    return Ok(Json(
                        crate::nodes::snapshots::upload_direct(
                            &directory,
                            &serde_json::to_vec(&value)?,
                        )
                        .await?,
                    )
                    .into_response());
                }
                ("DELETE", "discard") => {
                    snapshots::discard(&directory).await?;
                    json!({ "ok": true })
                }
                ("POST", "blocks" | "publication") => {
                    let value = input(request, snapshots::MAX_MANIFEST_BYTES).await?;
                    let hashes = serde_json::from_value(value["hashes"].clone())?;
                    let diagnostic = value
                        .get("diagnostic")
                        .and_then(|value| serde_json::from_value(value.clone()).ok());
                    let (length, body) = snapshots::served_publication_observed(
                        &directory, hashes, diagnostic, snapshot,
                    )
                    .await?;
                    return Ok(([("content-length", length.to_string())], body).into_response());
                }
                ("GET", hash) => {
                    return Ok(snapshots::served(&directory, hash).await?.into_response());
                }
                _ => {
                    return Err(Error::method_not_allowed(
                        "Invalid native snapshot operation.",
                    ));
                }
            }
        }
        ("POST", ["disks", run, "evict"]) => {
            let _disk = disk::lock(&controller.state).await?;
            uuid(run)?;
            if controller
                .active
                .lock()
                .await
                .values()
                .any(|attempt| attempt.run == *run)
            {
                return Err(Error::conflict(
                    "Native execution must finish stopping before cache eviction.",
                ));
            }
            let expected = input(request, 16384).await?;
            block::evict(&controller.data, &controller.state, run, &expected).await?
        }
        ("POST", ["disks", run, "restore"]) => {
            let _disk = disk::lock(&controller.state).await?;
            uuid(run)?;
            let request = input(request, snapshots::MAX_MANIFEST_BYTES).await?;
            block::restore(&controller.state, run, request).await?
        }
        ("POST", ["disks", run, action @ ("delete" | "prune")]) => {
            let _disk = disk::lock(&controller.state).await?;
            uuid(run)?;
            let root = workspace::directory(&controller.state, run)?;
            private_dir(&root).await?;
            let _owner = crate::file_lock::exclusive(
                &root.join("owner.lock"),
                "Native workspace is active.",
            )?;
            if root.join("block-restore.pending.json").exists() && *action == "prune" {
                return Err(Error::conflict(
                    "Finish native restore before pruning retained local work.",
                ));
            }
            let retired_grants = block::action(&controller.state, run, *action == "delete").await?;
            let mut entries = tokio::fs::read_dir(&root).await?;
            while let Some(entry) = entries.next_entry().await? {
                if entry.file_name().to_string_lossy().starts_with("stale-")
                    && entry.file_type().await?.is_dir()
                {
                    tokio::fs::remove_dir_all(entry.path()).await?;
                }
            }
            if *action == "delete" {
                for entry in [
                    "data",
                    "docker-runtime",
                    "initialized",
                    "resources.json",
                    "attempt",
                    "runtime.json",
                    "block-initialized",
                    "file-owner.json",
                    "block-seeding",
                    "block-restore.pending.json",
                    "tmp",
                ] {
                    let path = root.join(entry);
                    if let Ok(meta) = tokio::fs::symlink_metadata(&path).await {
                        if meta.is_dir() && !meta.file_type().is_symlink() {
                            tokio::fs::remove_dir_all(path).await?;
                        } else {
                            tokio::fs::remove_file(path).await?;
                        }
                    }
                }
            }
            if *action == "delete" {
                controller.bootstrap.forget(run);
                let stage = controller.data.join("runs").join(run);
                if stage.exists() {
                    tokio::fs::remove_dir_all(stage).await?;
                }
                let mut entries = tokio::fs::read_dir(&controller.state).await?;
                while let Some(entry) = entries.next_entry().await? {
                    if entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "run")
                        && tokio::fs::read_to_string(entry.path())
                            .await
                            .is_ok_and(|owner| owner == *run)
                    {
                        let id = entry
                            .path()
                            .file_stem()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned();
                        uuid(&id)?;
                        for extension in ["log", "lease"] {
                            let _ = tokio::fs::remove_file(controller.file(&id, extension)).await;
                        }
                        let _ = tokio::fs::remove_file(
                            controller
                                .state
                                .join("roots")
                                .join(&id)
                                .with_extension("plan.json"),
                        )
                        .await;
                        let _ = tokio::fs::remove_dir_all(controller.state.join("roots").join(&id))
                            .await;
                        let _ = tokio::fs::remove_file(
                            controller
                                .data
                                .join("runner-plans")
                                .join(format!("{id}.json")),
                        )
                        .await;
                    }
                }
            }
            json!({ "deleted": *action == "delete", "pruned": *action == "prune", "retiredGrants": retired_grants })
        }
        ("POST", ["runs", id, "artifact"]) => {
            let value = input(request, 16384).await?;
            let (snapshot, size) = controller.artifact_snapshot(id, value).await?;
            let file = tokio::fs::File::from_std(snapshot.reopen()?);
            let stream = futures_util::stream::try_unfold(
                (file, snapshot),
                |(mut file, snapshot)| async move {
                    let mut bytes = vec![0; 65536];
                    let count = file.read(&mut bytes).await?;
                    bytes.truncate(count);
                    Ok::<_, std::io::Error>((count > 0).then_some((bytes, (file, snapshot))))
                },
            );
            return Ok((
                [("content-length", size.to_string())],
                Body::from_stream(stream),
            )
                .into_response());
        }
        ("POST", ["runs", id, "projects", project]) => {
            let value = input(request, 16384).await?;
            controller.import_project(id, project, value).await?
        }
        _ => {
            return Err(Error::not_found(
                "Native controller operation is not supported.",
            ));
        }
    };
    Ok(Json(value).into_response())
}

// Called only after old workloads and block devices are fenced. Temporary
// anonymous bases/clones have no authority; conventional committed disks remain.
async fn recover_preparations(data: &Path, state: &Path) -> Result<()> {
    storage::environment::recover(state).await?;
    super::bases::recover_pins(data).await
}

async fn bind_controller(path: &Path) -> Result<tokio::net::UnixListener> {
    if let Ok(metadata) = tokio::fs::symlink_metadata(path).await {
        if !metadata.file_type().is_socket() {
            return Err(Error::bad(
                "Native controller socket path is occupied by another file.",
            ));
        }
        tokio::fs::remove_file(path).await?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    Ok(listener)
}

pub async fn serve(stop: CancellationToken) -> Result<()> {
    super::limits::configure()?;
    let data = crate::nodes::node_data_dir();
    let state = super::state_dir();
    private_dir(&state).await?;
    if !state.is_absolute() || tokio::fs::canonicalize(&state).await? != state {
        return Err(Error::bad(
            "Native state must use a canonical absolute directory.",
        ));
    }
    let _lock = crate::file_lock::exclusive(
        &state.join("controller.lock"),
        "Another native controller owns this storage.",
    )?;
    disk::recover_exports(&state).await?;
    atomic_write(
        &state.join("native-accounting.json"),
        &serde_json::to_vec(&json!({ "data": data }))?,
    )
    .await?;
    let identity = Arc::new(systemd::identity().await?);
    private_dir(&state.join("roots")).await?;
    let mut interrupted = Vec::new();
    let mut entries = tokio::fs::read_dir(&state).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "active")
        {
            let attempt = entry
                .path()
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            uuid(&attempt)?;
            let run = tokio::fs::read_to_string(entry.path()).await?;
            uuid(&run)?;
            systemd::request_stop(&attempt).await?;
            interrupted.push((attempt, run, entry.path()));
        }
    }
    // Host mounts survive controller death. Recovery aborts only verified dead
    // backends to release blocked I/O, confirms fencing, then unmounts. Complete
    // all other attributed fences before retiring any kernel device.
    block::recover_host_mounts(&state).await?;
    for (attempt, _, _) in &interrupted {
        systemd::stop(attempt).await?;
        super::suspension::recover(&state, attempt).await?;
    }

    {
        let state = state.clone();
        tokio::task::spawn_blocking(move || storage::ublk::cleanup_stale(&state))
            .await
            .map_err(Error::internal)??;
    }
    for (attempt, run, marker) in interrupted {
        block::recover_completed(&state, &run, &attempt).await?;
        preserve_interrupted_exit(&marker).await?;
        tokio::fs::remove_file(marker).await?;
    }
    recover_preparations(&data, &state).await?;
    let backend_namespace = storage::ublk::BackendNamespace::create(&state)?;
    let budget_path = state.join(crate::microvm::budget::FILE);
    let checked = load_budget(&budget_path).await?;
    let budget = serde_json::to_value(&checked)?;
    systemd::apply_budget(&checked).await?;
    atomic_write(&budget_path, &serde_json::to_vec(&budget_record(&budget))?).await?;
    let mut mode = mode::load(&state).await?;
    if mode["status"] == "applied"
        && let Err(error) = mode::sudo(&identity, mode["mode"] == "administrator").await
    {
        mode["status"] = "failed".into();
        mode["error"] = error.message.into();
        atomic_write(&state.join(mode::FILE), &serde_json::to_vec(&mode)?).await?;
    }
    let controller = Controller {
        bootstrap: bootstrap::Registry::new(
            crate::nodes::component_runtime(),
            crate::recovery::process_identity(std::process::id())
                .await
                .ok()
                .flatten()
                .map(|identity| identity.boot),
        ),
        storage_policy: crate::storage::policy::Control::default(),
        data,
        state,
        active: Arc::default(),
        capacity: Arc::default(),
        mounted: Arc::default(),
        leases: Arc::default(),
        admission: Arc::default(),
        exclusive: Arc::default(),
        base_warmer: Arc::default(),
        budget: Arc::new(Mutex::new(budget)),
        mode: Arc::new(Mutex::new(mode)),
        changed: Arc::default(),
        log_lock: Arc::default(),
        telemetry: health::Telemetry::default(),
        stop: stop.clone(),
        identity,
        backend_namespace,
    };
    controller.recover_exclusive().await?;
    let exclusive_monitor = tokio::spawn(controller.clone().monitor_exclusive());
    let telemetry_monitor = tokio::spawn(controller.clone().sample_health());
    let socket = controller.state.join("controller.sock");
    let listener = bind_controller(&socket).await?;
    let router = Router::new()
        .fallback(any(handler))
        .with_state(controller.clone());
    let server_stop = stop.clone();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(server_stop.cancelled_owned())
            .await
    });
    let result = tokio::select! {
        () = stop.cancelled() => None,
        result = &mut server => { stop.cancel(); Some(result) }
    };
    let active: Vec<_> = controller.active.lock().await.keys().cloned().collect();
    for id in active {
        controller.stop_attempt(&id).await?;
    }
    while !controller.active.lock().await.is_empty() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    exclusive_monitor.await.map_err(Error::internal)?;
    telemetry_monitor.abort();
    let _ = telemetry_monitor.await;
    controller.base_warmer.wait_idle().await;
    match result {
        Some(result) => result.map_err(Error::internal)??,
        None => server.await.map_err(Error::internal)??,
    }
    tokio::fs::remove_file(socket).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_budget_validates_persisted_protocol_and_keeps_fresh_bootstrap() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(crate::microvm::budget::FILE);
        assert_eq!(load_budget(&path).await.unwrap().slots, 1);
        let request = json!({
            "slots": 6,
            "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 },
        });
        assert_eq!(budget_request(&request).unwrap().slots, 6);
        for protocol in [None, Some(json!(null)), Some(json!(2)), Some(json!("1"))] {
            let mut record = request.clone();
            if let Some(protocol) = protocol {
                record["nativeModeProtocol"] = protocol;
            }
            let bytes = serde_json::to_vec(&record).unwrap();
            atomic_write(&path, &bytes).await.unwrap();
            let error = load_budget(&path).await.unwrap_err();
            assert!(error.message.contains("native mode storage protocol"));
            assert_eq!(tokio::fs::read(&path).await.unwrap(), bytes);
        }
        atomic_write(
            &path,
            &serde_json::to_vec(&budget_record(&request)).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(load_budget(&path).await.unwrap().slots, 6);
    }

    #[tokio::test]
    async fn native_startup_reclaims_interrupted_preparations_and_preserves_committed_disks() {
        let (_fixture, controller) = fixture().await;
        let run = crate::config::id();
        let attempt = crate::config::id();
        let disk = controller.state.join("disks").join(&run);
        let abandoned = controller
            .state
            .join("environments")
            .join(crate::config::id());
        private_dir(&disk.join("lazy")).await.unwrap();
        atomic_write(&disk.join("lazy/preserved"), b"committed payload")
            .await
            .unwrap();
        private_dir(&abandoned.join("lazy")).await.unwrap();
        atomic_write(&abandoned.join("lazy/partial"), b"unclaimed")
            .await
            .unwrap();
        let stage = controller.data.join("runs").join(&run);
        private_dir(&stage).await.unwrap();
        let pin = super::super::bases::pin_path(&stage, &attempt).unwrap();
        atomic_write(&pin, b"").await.unwrap();
        recover_preparations(&controller.data, &controller.state)
            .await
            .unwrap();
        assert!(!abandoned.exists());
        assert!(!pin.exists());
        assert_eq!(
            storage::environment::directory(&controller.state, &run).unwrap(),
            disk
        );
        assert_eq!(
            tokio::fs::read(disk.join("lazy/preserved")).await.unwrap(),
            b"committed payload"
        );
    }

    #[tokio::test]
    async fn optional_base_limits_do_not_reject_valid_zip_preparation() {
        let (_fixture, controller) = fixture().await;
        let run = crate::config::id();
        let project_root = controller.data.join("runs").join(&run).join("workspace");
        let archive = json!({
            "projectId": crate::config::id(),
            "revision": "a".repeat(40),
            "key": "b".repeat(64),
            "hash": "c".repeat(64),
            "bytes": 100,
            "logicalBytes": 15
        });
        for (size, count) in [(2_097_152, 1), (128, 65)] {
            let mut archives = Vec::new();
            for _ in 0..count {
                let mut value = archive.clone();
                value["projectId"] = crate::config::id().into();
                archives.push(value);
            }
            let plan = json!({
                "id": crate::config::id(),
                "runId": run,
                "projectRoot": project_root,
                "resources": { "cpu": 1, "memoryMiB": 512, "diskMiB": size },
                "imports": [{ "source": project_root, "target": project_root }],
                "nativeProjectArchives": archives
            });
            controller.check_preparation_space(&plan).await.unwrap();
            assert!(
                !controller
                    .bases()
                    .initialize(&plan, &CancellationToken::new())
                    .await
                    .unwrap()
            );
        }
    }

    #[tokio::test]
    async fn budget_update_does_not_block_budget_reads_while_waiting_for_storage() {
        let (_fixture, controller) = fixture().await;
        let owner =
            crate::file_lock::exclusive(&controller.state.join("storage.lock"), "fixture owner")
                .unwrap();
        let mut value = controller.budget.lock().await.clone();
        value["slots"] = 2.into();
        let updating = controller.clone();
        let pending =
            tokio::spawn(async move { route(updating, request("/node-budget", &value)).await });
        let waiting = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if crate::file_lock::exclusive(
                    &controller.state.join("storage-foreground.lock"),
                    "priority probe",
                )
                .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .is_ok();
        let readable = controller.budget.try_lock().is_ok();
        // Abort this direct route before releasing the fixture disk; no real
        // systemd budget operation is admitted by this concurrency test.
        pending.abort();
        let _ = pending.await;
        drop(owner);
        assert!(waiting);
        assert!(readable);
    }

    #[tokio::test]
    async fn mixed_storage_policy_changes_still_wait_for_the_disk_gate() {
        use crate::storage::policy::{Configuration, Policy};

        let (_fixture, controller) = fixture().await;
        let first = Configuration {
            generation: 1,
            policy: Policy::default(),
        };
        controller
            .storage_policy
            .apply(&controller.state, first.clone())
            .await
            .unwrap();
        let owner = disk::lock(&controller.state).await.unwrap();
        let mixed = Configuration {
            generation: 2,
            policy: Policy {
                reserve_mi_b: 64,
                memory_cache_mi_b: 128,
                ..first.policy.clone()
            },
        };
        let updating = controller.clone();
        let pending = tokio::spawn(async move {
            route(updating, request("/storage-policy", &json!(mixed))).await
        });
        let waiting = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if crate::file_lock::exclusive(
                    &controller.state.join("storage-foreground.lock"),
                    "priority probe",
                )
                .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .is_ok();
        let current = controller.storage_policy.current().await;
        // A queued general update must not retain Control's mutex and prevent
        // a later committed reserve reduction from reaching blocked writers.
        let relief = Configuration {
            generation: 3,
            policy: Policy {
                reserve_mi_b: 64,
                ..first.policy.clone()
            },
        };
        let applied = tokio::time::timeout(
            Duration::from_secs(2),
            route(
                controller.clone(),
                request("/storage-policy", &json!(relief)),
            ),
        )
        .await;
        drop(owner);
        let result = tokio::time::timeout(Duration::from_secs(2), pending).await;
        assert!(waiting);
        assert_eq!(current, Some(first));
        applied.unwrap().unwrap();
        result.unwrap().unwrap().unwrap();
        assert_eq!(controller.storage_policy.current().await, Some(relief));
    }

    #[test]
    fn storage_policy_relief_reaches_a_writer_while_the_disk_gate_is_held() {
        // Other storage tests deliberately hold process-global write admission.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native::service::tests::storage_policy_relief_child",
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
    #[ignore = "isolated process-global write admission and pressure relief child"]
    async fn storage_policy_relief_child() {
        use crate::storage::{
            Disk,
            policy::{Configuration, Policy},
        };

        let (_fixture, controller) = fixture().await;
        let directory = controller
            .state
            .join("environments")
            .join(crate::config::id());
        let policy = Policy {
            reserve_mi_b: 64,
            reserve_percent: 1,
            ..Policy::default()
        };
        drop(
            storage::runtime::create_prepared(
                &directory.join("lazy"),
                &json!({
                    "version": 1,
                    "size": 4096,
                    "blockSize": 4194304,
                    "blocks": [{ "offset": 0, "size": 4096, "hash": null }]
                }),
                &json!({ "unassigned": true, "policy": policy }),
            )
            .await
            .unwrap(),
        );
        let volume = storage::runtime::load(&directory).await.unwrap();
        let blocked = Configuration {
            generation: 1,
            policy: Policy {
                reserve_mi_b: 16 * 1024 * 1024,
                ..policy.clone()
            },
        };
        route(
            controller.clone(),
            request("/storage-policy", &json!(blocked)),
        )
        .await
        .unwrap();

        // Filesystem close holds this gate while syncfs waits for outstanding
        // block writes. Keep its ownership until the route has relieved I/O.
        let owner = disk::lock(&controller.state).await.unwrap();
        let writing = volume.clone();
        let mut writer = tokio::task::spawn_blocking(move || writing.write_at(0, b"preserved"));
        let pressure = tokio::time::timeout(Duration::from_secs(5), async {
            while volume.status().unwrap()["performance"]["writeAdmission"]["inFlight"] != 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let blocked = !writer.is_finished();

        let relief = Configuration {
            generation: 2,
            policy,
        };
        let updating = controller.clone();
        let value = json!(relief);
        let mut applying =
            tokio::spawn(async move { route(updating, request("/storage-policy", &value)).await });
        let applied = tokio::time::timeout(Duration::from_secs(2), &mut applying).await;
        // A broken route must fail without retaining a blocked writer/thread.
        if applied.is_err() {
            applying.abort();
            let _ = applying.await;
            controller
                .storage_policy
                .apply(&controller.state, relief.clone())
                .await
                .unwrap();
        }
        let written = tokio::time::timeout(Duration::from_secs(5), &mut writer).await;
        assert!(
            crate::file_lock::exclusive(&controller.state.join("storage.lock"), "other owner")
                .is_err()
        );
        drop(owner);
        pressure.unwrap();
        assert!(blocked, "the reserve must still protect the journal");
        applied.unwrap().unwrap().unwrap();
        written.unwrap().unwrap().unwrap();
        assert_eq!(controller.storage_policy.current().await, Some(relief));
        let mut bytes = [0; 9];
        volume.read_at(0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"preserved");
    }

    pub(super) async fn fixture() -> (tempfile::TempDir, Controller) {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let state = root.path().join("state");
        private_dir(&data).await.unwrap();
        private_dir(&state).await.unwrap();
        atomic_write(
            &data.join("runner-secret"),
            b"synthetic-private-controller-token",
        )
        .await
        .unwrap();
        let controller = Controller {
            bootstrap: bootstrap::Registry::new(
                "fixture-runtime".into(),
                Some(crate::config::id()),
            ),
            storage_policy: crate::storage::policy::Control::default(),
            data,
            state,
            active: Arc::default(),
            capacity: Arc::default(),
            mounted: Arc::default(),
            leases: Arc::default(),
            admission: Arc::default(),
            exclusive: Arc::default(),
            base_warmer: Arc::default(),
            mode: Arc::new(Mutex::new(
                json!({ "mode": "protected", "generation": null, "status": "applied" }),
            )),
            budget: Arc::new(Mutex::new(
                json!({ "slots": 1, "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 } }),
            )),
            changed: Arc::default(),
            log_lock: Arc::default(),
            telemetry: health::Telemetry::default(),
            stop: CancellationToken::new(),
            backend_namespace: storage::ublk::BackendNamespace::unavailable_for_test(),
            identity: Arc::new(systemd::Identity {
                user: "synthetic-workload".into(),
                uid: 65534,
                gid: 65534,
            }),
        };
        (root, controller)
    }

    #[tokio::test]
    async fn bootstrap_phase_is_visible_while_real_admission_waits() {
        let (_root, controller) = fixture().await;
        let id = crate::config::id();
        let admission = controller.admission.lock().await;
        let (starting, attempt) = (controller.clone(), id.clone());
        let start = tokio::spawn(async move { starting.start(&attempt).await });
        tokio::task::yield_now().await;

        let response = tokio::time::timeout(
            Duration::from_secs(1),
            handler(
                State(controller.clone()),
                Request::builder()
                    .uri(format!("/runs/{id}/bootstrap"))
                    .header("authorization", "Bearer synthetic-private-controller-token")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("bootstrap observation cannot wait for admission")
        .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 32_768)
            .await
            .unwrap();
        let health = tokio::time::timeout(Duration::from_secs(1), controller.health())
            .await
            .expect("observation cannot make health wait for admission");
        start.abort();
        let _ = start.await;
        drop(admission);

        assert_eq!(status, 200, "missing bootstrap observation: {body:?}");
        let report: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(report["bootstrap"]["attemptId"], id);
        assert_eq!(report["bootstrap"]["status"], "preparing");
        assert_eq!(report["bootstrap"]["phase"], "admission_wait");
        assert_eq!(health["activeRuns"], 0);
    }

    #[tokio::test]
    async fn bootstrap_lookup_requires_authentication_and_reports_retention_loss() {
        let (_root, controller) = fixture().await;
        let id = crate::config::id();
        let observation = controller.bootstrap.begin(&id);
        observation.context(&crate::config::id(), Some("fixture-runtime"));
        observation.submitted();
        let unauthorized = route(
            controller.clone(),
            Request::builder()
                .uri(format!("/runs/{id}/bootstrap"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(unauthorized.status, 401);
        let unknown = route(
            controller.clone(),
            Request::builder()
                .uri(format!("/runs/{}/bootstrap", crate::config::id()))
                .header("authorization", "Bearer synthetic-private-controller-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(unknown.status, 404);
        assert!(controller.bootstrap.get(&id).is_some());
    }

    #[tokio::test]
    async fn actual_home_reconciliation_exposes_its_held_phase_without_blocking_controls() {
        let module = module_path!().split_once("::").unwrap().1;
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                &format!("{module}::held_home_reconciliation_child"),
                "--ignored",
            ])
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(15), child.output())
            .await
            .expect("isolated blocked-I/O regression must finish")
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed;"));
    }

    #[tokio::test]
    #[ignore = "isolated FIFO regression, executed by its parent"]
    async fn held_home_reconciliation_child() {
        use std::io::Write;
        use std::os::unix::fs::MetadataExt;

        let (root, controller) = fixture().await;
        let data = root.path().join("durable");
        let source = root.path().join("staging");
        private_dir(&data).await.unwrap();
        private_dir(&source).await.unwrap();
        let metadata = std::fs::metadata(&data).unwrap();
        let identity = super::super::identity::Identity {
            user: "fixture".into(),
            root_uid: metadata.uid().saturating_add(131072),
            root_gid: metadata.gid().saturating_add(131072),
            subuid: 100000,
            subgid: 200000,
            host_engine: Some((metadata.uid(), metadata.gid())),
        };
        std::fs::write(
            data.join(".cairn-identity.json"),
            serde_json::to_vec(&identity).unwrap(),
        )
        .unwrap();
        let path = data.join(".cairn-identity.transition.json");
        let fifo = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let transition = serde_json::to_vec(&json!({
            "previous": &identity,
            "target": &identity,
        }))
        .unwrap();
        let (opened, opening) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            opened.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
            file.write_all(&transition).unwrap();
        });
        let id = crate::config::id();
        let scope = controller.bootstrap.begin(&id);
        let trace = scope.trace();
        let preparing = tokio::spawn(async move {
            super::super::home::prepare_for_identity(&source, &data, &identity, &trace).await
        });
        let arrived =
            tokio::task::spawn_blocking(move || opening.recv_timeout(Duration::from_secs(3)))
                .await
                .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(1), controller.health()).await;
        let phase = controller.bootstrap.get(&id).unwrap();
        let lease = tokio::time::timeout(
            Duration::from_secs(1),
            handler(
                State(controller.clone()),
                request(
                    &format!("/runs/{id}/lease"),
                    &json!({ "remainingMs": 60_000 }),
                ),
            ),
        )
        .await;
        release.send(()).unwrap();
        writer.join().unwrap();
        preparing.await.unwrap().unwrap();
        arrived.expect("the real reconciler must reach the held metadata read");
        response.expect("health must remain responsive during actual reconciliation");
        lease
            .expect("lease renewal must remain responsive")
            .unwrap();
        assert!(controller.leased(&id).await);
        let phase = serde_json::to_value(phase).unwrap();
        assert_eq!(phase["phase"], "identity_reconcile");
        assert_eq!(phase["status"], "preparing");
        assert_eq!(controller.health().await["activeRuns"], 0);
        drop(scope);
    }

    async fn busy_disk_fixture(controller: &Controller) -> (String, String, Value) {
        let run = crate::config::id();
        let id = crate::config::id();
        let root = workspace::directory(&controller.state, &run).unwrap();
        private_dir(&root).await.unwrap();
        // Stop before any privileged mount after ownership becomes available.
        // This permanent missing-journal conflict must never be retried.
        atomic_write(&root.join("initialized"), b"1").await.unwrap();
        atomic_write(&root.join("block-initialized"), b"1")
            .await
            .unwrap();
        atomic_write(&controller.file(&id, "log"), b"")
            .await
            .unwrap();
        let plan = json!({
            "runId": run,
            "resources": { "diskMiB": 128 },
        });
        controller
            .leases
            .lock()
            .await
            .insert(id.clone(), crate::nodes::boot_ms() + 60_000);
        (run, id, plan)
    }

    #[tokio::test]
    async fn start_path_observes_deadline_and_cancellation_before_storage_admission() {
        let (_fixture, controller) = fixture().await;
        let (run, id, _) = busy_disk_fixture(&controller).await;
        let stage = controller.data.join("runs").join(&run);
        for directory in [
            stage.join("workspace"),
            stage.join("home"),
            controller.data.join("runner-plans"),
        ] {
            private_dir(&directory).await.unwrap();
        }
        let mut plan = json!({
            "id": id,
            "runId": run,
            "backend": "native",
            "nodeLeaseRequired": true,
            "cwd": stage.join("workspace"),
            "projectRoot": stage.join("workspace"),
            "sandbox": "workspace-write",
            "args": [],
            "prompt": "fixture",
            "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
            "imports": [
                { "source": stage.join("workspace"), "target": stage.join("workspace") },
                { "source": stage.join("home"), "target": "/home/node" }
            ],
        });
        let now = Arc::new(std::sync::atomic::AtomicI64::new(crate::config::now()));
        let expires = now.load(std::sync::atomic::Ordering::SeqCst) + 1000;
        plan["expires"] = expires.into();
        atomic_write(
            &controller
                .data
                .join("runner-plans")
                .join(format!("{id}.json")),
            plan.to_string().as_bytes(),
        )
        .await
        .unwrap();
        // Hold storage without foreground intent, so a foreground lock proves
        // start has validated the plan and reached its disk admission wait.
        let held =
            crate::file_lock::exclusive(&controller.state.join("storage.lock"), "fixture owner")
                .unwrap();
        controller
            .leases
            .lock()
            .await
            .insert(id.clone(), crate::nodes::boot_ms() + 10_000);
        let error = tokio::time::timeout(Duration::from_secs(5), async {
            let starting = crate::config::TEST_NOW.scope(now.clone(), controller.start(&id));
            tokio::pin!(starting);
            tokio::select! {
                result = &mut starting => result,
                () = async {
                    while crate::file_lock::exclusive(
                        &controller.state.join("storage-foreground.lock"),
                        "fixture admission probe",
                    )
                    .is_ok()
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                } => {
                    // Fixture fsyncs cannot consume the execution deadline.
                    // Advance only start's wall clock once it waits on disk.
                    now.store(expires, std::sync::atomic::Ordering::SeqCst);
                    starting.await
                }
            }
        })
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.status, 408, "{}", error.message);
        let saved: Value = serde_json::from_slice(
            &tokio::fs::read(controller.file(&id, "stop-reason.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(saved["reason"], "time_limit");
        assert!(controller.active.lock().await.is_empty());

        let id = crate::config::id();
        plan["id"] = id.clone().into();
        plan["expires"] = Value::Null;
        atomic_write(
            &controller
                .data
                .join("runner-plans")
                .join(format!("{id}.json")),
            plan.to_string().as_bytes(),
        )
        .await
        .unwrap();
        controller
            .leases
            .lock()
            .await
            .insert(id.clone(), crate::nodes::boot_ms() + 10_000);
        let starting = tokio::spawn({
            let controller = controller.clone();
            let id = id.clone();
            async move { controller.start(&id).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while controller.admission.try_lock().is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // DELETE publishes this marker before it waits for admission/fencing.
        atomic_write(&controller.file(&id, "stopped"), b"1")
            .await
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), starting)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.is_conflict());
        assert!(controller.admission.try_lock().is_ok());
        assert!(controller.active.lock().await.is_empty());
        drop(held);
    }

    #[tokio::test]
    async fn native_disk_admission_waits_without_holding_the_global_storage_lock() {
        let (_fixture, controller) = fixture().await;
        let (run, id, plan) = busy_disk_fixture(&controller).await;
        let held = storage::environment::ownership(&controller.state, &run, "fixture owner")
            .await
            .unwrap();
        let waiting = tokio::spawn({
            let controller = controller.clone();
            let id = id.clone();
            async move {
                controller
                    .mount_after_release(
                        &id,
                        &plan,
                        &CancellationToken::new(),
                        Duration::from_secs(5),
                    )
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if tokio::fs::metadata(controller.file(&id, "log"))
                    .await
                    .unwrap()
                    .len()
                    > 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!waiting.is_finished());
        // Shutdown of the previous writer must be able to take this lock.
        let global = tokio::time::timeout(Duration::from_secs(1), disk::lock(&controller.state))
            .await
            .unwrap()
            .unwrap();
        drop(global);
        drop(held);
        let error = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap();
        assert_eq!(error.status, 409);
        assert!(error.message.starts_with("Native journal is missing"));
        assert!(!controller.file(&id, "exit").exists());
        assert!(
            storage::environment::ownership(&controller.state, &run, "probe")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn native_disk_admission_retries_physical_contention_during_bootstrap() {
        let (_fixture, controller) = fixture().await;
        let (run, id, plan) = busy_disk_fixture(&controller).await;
        let root = workspace::directory(&controller.state, &run).unwrap();
        tokio::fs::remove_file(root.join("block-initialized"))
            .await
            .unwrap();
        let lease = storage::environment::ownership(&controller.state, &run, "fixture")
            .await
            .unwrap();
        let held =
            crate::file_lock::exclusive(&lease.directory.join("lock"), "physical owner").unwrap();
        drop(lease);
        let waiting = tokio::spawn({
            let controller = controller.clone();
            let id = id.clone();
            async move {
                controller
                    .mount_after_release(
                        &id,
                        &plan,
                        &CancellationToken::new(),
                        Duration::from_secs(5),
                    )
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if tokio::fs::metadata(controller.file(&id, "log"))
                    .await
                    .unwrap()
                    .len()
                    > 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!waiting.is_finished());
        let global = tokio::time::timeout(Duration::from_secs(1), disk::lock(&controller.state))
            .await
            .unwrap()
            .unwrap();
        drop(global);
        drop(held);
        // The intentionally absent storage credentials reject before privileged
        // image preparation, proving that bootstrap retried after lock release.
        let error = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap();
        assert!(
            error.is_internal(),
            "invalid storage fixture must reject before image preparation"
        );
        assert_ne!(error.message, block::DISK_BUSY);
    }

    #[tokio::test]
    async fn lease_expiry_during_disk_admission_records_the_initiating_cause() {
        let (_fixture, controller) = fixture().await;
        let (run, id, plan) = busy_disk_fixture(&controller).await;
        let held = storage::environment::ownership(&controller.state, &run, "fixture")
            .await
            .unwrap();
        let stop = CancellationToken::new();
        controller
            .leases
            .lock()
            .await
            .insert(id.clone(), crate::nodes::boot_ms() + 150);
        let watching = tokio::spawn({
            let controller = controller.clone();
            let id = id.clone();
            let stop = stop.clone();
            async move { controller.watch_lease(&id, stop).await }
        });
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            controller.mount_after_release(&id, &plan, &stop, Duration::from_secs(5)),
        )
        .await
        .unwrap()
        .err()
        .unwrap();
        assert_eq!(error.message, "Native disk preparation stopped.");
        watching.await.unwrap();
        let path = controller.file(&id, "stop-reason.json");
        // Fencing only waits a bounded time for diagnostics. Its blocking
        // writer retains ownership and may commit after both callers return.
        let saved = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match tokio::fs::read(&path).await {
                    Ok(bytes) => break serde_json::from_slice::<Value>(&bytes).unwrap(),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("Could not read native stop reason: {error}"),
                }
            }
        })
        .await
        .expect("Native stop diagnostics must finish committing after fencing");
        assert_eq!(saved["reason"], "lease_expired");
        controller.record_stop(&id, "cancelled", Value::Null).await;
        assert_eq!(
            serde_json::from_slice::<Value>(&tokio::fs::read(path).await.unwrap()).unwrap(),
            saved
        );
        assert!(
            storage::environment::ownership(&controller.state, &run, "probe")
                .await
                .is_err()
        );
        drop(held);
    }

    #[tokio::test]
    async fn lease_expiry_on_global_lock_release_prevents_preparation_without_a_watcher() {
        let (_fixture, controller) = fixture().await;
        let (_, id, plan) = busy_disk_fixture(&controller).await;
        let held = disk::lock(&controller.state).await.unwrap();
        controller
            .leases
            .lock()
            .await
            .insert(id.clone(), crate::nodes::boot_ms() + 50);
        let waiting = tokio::spawn({
            let controller = controller.clone();
            let id = id.clone();
            async move {
                controller
                    .mount_after_release(
                        &id,
                        &plan,
                        &CancellationToken::new(),
                        Duration::from_secs(5),
                    )
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(held);
        let error = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap();
        assert_eq!(error.message, "Native disk preparation stopped.");
        let saved: Value = serde_json::from_slice(
            &tokio::fs::read(controller.file(&id, "stop-reason.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(saved["reason"], "lease_expired");
    }

    #[tokio::test]
    async fn controller_shutdown_during_preparation_retains_its_specific_reason() {
        let (_fixture, controller) = fixture().await;
        let (_, id, plan) = busy_disk_fixture(&controller).await;
        let held = disk::lock(&controller.state).await.unwrap();
        let waiting = tokio::spawn({
            let controller = controller.clone();
            let id = id.clone();
            async move {
                let stop = controller.stop.child_token();
                controller
                    .mount_after_release(&id, &plan, &stop, Duration::from_secs(5))
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        controller.stop.cancel();
        let error = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap();
        assert_eq!(error.message, "Native disk preparation stopped.");
        let saved: Value = serde_json::from_slice(
            &tokio::fs::read(controller.file(&id, "stop-reason.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(saved["reason"], "controller_shutdown");
        drop(held);
    }

    #[tokio::test]
    async fn native_disk_admission_deadline_keeps_the_existing_owner_fenced() {
        let (_fixture, controller) = fixture().await;
        let (run, id, plan) = busy_disk_fixture(&controller).await;
        let held = storage::environment::ownership(&controller.state, &run, "fixture owner")
            .await
            .unwrap();
        let error = controller
            .mount_after_release(
                &id,
                &plan,
                &CancellationToken::new(),
                Duration::from_millis(50),
            )
            .await
            .err()
            .unwrap();
        assert!(error.is_unavailable());
        assert!(error.message.contains("deadline"));
        assert!(
            storage::environment::ownership(&controller.state, &run, "probe")
                .await
                .is_err()
        );
        drop(held);
        assert!(
            storage::environment::ownership(&controller.state, &run, "probe")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn native_disk_admission_cancels_while_waiting_for_global_storage() {
        let (_fixture, controller) = fixture().await;
        let (_, id, plan) = busy_disk_fixture(&controller).await;
        let held = disk::lock(&controller.state).await.unwrap();
        let stop = CancellationToken::new();
        let waiting = tokio::spawn({
            let controller = controller.clone();
            let stop = stop.clone();
            async move {
                controller
                    .mount_after_release(&id, &plan, &stop, Duration::from_secs(5))
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        stop.cancel();
        let error = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .err()
            .unwrap();
        assert_eq!(error.message, "Native disk preparation stopped.");
        drop(held);
        assert!(disk::lock(&controller.state).await.is_ok());
    }

    #[tokio::test]
    async fn execution_deadline_bounds_global_disk_wait_and_records_the_cause() {
        let (_fixture, controller) = fixture().await;
        let (_, id, mut plan) = busy_disk_fixture(&controller).await;
        let held = disk::lock(&controller.state).await.unwrap();
        plan["expires"] = (crate::config::now() + 50).into();
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            controller.mount_after_release(
                &id,
                &plan,
                &CancellationToken::new(),
                Duration::from_secs(5),
            ),
        )
        .await
        .unwrap()
        .err()
        .unwrap();
        assert_eq!(error.status, 408);
        let saved: Value = serde_json::from_slice(
            &tokio::fs::read(controller.file(&id, "stop-reason.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(saved["reason"], "time_limit");
        drop(held);
    }

    #[tokio::test]
    async fn native_disk_admission_does_not_retry_permanent_conflicts_or_expired_plans() {
        let (_fixture, controller) = fixture().await;
        let (_, id, mut plan) = busy_disk_fixture(&controller).await;
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            controller.mount_after_release(
                &id,
                &plan,
                &CancellationToken::new(),
                Duration::from_secs(5),
            ),
        )
        .await
        .unwrap()
        .err()
        .unwrap();
        assert!(error.message.starts_with("Native journal is missing"));
        assert_eq!(
            tokio::fs::metadata(controller.file(&id, "log"))
                .await
                .unwrap()
                .len(),
            0
        );
        plan["expires"] = 1.into();
        let error = controller
            .mount_after_release(
                &id,
                &plan,
                &CancellationToken::new(),
                Duration::from_secs(5),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.status, 408);
    }

    #[tokio::test]
    async fn native_supervision_records_the_trigger_and_both_disk_budgets() {
        let (_fixture, controller) = fixture().await;
        let id = crate::config::id();
        atomic_write(&controller.file(&id, "log"), b"")
            .await
            .unwrap();
        let usage = json!({
            "workspaceBytes": 24,
            "temporaryBytes": 24,
            "conversationBytes": 48,
            "conversationLimitBytes": 32,
            "nodeBytes": 100,
            "nodeLimitBytes": 512,
        });
        let reason = supervision_stop_reason(true, false, 48, 32, 100, 512).unwrap();
        let details = json!({ "reason": reason, "at": crate::config::now(), "usage": usage });
        controller.write_stop_diagnostics(&id, &details).await;
        let saved: Value = serde_json::from_slice(
            &tokio::fs::read(controller.file(&id, "stop-reason.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(saved["reason"], "conversation_disk_limit");
        assert_eq!(saved["usage"], details["usage"]);
        assert!(saved["at"].as_i64().unwrap() > 0);
        assert!(
            tokio::fs::metadata(controller.file(&id, "log"))
                .await
                .unwrap()
                .len()
                > 0
        );
        assert_eq!(
            supervision_stop_reason(false, false, 0, 32, 0, 512),
            Some("lease_expired")
        );
        assert_eq!(
            supervision_stop_reason(true, true, 0, 32, 0, 512),
            Some("time_limit")
        );
        assert_eq!(
            supervision_stop_reason(true, false, 32, 32, 513, 512),
            Some("node_disk_limit")
        );
        assert_eq!(supervision_stop_reason(true, false, 32, 32, 512, 512), None);
    }

    #[tokio::test]
    async fn native_stop_fences_after_the_log_lock_remains_held() {
        let (_fixture, controller) = fixture().await;
        let id = crate::config::id();
        atomic_write(&controller.file(&id, "log"), b"")
            .await
            .unwrap();
        let details = json!({ "reason": "time_limit", "at": crate::config::now(), "usage": null });
        super::super::record_stop_once(&controller.file(&id, "stop-reason.json"), &details)
            .await
            .unwrap();
        let held = controller.log_lock.lock().await;
        tokio::time::timeout(
            Duration::from_secs(5),
            controller.record_stop(&id, "time_limit", Value::Null),
        )
        .await
        .expect("Stop diagnostics must not prevent fencing when the log lock is held");
        assert_eq!(
            tokio::fs::metadata(controller.file(&id, "log"))
                .await
                .unwrap()
                .len(),
            0
        );
        let marker: Value = serde_json::from_slice(
            &tokio::fs::read(controller.file(&id, "stop-reason.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(marker, details);
        drop(held);
    }

    fn request(path: &str, body: &Value) -> Request {
        Request::builder()
            .method("POST")
            .uri(path)
            .header("authorization", "Bearer synthetic-private-controller-token")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn scoped_close_contention_measurement() {
        use std::os::fd::AsRawFd;

        fn cpu_us() -> u64 {
            let mut time: libc::timespec = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
                0
            );
            time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1_000
        }

        let (_fixture, controller) = fixture().await;
        let bytes = vec![0x5a; 4096];
        let path = controller.state.join("measurement-file");
        tokio::fs::write(&path, &bytes).await.unwrap();
        let mut samples = Vec::new();
        for trial in 0..8 {
            for legacy in if trial % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                let held = disk::lock(&controller.state).await.unwrap();
                let unrelated = tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    drop(held);
                });
                let started = std::time::Instant::now();
                let cpu = cpu_us();
                let mut sample = Value::Null;
                let close_io = async {
                    let admission_us = started.elapsed().as_micros() as u64;
                    let writing = std::time::Instant::now();
                    let mut file = tokio::fs::OpenOptions::new()
                        .write(true)
                        .open(&path)
                        .await
                        .unwrap();
                    file.write_all(&bytes).await.unwrap();
                    file.sync_all().await.unwrap();
                    let write_us = writing.elapsed().as_micros() as u64;
                    let advice = unsafe {
                        libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                    };
                    let reading = std::time::Instant::now();
                    let cold_hinted = tokio::fs::read(&path).await.unwrap();
                    let cold_hint_us = reading.elapsed().as_micros() as u64;
                    let reading = std::time::Instant::now();
                    let warm = tokio::fs::read(&path).await.unwrap();
                    let warm_us = reading.elapsed().as_micros() as u64;
                    assert_eq!(cold_hinted, bytes);
                    assert_eq!(warm, bytes);
                    sample = json!({
                        "legacyGlobalDrain": legacy,
                        "trial": trial,
                        "admissionUs": admission_us,
                        "writeAndFsyncUs": write_us,
                        "coldHintReadUs": cold_hint_us,
                        "warmReadUs": warm_us,
                        "coldHintStatus": advice,
                        "fileBytes": tokio::fs::metadata(&path).await.unwrap().len(),
                        "processCpuUs": cpu_us().saturating_sub(cpu),
                        "hash": blake3::hash(&warm).to_hex().to_string(),
                    });
                    Ok(block::Completion::Flushed)
                };
                if legacy {
                    // Exact admission sequence of ADR0077, under the same real
                    // pool lock as the candidate. No network latency model.
                    drop(disk::lock(&controller.state).await.unwrap());
                    close_io.await.unwrap();
                } else {
                    StorageClose::default().finish(close_io).await.unwrap();
                }
                unrelated.await.unwrap();
                samples.push(sample);
            }
        }
        eprintln!(
            "NATIVE_CLOSE_GATE_MEASUREMENT {}",
            json!({
                "samples": samples,
                "unrelatedPoolHoldMs": 50,
                "fixture": "4KiB raw host file, same bytes; no journal or object encoding changed",
                "coldRead": "per-file DONTNEED hint after fsync; residency not asserted",
                "host": std::env::var("ImageVersion").unwrap_or_else(|_| "unspecified".into()),
                "cpus": std::thread::available_parallelism().unwrap().get(),
                "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap().trim(),
                "cpuMeasurement": "process CPU; run this test alone (nextest or --exact)",
                "scope": "controlled production-gate contention fixture, not production/ublk latency",
            })
        );
    }

    #[tokio::test]
    async fn first_close_drain_does_not_wait_for_an_unrelated_pool_operation() {
        let (_fixture, controller) = fixture().await;
        // This is the production pool gate used by seed transfers and policy
        // updates, not a simulated delay in the close operation itself.
        let unrelated = disk::lock(&controller.state).await.unwrap();
        let mut close = StorageClose::default();
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            close.finish(async { Ok(block::Completion::Flushed) }),
        )
        .await;
        drop(unrelated);
        assert!(
            result.is_ok(),
            "unrelated pool ownership blocked the first close drain"
        );
        assert_eq!(result.unwrap().unwrap(), block::Completion::Flushed);
    }

    #[tokio::test]
    async fn close_drains_admitted_mutations_then_allows_policy_changes_and_refuses_late_mutations()
    {
        let (_fixture, controller) = fixture().await;
        let (run, id, _) = busy_disk_fixture(&controller).await;
        let stop = CancellationToken::new();
        stop.cancel();
        controller.active.lock().await.insert(
            id.clone(),
            Attempt {
                live_access: Arc::default(),
                run: run.clone(),
                expires: None,
                stop,
                started: true,
                protection: None,
                suspension: Arc::new(super::super::suspension::Suspension::new(
                    &controller.state,
                    &id,
                )),
            },
        );
        let live_access = controller
            .active
            .lock()
            .await
            .get(&id)
            .unwrap()
            .live_access
            .clone();
        let admitted = live_access.lock().await;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let closing = tokio::spawn({
            let live_access = live_access.clone();
            let entered = entered.clone();
            let release = release.clone();
            async move {
                StorageClose::new(live_access)
                    .finish(async {
                        entered.notify_one();
                        release.notified().await;
                        Ok(block::Completion::Flushed)
                    })
                    .await
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), entered.notified())
                .await
                .is_err()
        );
        assert!(!closing.is_finished());
        drop(admitted);
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        let result = async {
            let policy = storage::policy::Configuration {
                generation: 1,
                policy: storage::policy::Policy {
                    reserve_mi_b: 64,
                    reserve_percent: 1,
                    memory_cache_mi_b: 128,
                    ..storage::policy::Policy::default()
                },
            };
            route(
                controller.clone(),
                request("/storage-policy", &json!(policy)),
            )
            .await?;
            let mut budget = controller.budget.lock().await.clone();
            budget["slots"] = 2.into();
            route(controller.clone(), request("/node-budget", &budget)).await?;
            for path in [
                format!("/runs/{id}/artifact"),
                format!("/runs/{id}/projects/{}", crate::config::id()),
            ] {
                let error = route(controller.clone(), request(&path, &json!({"runId":run})))
                    .await
                    .unwrap_err();
                assert_eq!(error.status, 403);
            }
            assert!(controller.active.lock().await.contains_key(&id));
            assert!(controller.leases.lock().await.contains_key(&id));
            assert!(!controller.file(&id, "exit").exists());
            Ok::<_, Error>(())
        };
        let result = tokio::time::timeout(Duration::from_secs(2), result).await;
        release.notify_one();
        assert_eq!(closing.await.unwrap().unwrap(), block::Completion::Flushed);
        result.unwrap().unwrap();
    }

    #[tokio::test]
    async fn native_wait_streams_heartbeats_until_physical_ownership_is_released() {
        use futures_util::StreamExt;

        let (_fixture, controller) = fixture().await;
        let attempt = crate::config::id();
        controller.active.lock().await.insert(
            attempt.clone(),
            Attempt {
                live_access: Arc::default(),
                run: crate::config::id(),
                expires: None,
                stop: CancellationToken::new(),
                started: true,
                protection: None,
                suspension: Arc::new(super::super::suspension::Suspension::new(
                    &controller.state,
                    &attempt,
                )),
            },
        );
        atomic_write(&controller.file(&attempt, "exit"), b"17")
            .await
            .unwrap();

        let response = tokio::time::timeout(
            Duration::from_millis(250),
            handler(
                State(controller.clone()),
                request(&format!("/runs/{attempt}/wait"), &json!({})),
            ),
        )
        .await
        .expect("Native wait must send response headers before the attempt finishes")
        .unwrap();
        assert_eq!(response.headers()["content-type"], "application/json");
        let mut body = response.into_body().into_data_stream();
        let mut received = Vec::new();
        for _ in 0..2 {
            let bytes = tokio::time::timeout(Duration::from_secs(2), body.next())
                .await
                .expect("An unfinished native attempt must keep its wait connection alive")
                .unwrap()
                .unwrap();
            assert!(bytes.iter().all(u8::is_ascii_whitespace));
            received.extend_from_slice(&bytes);
        }

        controller.active.lock().await.remove(&attempt);
        while let Some(bytes) = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .unwrap()
        {
            received.extend_from_slice(&bytes.unwrap());
        }
        let result: Value = serde_json::from_slice(&received).unwrap();
        assert_eq!(result, json!({ "StatusCode": 17 }));
    }

    #[tokio::test]
    async fn failed_native_flush_survives_read_only_final_write_after_cleanup() {
        use futures_util::StreamExt;
        use std::os::unix::fs::PermissionsExt;
        let (_fixture, controller) = fixture().await;
        let attempt = crate::config::id();
        let (code, failed) = completion_status(1, block::Completion::FailedFlush);
        let record = exit_record(code, failed);
        controller.active.lock().await.insert(
            attempt.clone(),
            Attempt {
                live_access: Arc::default(),
                run: crate::config::id(),
                expires: None,
                stop: CancellationToken::new(),
                started: true,
                protection: None,
                suspension: Arc::new(super::super::suspension::Suspension::new(
                    &controller.state,
                    &attempt,
                )),
            },
        );
        atomic_write(&controller.file(&attempt, "exit"), record.as_bytes())
            .await
            .unwrap();
        let marker = controller.file(&attempt, "active");
        atomic_write(&marker, b"run").await.unwrap();
        preserve_interrupted_exit(&marker).await.unwrap();
        let response = handler(
            State(controller.clone()),
            request(&format!("/runs/{attempt}/wait"), &json!({})),
        )
        .await
        .unwrap();
        let mut body = response.into_body().into_data_stream();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), body.next())
                .await
                .is_err()
        );
        let other_path = controller.file(&crate::config::id(), "exit");
        std::os::unix::fs::symlink(controller.file(&attempt, "exit"), &other_path).unwrap();
        std::fs::set_permissions(&controller.state, std::fs::Permissions::from_mode(0o500))
            .unwrap();
        let written = crate::runner::write_final_result(
            &controller.file(&attempt, "exit"),
            record.as_bytes(),
            true,
        )
        .await;
        let other = crate::runner::write_final_result(&other_path, record.as_bytes(), true).await;
        std::fs::write(controller.file(&attempt, "exit"), b"{broken").unwrap();
        let corrupt = crate::runner::write_final_result(
            &controller.file(&attempt, "exit"),
            record.as_bytes(),
            true,
        )
        .await;
        std::fs::write(controller.file(&attempt, "exit"), record.as_bytes()).unwrap();
        std::fs::set_permissions(&controller.state, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert!(
            written.is_ok(),
            "a committed failure must not require another write: {written:?}"
        );
        assert!(other.is_err() && corrupt.is_err());
        let hardlink = controller.file(&crate::config::id(), "exit");
        std::fs::hard_link(controller.file(&attempt, "exit"), &hardlink).unwrap();
        std::fs::set_permissions(&controller.state, std::fs::Permissions::from_mode(0o500))
            .unwrap();
        let other = crate::runner::write_final_result(&hardlink, record.as_bytes(), true).await;
        std::fs::set_permissions(&controller.state, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::remove_file(hardlink).unwrap();
        assert!(other.is_err());
        controller.active.lock().await.remove(&attempt);
        let mut received = Vec::new();
        while let Some(bytes) = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .unwrap()
        {
            received.extend_from_slice(&bytes.unwrap());
        }
        assert_eq!(
            serde_json::from_slice::<Value>(&received).unwrap(),
            json!({ "StatusCode": 70, "FinalizationFailed": true })
        );
        for _ in 0..2 {
            let response = handler(
                State(controller.clone()),
                request(&format!("/runs/{attempt}/wait"), &json!({})),
            )
            .await
            .unwrap();
            let body = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            let result: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                result,
                json!({ "StatusCode": 70, "FinalizationFailed": true })
            );
        }
        let (code, failed) = completion_status(1, block::Completion::Flushed);
        assert_eq!(
            serde_json::from_str::<Value>(&exit_record(code, failed)).unwrap(),
            json!({ "StatusCode": 1, "PhysicalFinalized": true })
        );
    }

    #[tokio::test]
    async fn native_stop_cancels_workload_when_stop_marker_cannot_be_written() {
        let (_root, controller) = fixture().await;
        let id = crate::config::id();
        let stop = CancellationToken::new();
        controller.active.lock().await.insert(
            id.clone(),
            Attempt {
                live_access: Arc::default(),
                run: crate::config::id(),
                expires: None,
                stop: stop.clone(),
                started: true,
                protection: None,
                suspension: Arc::new(super::super::suspension::Suspension::new(
                    &controller.state,
                    &id,
                )),
            },
        );
        tokio::fs::create_dir(controller.file(&id, "stopped"))
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), controller.stop_attempt(&id))
                .await
                .unwrap()
                .is_err()
        );
        assert!(stop.is_cancelled());
        assert!(controller.active.lock().await.contains_key(&id));
    }

    #[tokio::test]
    async fn native_logs_keep_quiet_attempts_alive_without_splitting_json_records() {
        use futures_util::StreamExt;

        let (_fixture, controller) = fixture().await;
        let attempt = crate::config::id();
        controller.active.lock().await.insert(
            attempt.clone(),
            Attempt {
                live_access: Arc::default(),
                run: crate::config::id(),
                expires: None,
                stop: CancellationToken::new(),
                started: true,
                protection: None,
                suspension: Arc::new(super::super::suspension::Suspension::new(
                    &controller.state,
                    &attempt,
                )),
            },
        );
        let log = controller.file(&attempt, "log");
        atomic_write(&log, b"").await.unwrap();
        let request = Request::builder()
            .method("GET")
            .uri(format!("/runs/{attempt}/logs"))
            .header("authorization", "Bearer synthetic-private-controller-token")
            .body(Body::empty())
            .unwrap();
        let response = handler(State(controller.clone()), request).await.unwrap();
        let mut body = response.into_body().into_data_stream();
        let heartbeat = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("A quiet native attempt must keep its logs connection alive")
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&heartbeat).unwrap(),
            json!({ "type": "heartbeat" }),
        );

        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&log)
            .await
            .unwrap();
        file.write_all(b"{\"type\":\"agent_").await.unwrap();
        file.flush().await.unwrap();
        let prefix = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(&prefix[..], b"{\"type\":\"agent_");
        assert!(
            tokio::time::timeout(Duration::from_millis(1200), body.next())
                .await
                .is_err(),
            "Heartbeats must not be inserted inside a partially written JSON record",
        );
        // Queue the reader's EOF lifecycle check behind the final append and retirement.
        let mut active = controller.active.lock().await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        file.write_all(b"message\",\"text\":\"ok\"}\n")
            .await
            .unwrap();
        file.flush().await.unwrap();
        assert_eq!(
            tokio::fs::read(&log).await.unwrap(),
            b"{\"type\":\"agent_message\",\"text\":\"ok\"}\n"
        );
        active.remove(&attempt);
        drop(active);
        let mut record = prefix.to_vec();
        while let Some(bytes) = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .unwrap()
        {
            record.extend_from_slice(&bytes.unwrap());
        }
        assert_eq!(
            serde_json::from_slice::<Value>(&record).unwrap(),
            json!({ "type": "agent_message", "text": "ok" }),
        );
    }

    async fn sparse(path: &Path, size: u64) {
        private_dir(path.parent().unwrap()).await.unwrap();
        tokio::fs::File::create(path)
            .await
            .unwrap()
            .set_len(size)
            .await
            .unwrap();
    }

    async fn artifact_fixture() -> (tempfile::TempDir, Controller, String, String, PathBuf) {
        let (fixture, controller) = fixture().await;
        let run = crate::config::id();
        let attempt = crate::config::id();
        let stage = controller.data.join("runs").join(&run);
        let environment = workspace::directory(&controller.state, &run).unwrap();
        for directory in [
            stage.join("workspace"),
            stage.join("home"),
            environment.join("tmp"),
            environment.join("data/tmp"),
            controller.data.join("runner-plans"),
        ] {
            private_dir(&directory).await.unwrap();
        }
        let plan = json!({
            "id": attempt, "runId": run, "backend": "native", "nodeLeaseRequired": true, "expires": null,
            "cwd": stage.join("workspace"), "projectRoot": stage.join("workspace"),
            "sandbox": "workspace-write", "args": [], "prompt": "fixture",
            "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
            "imports": [
                { "source": stage.join("workspace"), "target": stage.join("workspace") },
                { "source": stage.join("home"), "target": "/home/node" }
            ]
        });
        atomic_write(
            &controller
                .data
                .join("runner-plans")
                .join(format!("{attempt}.json")),
            &serde_json::to_vec(&plan).unwrap(),
        )
        .await
        .unwrap();
        controller.active.lock().await.insert(
            attempt.clone(),
            Attempt {
                live_access: Arc::default(),
                run: run.clone(),
                expires: None,
                stop: CancellationToken::new(),
                started: true,
                protection: None,
                suspension: Arc::new(super::super::suspension::Suspension::new(
                    &controller.state,
                    &attempt,
                )),
            },
        );
        (fixture, controller, run, attempt, environment)
    }

    #[tokio::test]
    async fn queued_live_routes_do_not_delay_close_or_admit_after_cancellation() {
        use std::{
            future::{Future, poll_fn},
            task::Poll,
        };

        for artifact in [true, false] {
            let (_fixture, controller, run, id, environment) = artifact_fixture().await;
            let project = crate::config::id();
            let target = environment.join("data/workspaces").join(&project);
            private_dir(&target).await.unwrap();
            tokio::fs::write(target.join("preserved"), b"existing project bytes")
                .await
                .unwrap();
            let plan = controller.plan(&id).await.unwrap();
            let (path, body) = if artifact {
                (
                    format!("/runs/{id}/artifact"),
                    json!({ "runId": run, "path": "/tmp/result" }),
                )
            } else {
                (
                    format!("/runs/{id}/projects/{project}"),
                    json!({
                        "runId": run,
                        "source": guest_data(&plan, &controller.data).join("runs").join(&run).join("workspace").join(&project),
                        "target": Path::new(text(&plan, "projectRoot")).join(&project),
                    }),
                )
            };
            let unrelated = disk::lock(&controller.state).await.unwrap();
            let pending = route(controller.clone(), request(&path, &body));
            tokio::pin!(pending);
            // Poll the actual handler into the global gate before closing. It
            // must not acquire this attempt's live-access guard while queued.
            let queued = poll_fn(|cx| Poll::Ready(pending.as_mut().poll(cx))).await;
            assert!(queued.is_pending());
            let live_access = {
                let active = controller.active.lock().await;
                let attempt = active.get(&id).unwrap();
                attempt.stop.cancel();
                attempt.live_access.clone()
            };
            let mut close = StorageClose::new(live_access);
            let closed = tokio::time::timeout(
                Duration::from_millis(250),
                close.finish(async { Ok(block::Completion::Flushed) }),
            )
            .await;
            let still_held = crate::file_lock::exclusive(
                &controller.state.join("storage.lock"),
                "unrelated pool operation is still active",
            )
            .is_err();
            drop(unrelated);
            let rejected = tokio::time::timeout(Duration::from_secs(2), pending).await;
            assert_eq!(closed.unwrap().unwrap(), block::Completion::Flushed);
            assert!(still_held);
            assert_eq!(rejected.unwrap().unwrap_err().status, 403);
            assert!(!controller.state.join("exports").exists());
            assert_eq!(
                tokio::fs::read(target.join("preserved")).await.unwrap(),
                b"existing project bytes"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_artifact_request_drains_its_admitted_io_before_close() {
        assert_interrupted_artifact_drain(false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timed_out_artifact_request_drains_its_admitted_io_before_close() {
        assert_interrupted_artifact_drain(true).await;
    }

    async fn assert_interrupted_artifact_drain(request_timeout: bool) {
        let (_fixture, controller, run, id, environment) = artifact_fixture().await;
        let path = environment.join("tmp/result");
        tokio::fs::write(&path, b"preserved artifact bytes")
            .await
            .unwrap();
        let (opening, release) = crate::artifacts::file::tests::delay_open(&path);
        let expire_request = Arc::new(Notify::new());
        let export = tokio::spawn({
            let controller = controller.clone();
            let id = id.clone();
            let expire_request = expire_request.clone();
            async move {
                let exporting = route(
                    controller,
                    request(
                        &format!("/runs/{id}/artifact"),
                        &json!({ "runId": run, "path": "/tmp/result" }),
                    ),
                );
                tokio::pin!(exporting);
                tokio::select! {
                    result = &mut exporting => result,
                    () = expire_request.notified() => {
                        // The real open is already blocked before this short
                        // HTTP deadline starts, so no scheduler timing is assumed.
                        tokio::time::timeout(Duration::from_millis(1), &mut exporting)
                            .await
                            .unwrap_or_else(|_| Err(Error::timeout("Synthetic request timeout.")))
                    }
                }
            }
        });
        let opened =
            tokio::task::spawn_blocking(move || opening.recv_timeout(Duration::from_secs(5)))
                .await
                .unwrap();
        if opened.is_err() {
            let _ = release.send(());
        }
        opened.expect("artifact must reach its real blocking filesystem open");
        if request_timeout {
            expire_request.notify_one();
            assert_eq!(
                export.await.unwrap().unwrap_err().message,
                "Synthetic request timeout."
            );
        } else {
            export.abort();
            assert!(export.await.unwrap_err().is_cancelled());
        }
        controller
            .active
            .lock()
            .await
            .get(&id)
            .unwrap()
            .stop
            .cancel();
        let pool_retained = crate::file_lock::exclusive(
            &controller.state.join("storage.lock"),
            "admitted filesystem work must retain pool ownership",
        )
        .is_err();
        let entered = Arc::new(Notify::new());
        let live_access = controller
            .active
            .lock()
            .await
            .get(&id)
            .unwrap()
            .live_access
            .clone();
        let closing = tokio::spawn({
            let entered = entered.clone();
            async move {
                StorageClose::new(live_access)
                    .finish(async {
                        entered.notify_one();
                        Ok(block::Completion::Flushed)
                    })
                    .await
            }
        });
        let drained_too_early =
            tokio::time::timeout(Duration::from_millis(100), entered.notified())
                .await
                .is_ok();
        // Always release the actual blocking operation before an assertion.
        let _ = release.send(());
        let closed = tokio::time::timeout(Duration::from_secs(5), closing).await;
        assert!(
            pool_retained,
            "request cancellation detached IO from its ownership"
        );
        assert!(
            !drained_too_early,
            "close raced an admitted filesystem open"
        );
        assert_eq!(
            closed.unwrap().unwrap().unwrap(),
            block::Completion::Flushed
        );
        assert_eq!(
            tokio::fs::read(path).await.unwrap(),
            b"preserved artifact bytes"
        );
        assert_eq!(
            std::fs::read_dir(controller.state.join("exports"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn tmp_artifact_uses_the_ephemeral_path_and_rejects_redirects() {
        let (_fixture, controller, run, attempt, environment) = artifact_fixture().await;
        tokio::fs::write(environment.join("tmp/result"), b"actual temporary artifact")
            .await
            .unwrap();
        tokio::fs::write(environment.join("data/tmp/result"), b"wrong stale bytes")
            .await
            .unwrap();
        let response = route(
            controller.clone(),
            request(
                &format!("/runs/{attempt}/artifact"),
                &json!({ "runId": run, "path": "/tmp/result" }),
            ),
        )
        .await
        .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), b"actual temporary artifact");
        std::os::unix::fs::symlink(
            environment.join("data/tmp/result"),
            environment.join("tmp/redirect"),
        )
        .unwrap();
        assert!(
            route(
                controller,
                request(
                    &format!("/runs/{attempt}/artifact"),
                    &json!({ "runId": run, "path": "/tmp/redirect" })
                )
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn delayed_artifact_keeps_health_leases_and_cancellation_responsive() {
        // Isolate the native tool bundle environment from parallel unit tests.
        let tools = tempfile::tempdir().unwrap();
        let bundle = tools.path().join("tools");
        std::fs::create_dir(&bundle).unwrap();
        std::fs::write(bundle.join("manifest.json"), b"{}").unwrap();
        std::fs::write(
            tools.path().join("release.json"),
            serde_json::to_vec(&json!({
                "runtimeId": crate::nodes::component_runtime(),
                "os": "linux", "arch": "x86_64"
            }))
            .unwrap(),
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native::service::tests::delayed_artifact_control_child",
                "--ignored",
            ])
            .env("CAIRN_NATIVE_TOOLS", bundle)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "isolated native tool bundle environment"]
    fn delayed_artifact_control_child() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (_fixture, controller, run, attempt, environment) =
            runtime.block_on(artifact_fixture());
        let path = environment.join("tmp/result");
        std::fs::write(&path, b"completed artifact").unwrap();
        let (opening, release) = crate::artifacts::file::tests::delay_open(&path);
        let exporting = controller.clone();
        let id = attempt.clone();
        let export = runtime.spawn(async move {
            handler(
                State(exporting),
                request(
                    &format!("/runs/{id}/artifact"),
                    &json!({ "runId": run, "path": "/tmp/result" }),
                ),
            )
            .await
        });
        let opened = opening.recv_timeout(Duration::from_secs(5));
        let physical_owner = crate::file_lock::exclusive(
            &controller.state.join("storage.lock"),
            "Export must retain physical ownership",
        );
        let controls = controller.clone();
        let (progress, observed) = std::sync::mpsc::channel();
        let control = runtime.spawn(async move {
            let health = handler(
                State(controls.clone()),
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(health.status(), 200);
            let health: Value = serde_json::from_slice(
                &axum::body::to_bytes(health.into_body(), 100_000)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(health["activeRuns"], 1);
            let lease = handler(
                State(controls.clone()),
                request(
                    &format!("/runs/{attempt}/lease"),
                    &json!({ "remainingMs": 60_000 }),
                ),
            )
            .await
            .unwrap();
            assert_eq!(lease.status(), 200);
            assert!(controls.leased(&attempt).await);
            // Exercise the same active-map cancellation and disk-abort path
            // without invoking privileged host systemd operations in a unit test.
            controls
                .active
                .lock()
                .await
                .get(&attempt)
                .unwrap()
                .stop
                .cancel();
            controls.abort_disk(&attempt).await.unwrap();
            let _ = progress.send(());
        });
        let responsive = observed.recv_timeout(Duration::from_secs(2));
        // Release the filesystem even when the old controller holds active.
        let _ = release.send(());
        let exported = runtime.block_on(export).unwrap();
        runtime.block_on(control).unwrap();
        opened.expect("The native artifact must reach its filesystem open");
        assert!(physical_owner.is_err());
        responsive
            .expect("Health, lease renewal and cancellation must finish during the delayed open");
        assert_eq!(exported.unwrap_err().status, 403);
        assert!(
            crate::file_lock::exclusive(&controller.state.join("storage.lock"), "Export finished",)
                .is_ok()
        );
        assert_eq!(
            std::fs::read_dir(controller.state.join("exports"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn native_disk_pool_charges_retained_conversations_archives_and_staging() {
        let (fixture, controller) = fixture().await;
        controller.budget.lock().await["limits"]["diskMiB"] = 32768.into();
        let first = crate::config::id();
        let second = crate::config::id();
        let gib = 1024 * disk::MIB;
        let retained = workspace::directory(&controller.state, &first)
            .unwrap()
            .join("data/workspaces/model");
        sparse(&retained, 20 * gib).await;
        let archive = controller.state.join("snapshots").join(crate::config::id());
        sparse(&archive.join("disk"), gib).await;
        atomic_write(&archive.join("run"), first.as_bytes())
            .await
            .unwrap();
        sparse(
            &controller.data.join("runs").join(&second).join("staging"),
            2 * gib,
        )
        .await;
        let usage = controller.usage().await.unwrap();
        assert!(usage["diskBytes"].as_u64().unwrap() > 23 * gib);
        assert_eq!(usage["diskMiB"], 23 * 1024 + 1);

        // A second 20 GiB conversation cannot consume the remaining shared pool.
        // The transfer checks the size frame before reading/writing its contents.
        let incoming = fixture.path().join("remote-source");
        sparse(&incoming.join("model"), 20 * gib).await;
        let receiving = tempfile::tempdir_in(controller.data.join("runs")).unwrap();
        let _owner = disk::lock(&controller.state).await.unwrap();
        let error = workspace::copy(
            &incoming,
            receiving.path(),
            controller.disk_growth().await.unwrap(),
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("disk budget"), "{}", error.message);
        assert!(!receiving.path().join("model").exists());
        assert_eq!(tokio::fs::metadata(retained).await.unwrap().len(), 20 * gib);
        assert_eq!(controller.usage().await.unwrap(), usage);
    }

    #[tokio::test]
    async fn private_socket_routes_ignore_a_spoofed_tcp_controller() {
        let (_root, controller) = fixture().await;
        let socket = controller.state.join("controller.sock");
        let listener = bind_controller(&socket).await.unwrap();
        let spoof = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", spoof.local_addr().unwrap());
        let router = Router::new().fallback(any(handler)).with_state(controller);
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let response = super::super::controller_client(&socket)
            .unwrap()
            .post(format!("{origin}/node-budget"))
            .bearer_auth("synthetic-private-controller-token")
            .json(&json!({
                "slots": 1,
                "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 }
            }))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), spoof.accept())
                .await
                .is_err()
        );
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn controller_socket_never_replaces_an_unrelated_file() {
        let (_root, controller) = fixture().await;
        let socket = controller.state.join("controller.sock");
        atomic_write(&socket, b"preserved owner data")
            .await
            .unwrap();
        assert!(bind_controller(&socket).await.is_err());
        assert_eq!(
            tokio::fs::read(socket).await.unwrap(),
            b"preserved owner data"
        );
    }

    #[tokio::test]
    async fn frozen_skill_copy_preserves_approved_executable_bits() {
        let root = tempfile::tempdir().unwrap();
        let (source, target) = (root.path().join("seed"), root.path().join("home"));
        private_dir(&source.join(".agents/skills/game/scripts"))
            .await
            .unwrap();
        private_dir(&target).await.unwrap();
        let relative = ".agents/skills/game/scripts/build";
        atomic_write(&source.join(relative), b"#!/bin/sh\nexit 0\n")
            .await
            .unwrap();
        tokio::fs::set_permissions(
            source.join(relative),
            std::fs::Permissions::from_mode(0o700),
        )
        .await
        .unwrap();
        atomic_write(
            &source.join(".agents/skills/game/SKILL.md"),
            b"Use the build script",
        )
        .await
        .unwrap();
        install_home(&source, &target).await.unwrap();
        assert_eq!(
            tokio::fs::metadata(target.join(relative))
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            tokio::fs::metadata(target.join(".agents/skills/game/SKILL.md"))
                .await
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            tokio::process::Command::new(target.join(relative))
                .status()
                .await
                .unwrap()
                .success()
        );
    }

    #[tokio::test]
    async fn private_controller_routes_require_the_node_credential() {
        let (_root, controller) = fixture().await;
        for (method, path) in [
            ("POST", "/node-budget"),
            ("GET", "/exclusive"),
            ("POST", "/exclusive/acquire"),
            ("POST", "/exclusive/release"),
        ] {
            let response = route(
                controller.clone(),
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap_err();
            assert_eq!(response.status, 401);
        }
    }

    #[tokio::test]
    async fn native_mode_final_write_failure_reports_unconfirmed_privileges() {
        let (_root, controller) = fixture().await;
        let target = json!({ "mode": "administrator", "generation": crate::config::id() });
        {
            let mut pending = controller.mode.lock().await;
            pending["status"] = "pending".into();
            pending["target"] = target.clone();
        }
        private_dir(&controller.state.join(mode::FILE))
            .await
            .unwrap();
        let applied = json!({ "mode": target["mode"], "generation": target["generation"], "status": "applied" });
        // Exercise final acknowledgement persistence without invoking host sudo.
        assert!(controller.persist_mode(&applied).await.is_err());
        let observed = controller.mode.lock().await.clone();
        assert_eq!(observed["status"], "failed");
        assert_eq!(
            observed["mode"], "protected",
            "Never publish an undurable applied mode"
        );
        assert_eq!(observed["target"], target);
        assert!(!text(&observed, "error").is_empty());
        assert!(mode::check_plan(&observed, &json!({})).is_err());
        assert!(mode::check_plan(&observed, &json!({ "nativeMode": target })).is_err());
    }

    #[tokio::test]
    async fn native_mode_persistence_failure_blocks_previously_leased_plans() {
        let (_root, controller) = fixture().await;
        let run = crate::config::id();
        let attempt = crate::config::id();
        let stage = controller.data.join("runs").join(&run);
        for directory in [
            stage.join("workspace"),
            stage.join("home"),
            controller.data.join("runner-plans"),
        ] {
            private_dir(&directory).await.unwrap();
        }
        let plan = json!({
            "id": attempt, "runId": run, "backend": "native", "nodeLeaseRequired": true, "expires": null,
            "cwd": stage.join("workspace"), "projectRoot": stage.join("workspace"),
            "sandbox": "workspace-write", "args": [], "prompt": "fixture",
            "resources": { "cpu": 1, "memoryMiB": 128, "diskMiB": 128 },
            "imports": [
                { "source": stage.join("workspace"), "target": stage.join("workspace") },
                { "source": stage.join("home"), "target": "/home/node" }
            ]
        });
        atomic_write(
            &controller
                .data
                .join("runner-plans")
                .join(format!("{attempt}.json")),
            &serde_json::to_vec(&plan).unwrap(),
        )
        .await
        .unwrap();
        controller
            .leases
            .lock()
            .await
            .insert(attempt.clone(), crate::nodes::boot_ms() + 60000);
        controller.plan(&attempt).await.unwrap();
        mode::check_plan(&*controller.mode.lock().await, &plan).unwrap();
        // A directory at the state-file destination guarantees an atomic rename
        // failure on this disposable fixture, before any host sudo operation.
        private_dir(&controller.state.join(mode::FILE))
            .await
            .unwrap();
        let request = json!({ "mode": "administrator", "generation": crate::config::id() });
        assert!(controller.change_mode(request.clone()).await.is_err());
        let state = controller.mode.lock().await.clone();
        assert_eq!(state["status"], "failed");
        assert!(!text(&state, "error").is_empty());
        assert_eq!(state["target"], request);
        let error = controller.start(&attempt).await.unwrap_err();
        assert_eq!(error.status, 409);
        assert!(error.message.contains("privilege generation"), "{error:?}");
        assert!(controller.active.lock().await.is_empty());
    }

    #[tokio::test]
    async fn expired_or_stopped_attempts_cannot_start_or_regain_a_lease() {
        let (_root, controller) = fixture().await;
        let attempt = crate::config::id();
        assert_eq!(controller.start(&attempt).await.unwrap_err().status, 409);
        atomic_write(&controller.file(&attempt, "stopped"), b"1")
            .await
            .unwrap();
        let error = route(
            controller.clone(),
            request(
                &format!("/runs/{attempt}/lease"),
                &json!({ "remainingMs": 60000 }),
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, 409);
        assert!(!controller.leased(&attempt).await);
    }

    #[tokio::test]
    async fn deleting_a_managed_workspace_does_not_follow_its_symlink_or_remove_unrelated_files() {
        let (_root, controller) = fixture().await;
        let outside = tempfile::tempdir().unwrap();
        tokio::fs::write(outside.path().join("keep"), b"owner files")
            .await
            .unwrap();
        let run = crate::config::id();
        let directory = workspace::directory(&controller.state, &run).unwrap();
        private_dir(&directory).await.unwrap();
        std::os::unix::fs::symlink(outside.path(), directory.join("data")).unwrap();
        let docker_runtime = directory.join("docker-runtime");
        let mirror = docker_runtime.join("runtime/mounts/fixture/source");
        tokio::fs::create_dir_all(&mirror).await.unwrap();
        tokio::fs::create_dir_all(docker_runtime.join("rootlesskit"))
            .await
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), mirror.join("outside")).unwrap();
        tokio::fs::write(docker_runtime.join("rootlesskit/child_pid"), b"123")
            .await
            .unwrap();
        tokio::fs::write(directory.join("unrelated"), b"preserved")
            .await
            .unwrap();
        route(
            controller,
            request(&format!("/disks/{run}/delete"), &json!({})),
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read(outside.path().join("keep")).await.unwrap(),
            b"owner files"
        );
        assert!(!directory.join("data").exists());
        assert!(!docker_runtime.exists());
        assert!(directory.join("unrelated").exists());
    }

    #[tokio::test]
    async fn native_budget_supports_six_slots_and_rejects_invalid_capacities() {
        let (_root, controller) = fixture().await;
        route(
            controller.clone(),
            request(
                "/node-budget",
                &json!({ "slots": 6, "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 } }),
            ),
        )
        .await
        .unwrap();
        assert_eq!(controller.budget.lock().await["slots"], 6);
        for slots in [json!(0), json!(4097), json!(1.5), json!(null)] {
            assert!(route(controller.clone(), request("/node-budget", &json!({
                "slots": slots, "limits": { "cpu": 1, "memoryMiB": 1024, "diskMiB": 1024 }
            }))).await.is_err());
        }
        assert_eq!(controller.budget.lock().await["slots"], 6);
    }

    #[tokio::test]
    async fn controller_enforces_all_configured_slots_before_preparing_an_attempt() {
        let (_root, controller) = fixture().await;
        controller.budget.lock().await["slots"] = 6.into();
        let attempt = crate::config::id();
        controller
            .leases
            .lock()
            .await
            .insert(attempt.clone(), crate::nodes::boot_ms() + 60000);
        for _ in 0..5 {
            controller.active.lock().await.insert(
                crate::config::id(),
                Attempt {
                    live_access: Arc::default(),
                    run: crate::config::id(),
                    expires: None,
                    stop: CancellationToken::new(),
                    started: true,
                    protection: None,
                    suspension: Arc::new(super::super::suspension::Suspension::new(
                        &controller.state,
                        &attempt,
                    )),
                },
            );
        }
        // Five occupied slots still admit a sixth attempt as far as plan loading.
        assert_ne!(controller.start(&attempt).await.unwrap_err().status, 503);
        let existing = crate::config::id();
        controller.active.lock().await.insert(
            existing.clone(),
            Attempt {
                live_access: Arc::default(),
                run: crate::config::id(),
                expires: None,
                stop: CancellationToken::new(),
                started: true,
                protection: None,
                suspension: Arc::new(super::super::suspension::Suspension::new(
                    &controller.state,
                    &attempt,
                )),
            },
        );
        assert_eq!(controller.start(&attempt).await.unwrap_err().status, 503);
        // Retrying an existing attempt never consumes a second slot.
        controller.start(&existing).await.unwrap();
        controller.active.lock().await.remove(&existing);
        assert_ne!(controller.start(&attempt).await.unwrap_err().status, 503);
    }
}
