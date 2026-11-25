//! Native implementation of the existing authenticated node controller protocol.
mod cleanup;
mod exclusive;
#[cfg(test)]
mod exclusive_tests;
mod health;
#[cfg(test)]
mod terminal_tests;

use super::{atomic_write, bootstrap, directory as block, disk, mode, systemd, workspace};
use crate::runner::exclusive::{Ledger, Reservations};
use crate::{
    error::{Error, Result},
    microvm::{plan::Plan, protocol::Event, wire},
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
use serde::{Deserialize, Serialize};
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

async fn remove_environment_sockets(root: &Path) -> Result<()> {
    for extension in ["e", "r"] {
        require_removed(tokio::fs::remove_dir_all(root.with_extension(extension)).await)?;
    }
    Ok(())
}

#[cfg(test)]
mod environment_cleanup_tests {
    use super::*;

    #[tokio::test]
    async fn retirement_removes_only_its_environment_socket_directories() {
        let root = tempfile::tempdir().unwrap();
        let attempt = root.path().join("attempt");
        let other = root.path().join("other.e");
        for path in [
            attempt.with_extension("e"),
            attempt.with_extension("r"),
            other.clone(),
        ] {
            tokio::fs::create_dir(&path).await.unwrap();
            tokio::fs::write(path.join("marker"), b"keep")
                .await
                .unwrap();
        }
        remove_environment_sockets(&attempt).await.unwrap();
        remove_environment_sockets(&attempt).await.unwrap();
        assert!(!attempt.with_extension("e").exists());
        assert!(!attempt.with_extension("r").exists());
        assert!(other.join("marker").exists());
    }
}

/// A completed retirement reaps only its captured old attempts, never the new
/// environment's canonical disk, bootstrap state or manager namespace.
async fn erase_native_attempts(
    controller: &Controller,
    run: &str,
    known: &[String],
    only_known: bool,
) -> Result<()> {
    if only_known && known.is_empty() {
        return Ok(());
    }
    {
        let active = controller.active.lock().await;
        if known.iter().any(|id| active.contains_key(id)) {
            return Err(Error::conflict("A retired native attempt is still active."));
        }
    }
    crate::storage::retirement::seed_attempts(&controller.state, &controller.data, run, known)
        .await?;
    let mut identities = Vec::new();
    let mut entries = tokio::fs::read_dir(&controller.state).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "run")
        {
            continue;
        }
        let id = entry
            .path()
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if only_known && !known.contains(&id) {
            continue;
        }
        let owner = match tokio::fs::read_to_string(entry.path()).await {
            Ok(owner) => owner,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if owner != run {
            continue;
        }
        uuid(&id)?;
        if controller.active.lock().await.contains_key(&id) {
            return Err(Error::conflict("A native attempt is still active."));
        }
        atomic_write(&controller.file(&id, "stopped"), b"1").await?;
        for extension in [
            "active",
            "vm.json",
            "log",
            "lease",
            "terminal",
            "cleanup-result",
        ] {
            require_removed(tokio::fs::remove_file(controller.file(&id, extension)).await)?;
        }
        require_removed(
            tokio::fs::remove_file(
                controller
                    .state
                    .join("roots")
                    .join(&id)
                    .with_extension("plan.json"),
            )
            .await,
        )?;
        identities.push(entry.path());
        require_removed(tokio::fs::remove_dir_all(controller.state.join("roots").join(&id)).await)?;
        remove_environment_sockets(&controller.state.join("roots").join(&id)).await?;
        require_removed(
            tokio::fs::remove_file(
                controller
                    .data
                    .join("runner-plans")
                    .join(format!("{id}.json")),
            )
            .await,
        )?;
    }
    for directory in [
        controller.state.clone(),
        controller.state.join("roots"),
        controller.data.join("runner-plans"),
    ] {
        if directory.is_dir() {
            tokio::fs::File::open(directory).await?.sync_all().await?;
        }
    }
    // Keep run ownership until private output and plan removals are durable.
    // A failed erase must remain discoverable even without request hints.
    for identity in identities {
        require_removed(tokio::fs::remove_file(identity).await)?;
    }
    tokio::fs::File::open(&controller.state)
        .await?
        .sync_all()
        .await?;
    Ok(())
}

fn require_removed(result: std::io::Result<()>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

const MAX_PLAN: u64 = 8_000_000;
const MAX_LOG: u64 = 64 * 1024 * 1024;
const MAX_TERMINAL_METADATA: u64 = 128;

#[derive(Clone, Copy, Eq, PartialEq)]
enum LogPurpose {
    Execution,
    Diagnostic,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TerminalMetadata {
    reason: crate::runner::TerminalReason,
}

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

#[derive(Debug)]
struct PreparationSpace {
    incoming: u64,
    spare: bool,
    usage_sample: (u64, std::time::Instant),
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

    #[cfg(test)]
    pub(super) async fn finish_budgeted(
        &mut self,
        volume: &Arc<storage::runtime::Volume>,
        closing: impl std::future::Future<Output = Result<block::Completion>>,
    ) -> Result<block::Completion> {
        volume.finish_fenced(self.finish(closing)).await
    }

    pub(super) async fn finish(
        &mut self,
        closing: impl std::future::Future<Output = Result<block::Completion>>,
    ) -> Result<block::Completion> {
        self.drain().await?;
        // The clean backend namespace removes host mount-inventory coupling.
        closing.await
    }

    pub(super) async fn drain(&mut self) -> Result<()> {
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
        Ok(())
    }
}

async fn settle_client(child: &mut tokio::process::Child, attempt: &str) {
    let mut kill_requested = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(_)) => return,
            Ok(Err(error)) => {
                tracing::error!(%attempt, %error, "Native supervisor client wait is unconfirmed; execution ownership retained");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(_) => {
                if !kill_requested {
                    let _ = child.start_kill();
                    kill_requested = true;
                }
                tracing::error!(%attempt, "Native supervisor client exit is unconfirmed; execution ownership retained");
            }
        }
    }
}

async fn settle_output(
    task: &mut tokio::task::JoinHandle<Result<()>>,
    bound: Duration,
) -> Result<()> {
    match tokio::time::timeout(bound, &mut *task).await {
        Ok(result) => result.map_err(Error::internal)?,
        Err(_) => {
            // Close only our reader; an inherited output FD never authorizes
            // killing an unrelated host service. Filesystem consumer fencing
            // still runs in the finalizer after this explicit failure.
            task.abort();
            let _ = task.await;
            Err(Error::unavailable(
                "Native output closure is unconfirmed after workload stop.",
            ))
        }
    }
}

fn completion_status(previous: i32, completion: block::Completion) -> (i32, bool) {
    (
        completion.exit_code(previous),
        completion == block::Completion::FailedFlush,
    )
}

fn exit_record(
    code: i32,
    finalization_failed: bool,
    terminal_reason: Option<crate::runner::TerminalReason>,
) -> String {
    if finalization_failed {
        return json!({ "StatusCode": code, "FinalizationFailed": true }).to_string();
    }
    // This function is called for a healthy final write only after the
    // workload, mount ownership and suspension accounting have drained.
    let mut record = json!({ "StatusCode": code, "PhysicalFinalized": true });
    if code == crate::runner::CONTROLLER_FAILED
        && let Some(reason) = terminal_reason
    {
        record["TerminalReason"] = json!(reason);
    }
    record.to_string()
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
    cleanup: super::cleanup::Registry,
    environments: Arc<Mutex<HashMap<String, crate::background_jobs::controller::Control>>>,
}

impl Controller {
    async fn environment(
        &self,
        id: &str,
        plan: &Value,
        expected_uid: u32,
        listener: tokio::net::UnixListener,
        control: crate::background_jobs::controller::Control,
        stop: CancellationToken,
    ) -> Result<()> {
        use crate::background_jobs::{
            channel::Channel,
            protocol::{Event as EnvironmentEvent, Request as EnvironmentRequest},
        };
        let (stream, _) = tokio::time::timeout(Duration::from_secs(30), listener.accept())
            .await
            .map_err(|_| Error::timeout("Execution environment did not connect."))??;
        let peer = stream.peer_cred()?;
        if peer.uid() != expected_uid {
            return Err(Error::forbidden(
                "Unexpected execution environment identity.",
            ));
        }
        let pid = peer
            .pid()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| Error::forbidden("Missing environment process identity."))?;
        let before = crate::recovery::process_identity(pid)
            .await?
            .ok_or_else(|| Error::forbidden("Environment process exited."))?;
        let membership = tokio::fs::read_to_string(format!("/proc/{pid}/cgroup")).await?;
        let expected_group = format!("{}/{}", systemd::workload_cgroup()?, systemd::unit(id)?);
        if !membership.lines().any(|line| {
            line.strip_prefix("0::").is_some_and(|group| {
                group == expected_group || group.starts_with(&format!("{expected_group}/"))
            })
        }) || crate::recovery::process_identity(pid).await?.as_ref() != Some(&before)
        {
            return Err(Error::forbidden(
                "Environment process is outside its attempt cgroup.",
            ));
        }
        let (channel, mut events) = Channel::attach(stream);
        let ready = tokio::time::timeout(Duration::from_secs(30), events.recv())
            .await
            .map_err(|_| Error::timeout("Execution environment did not become ready."))?;
        let Some(EnvironmentEvent::Ready {
            attempt_id,
            generation,
        }) = ready
        else {
            return Err(if ready.is_none() {
                channel.disconnect_error()
            } else {
                Error::bad_gateway("Invalid environment readiness.")
            });
        };
        if attempt_id != id {
            return Err(Error::forbidden("Environment attempt does not match."));
        }
        control.canonical_plan(plan.clone()).await;
        control.attach(channel.clone(), generation).await?;
        let turn = plan["turnId"]
            .as_str()
            .ok_or_else(|| Error::bad("Missing provider turn identity."))?;
        let initial = control
            .request(EnvironmentRequest::ResumeTurn {
                request_id: format!("start-{turn}"),
                turn_id: turn.into(),
                plan: plan.clone(),
            })
            .await?;
        if let EnvironmentEvent::Refused { message, .. } = initial {
            return Err(Error::unavailable(message));
        }
        loop {
            let event = tokio::select! { () = stop.cancelled() => return Ok(()), event = events.recv() => event };
            match event {
                Some(EnvironmentEvent::TurnStarted { turn_id }) => {
                    let _log = self.log_lock.lock().await;
                    let length = match tokio::fs::metadata(self.file(id, "log")).await {
                        Ok(metadata) => metadata.len(),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                        Err(error) => return Err(error.into()),
                    };
                    control.begin(&turn_id, length).await?;
                }
                Some(EnvironmentEvent::Output { stderr, data, .. }) => {
                    let bytes = STANDARD.decode(data).map_err(Error::internal)?;
                    self.append(id, stderr, &bytes, LogPurpose::Execution)
                        .await?;
                }
                Some(EnvironmentEvent::TurnFinished { turn_id, code }) => {
                    let _log = self.log_lock.lock().await;
                    let length = tokio::fs::metadata(self.file(id, "log")).await?.len();
                    control.finished(id, &turn_id, code, length).await?;
                }
                Some(EnvironmentEvent::TurnFailed { message, .. }) => {
                    return Err(Error::coded(
                        502,
                        crate::background_jobs::protocol::CONTROL_INTERRUPTED,
                        message,
                    ));
                }
                Some(EnvironmentEvent::JobChanged {
                    outcome: crate::background_jobs::Outcome::Unknown { message },
                    ..
                }) => {
                    return Err(Error::coded(
                        502,
                        crate::background_jobs::protocol::CONTROL_INTERRUPTED,
                        message,
                    ));
                }
                Some(_) => {}
                None => return Err(channel.disconnect_error()),
            }
        }
    }

    fn bases(&self) -> super::bases::Bases {
        super::bases::Bases {
            data: self.data.clone(),
            state: self.state.clone(),
            identity: (self.identity.uid, self.identity.gid),
            #[cfg(test)]
            build_entered: None,
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
        super::cleanup::check(&self.state, text(&plan, "runId"))?;
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
        let disk = self.start_storage(id, &plan).await?;
        trace.phase(Phase::PoolAccounting);
        let space = self.check_preparation_space(&plan).await?;
        let pool = workspace::data_root(&self.state);
        let filesystem = if pool.try_exists()? {
            &pool
        } else {
            &self.state
        };
        let preparation =
            disk::Preparation::reserve_on(&self.state, id, space.incoming, filesystem).await?;
        trace.phase(Phase::WorkspaceOwnership);
        let owner =
            Arc::new(workspace::execution_ownership(&self.state, text(&plan, "runId")).await?);
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
        // Keep deletion out until the environment owner and active receipt
        // are both visible. An admitted spare also keeps the gate until its
        // claim is recorded so a peer cannot claim or retire it.
        let spare_guard = space.spare.then_some(disk);
        let (used, observed) = space.usage_sample;
        let ready_usage = space
            .spare
            .then_some((used.saturating_add(space.incoming), observed));
        let (controller, id) = (self.clone(), id.to_owned());
        let observation = Arc::new(std::mem::take(observation));
        let (lease_controller, lease_id, lease_stop) =
            (controller.clone(), id.clone(), stop.clone());
        tokio::spawn(async move {
            lease_controller.watch_lease(&lease_id, lease_stop).await;
        });
        super::cleanup::spawn(async move {
            let execution_guard = Arc::new(Mutex::new(()));
            let launch_guard = Arc::new(Mutex::new(()));
            let execution_launch = launch_guard.clone();
            let execution_owner = execution_guard.clone().lock_owned().await;
            let execution_controller = controller.clone();
            let execution_id = id.clone();
            let execution_plan = plan.clone();
            let execution_stop = stop.clone();
            let execution_workspace = owner.clone();
            let execution_observation = observation.clone();
            let executing = super::cleanup::spawn(async move {
                let _execution_owner = execution_owner;
                execution_controller
                    .execute(
                        &execution_id,
                        &execution_plan,
                        execution_stop,
                        &execution_observation,
                        execution_workspace,
                        preparation,
                        spare_guard,
                        ready_usage,
                        execution_launch,
                    )
                    .await
            });
            let result = match executing {
                Ok(mut result) => {
                    tokio::select! {
                        result = &mut result => result.map_err(Error::internal).and_then(|result| result),
                        () = stop.cancelled() => {
                            match tokio::time::timeout(Duration::from_secs(20), result).await {
                                Ok(result) => result.map_err(Error::internal).and_then(|result| result),
                                Err(_) => Err(Error::unavailable("Native filesystem operation did not settle after cancellation; cleanup retains ownership.")),
                            }
                        }
                    }
                }
                Err(error) => Err(error),
            };
            let environment = controller.environments.lock().await.remove(&id);
            if let Some(environment) = environment {
                let message = result.as_ref().err().map_or_else(
                    || "Execution environment stopped.".to_owned(),
                    |error| error.message.clone(),
                );
                environment.fail(message).await;
            }

            if result.is_err() {
                observation.rejected();
            }
            // The observation ends at submission, or at an interrupted bootstrap;
            // it never waits for fencing, filesystem close, or a telemetry sink.
            drop(observation);
            // Reject in-flight file mutations before flushing/unmounting.
            // Already admitted access retains its attempt guard on its worker.
            stop.cancel();
            let launch_fence = launch_guard.lock().await;
            // The physical owner lock and active marker survive any uncertain stop.
            while let Err(error) = systemd::stop(&id).await {
                let _ = controller.abort_disk(&id).await;
                tracing::error!(attempt = id, message = %error.message, "Native workload fencing must be retried");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            drop(launch_fence);
            if let Err(error) =
                remove_environment_sockets(&controller.state.join("roots").join(&id)).await
            {
                tracing::warn!(attempt = id, message = %error.message, "Environment socket cleanup deferred to retirement");
            }
            let protection = controller
                .active
                .lock()
                .await
                .get_mut(&id)
                .and_then(|attempt| attempt.protection.take());
            let mut code = controller.execution_status(&id, result).await;
            let finalization_failed;
            let mounted = controller
                .mounted
                .lock()
                .await
                .get(text(&plan, "runId"))
                .cloned();
            let suspension = controller
                .active
                .lock()
                .await
                .get(&id)
                .expect("owned native attempt")
                .suspension
                .clone();
            let completion_lock = Arc::new(Mutex::new(()));
            let work = cleanup::CloseWork {
                protection,
                mounted,
                mounts: controller.mounted.clone(),
                storage_close: StorageClose::new(live_access),
                completion_lock: completion_lock.clone(),
                execution_guard,
                _owner: Some(owner.clone()),
                data: controller.data.clone(),
                exported: false,
                suspension,
                state: controller.state.clone(),
                run: text(&plan, "runId").into(),
                attempt: id.clone(),
                completion: None,
            };
            let closing =
                controller
                    .cleanup
                    .start(&controller.state, text(&plan, "runId"), &id, work);
            let outcome = match closing {
                Ok(mut ticket) => ticket.wait(super::cleanup::WAIT).await,
                Err(error) => super::cleanup::Outcome::Pending(error.message),
            };
            if let super::cleanup::Outcome::Pending(message) = outcome {
                let _completion = completion_lock.lock().await;
                while let Err(error) = controller.record_cleanup_pending(&id, &message).await {
                    tracing::error!(attempt = id, %error, "Native cleanup fence must be persisted before releasing workload accounting");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                let suspension = controller
                    .active
                    .lock()
                    .await
                    .get(&id)
                    .map(|attempt| attempt.suspension.clone());
                if let Some(suspension) = suspension {
                    let _ = tokio::time::timeout(Duration::from_secs(2), suspension.finish()).await;
                }
                if controller
                    .file(&id, "cleanup-result")
                    .try_exists()
                    .unwrap_or(false)
                {
                    let _ = tokio::fs::remove_file(controller.file(&id, "active")).await;
                }
                controller.active.lock().await.remove(&id);
                controller.capacity.lock().await.attempts.remove(&id);
                controller.leases.lock().await.remove(&id);
                controller.changed.notify_waiters();
                return;
            }
            // A worker only completes after the existing close phases and its
            // physical ownership have ended.
            controller.mounted.lock().await.remove(text(&plan, "runId"));
            match tokio::fs::read(controller.file(&id, "cleanup-result"))
                .await
                .ok()
                .and_then(|bytes| serde_json::from_slice::<cleanup::Completed>(&bytes).ok())
            {
                Some(completion) => {
                    (code, finalization_failed) = completion_status(code, completion.completion);
                }
                None => {
                    code = crate::runner::CONTROLLER_FAILED;
                    finalization_failed = true;
                }
            }
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
            let terminal_reason = controller.terminal_reason(&id).await;
            let exit = exit_record(code, finalization_failed, terminal_reason);
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
        })?;
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

    async fn execution_status(&self, id: &str, result: Result<i32>) -> i32 {
        match result {
            Ok(code) => code,
            Err(error) => {
                let message = format!("Native execution failed: {}\n", error.message);
                let _ = self
                    .append(id, true, message.as_bytes(), LogPurpose::Diagnostic)
                    .await;
                crate::runner::CONTROLLER_FAILED
            }
        }
    }

    /// This file belongs to the root-private controller state, outside the
    /// child's log and filesystem. Never decode an arbitrary error or payload.
    async fn terminal_reason(&self, id: &str) -> Option<crate::runner::TerminalReason> {
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.file(id, "terminal"))
            .await
            .ok()?;
        let metadata = file.metadata().await.ok()?;
        if !metadata.is_file() || metadata.len() > MAX_TERMINAL_METADATA {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_TERMINAL_METADATA + 1)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() as u64 > MAX_TERMINAL_METADATA {
            return None;
        }
        serde_json::from_slice::<TerminalMetadata>(&bytes)
            .ok()
            .map(|value| value.reason)
    }

    async fn record_terminal_reason(
        &self,
        id: &str,
        reason: crate::runner::TerminalReason,
    ) -> Result<()> {
        if self.terminal_reason(id).await == Some(reason) {
            return Ok(());
        }
        let metadata = serde_json::to_vec(&TerminalMetadata { reason })?;
        atomic_write(&self.file(id, "terminal"), &metadata).await
    }

    async fn append(
        &self,
        id: &str,
        stderr: bool,
        bytes: &[u8],
        purpose: LogPurpose,
    ) -> Result<()> {
        let _log = self.log_lock.lock().await;
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(self.file(id, "log"))
            .await?;
        if file.metadata().await?.len() > MAX_LOG {
            let reason = crate::runner::TerminalReason::NativeOutputLimit;
            // A failed best-effort diagnostic cannot become the initiating cause.
            // One fixed metadata write at an execution guard, never per frame.
            if purpose == LogPurpose::Execution
                && let Err(error) = self.record_terminal_reason(id, reason).await
            {
                tracing::warn!(
                    status = error.status,
                    "Native terminal reason could not be saved"
                );
            }
            // A metadata I/O failure must not replace the original budget error.
            return Err(Error::bad(reason.message()));
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
            self.append(id, stderr, &buffer[..count], LogPurpose::Execution)
                .await?;
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
    ) -> Result<(bool, block::Mounted)> {
        self.mount_after_release_observed(
            id,
            plan,
            stop,
            wait,
            &bootstrap::Trace::default(),
            None,
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn mount_after_release_observed(
        &self,
        id: &str,
        plan: &Value,
        stop: &CancellationToken,
        wait: Duration,
        trace: &bootstrap::Trace,
        workspace_owner: Option<Arc<crate::file_lock::Guard>>,
        reservation: Option<&disk::Preparation>,
        mut spare_guard: Option<disk::StorageGuard>,
    ) -> Result<(bool, block::Mounted)> {
        let (deadline, time_limited) = disk_admission_deadline(plan, wait);
        let require_spare = spare_guard.is_some();
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
            let owner = if let Some(owner) = spare_guard.take() {
                owner
            } else {
                tokio::select! {
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
                if workspace_owner.is_none() {
                    self.check_preparation_space(plan).await?;
                }
                trace.phase(Phase::FilesystemInitialize);
                let from_base = self
                    .bases()
                    .initialize_reserved(plan, stop, reservation, Some(owner), require_spare)
                    .await?;
                trace.phase(Phase::Mount);
                let state = self.state.clone();
                let owned_plan = plan.clone();
                let owned_stop = stop.clone();
                let receive = super::cleanup::prepare(workspace_owner.clone(), move || {
                    Box::pin(async move {
                        block::mount(&state, text(&owned_plan, "runId"), &owned_plan, &owned_stop)
                            .await
                    })
                })?;
                let mut receive = receive;
                let mounted = match tokio::time::timeout_at(deadline, &mut receive).await {
                    Ok(result) => result.map_err(Error::internal)??,
                    Err(_) => {
                        stop.cancel();
                        receive.await.map_err(Error::internal)??
                    }
                };
                Ok((from_base, mounted))
            }
            .await;
            match result {
                Ok((from_base, mounted)) => return Ok((from_base, mounted)),
                Err(error) if error.is_conflict() && error.message == block::DISK_BUSY => {
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
                                LogPurpose::Diagnostic,
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
        let _ = self
            .append(id, true, message.as_bytes(), LogPurpose::Diagnostic)
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_files(
        &self,
        plan: Value,
        identity: super::identity::Identity,
        storage_identity: super::identity::Identity,
        from_base: bool,
        data: PathBuf,
        mounted: Arc<block::Handle>,
        trace: bootstrap::Trace,
        stop: CancellationToken,
        reservation: disk::Preparation,
        ready_usage: Option<(u64, std::time::Instant)>,
    ) -> Result<Vec<super::home::Binding>> {
        let controller = self.clone();
        let ingress = mounted.ingress.clone().lock_owned().await;
        let reserved = reservation.bytes;
        let allocation = Arc::new(reservation);
        let reservation = allocation.clone();
        super::ingress::write((mounted.clone(), ingress, reservation), async move {
            let _mounted = mounted.mount.lock().await;
            let policy = storage::policy::for_node(&plan["storage"]["policy"])?;
            let limit = controller.disk_limit().await?;
            let admission = super::ingress::Admission::project(
                &controller.data,
                &controller.state,
                workspace::workload(&controller.state, text(&plan, "runId"))?,
                limit,
                &policy,
                ready_usage,
            )?;
            // The admission has just taken a full usage sample; reuse it.
            let growth = disk::available(limit, admission.sampled_usage());
            let check = std::sync::Arc::new(std::sync::Mutex::new(
                admission.with_reservation(&allocation, true)?,
            ));
            let copied = check.clone();
            let admit = std::sync::Arc::new(std::sync::Mutex::new(move |bytes| {
                if stop.is_cancelled() {
                    return Err(Error::unavailable("Native preparation stopped."));
                }
                copied.lock().map_err(Error::internal)?.admit(bytes)
            }));
            trace.phase(Phase::FilesystemInitialize);
            super::block::initialize_admitted(
                &controller.state,
                text(&plan, "runId"),
                &plan,
                controller
                    .disk_budget(&plan)
                    .min(growth.saturating_add(reserved)),
                admit,
            )
            .await?;
            trace.phase(Phase::TemporaryPrepare);
            let temporary = block::temporary(&controller.state, text(&plan, "runId")).await?;
            if temporary.exists() {
                tokio::fs::remove_dir_all(&temporary).await?;
            }
            check.lock().map_err(Error::internal)?.admit(5120)?;
            private_dir(&temporary).await?;
            workspace::own_tree(&temporary, identity.uid(), identity.gid()).await?;
            let stage = controller.data.join("runs").join(text(&plan, "runId"));
            if from_base
                && (storage_identity.uid(), storage_identity.gid())
                    == (controller.identity.uid, controller.identity.gid)
            {
                super::identity::mark_base_owner(&data, &storage_identity).await?;
            }
            let bindings = super::home::prepare_for_identity_admitted(
                &stage.join("home"),
                &data,
                &identity,
                &storage_identity,
                &trace,
                &mut |bytes| check.lock().map_err(Error::internal)?.admit(bytes),
            )
            .await?;
            let view = block::view(&controller.state, text(&plan, "runId")).await?;
            let administrator = plan["nativeMode"]["mode"] == "administrator";
            let docker_data = if administrator {
                view.docker_data()?.with_file_name("docker-administrator")
            } else {
                view.docker_data()?
            };
            check.lock().map_err(Error::internal)?.admit(5120)?;
            tokio::fs::create_dir_all(&docker_data).await?;
            std::os::unix::fs::lchown(
                &docker_data,
                Some(if administrator {
                    0
                } else {
                    storage_identity.root_uid
                }),
                Some(if administrator {
                    0
                } else {
                    storage_identity.root_gid
                }),
            )?;
            tokio::fs::set_permissions(&docker_data, std::fs::Permissions::from_mode(0o700))
                .await?;
            Ok(bindings)
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute(
        &self,
        id: &str,
        plan: &Value,
        stop: CancellationToken,
        observation: &bootstrap::Scope,
        owner: Arc<crate::file_lock::Guard>,
        preparation: disk::Preparation,
        spare_guard: Option<disk::StorageGuard>,
        ready_usage: Option<(u64, std::time::Instant)>,
        launch_guard: Arc<Mutex<()>>,
    ) -> Result<i32> {
        let trace = observation.trace();
        let root = workspace::directory(&self.state, text(plan, "runId"))?;
        if stop.is_cancelled() {
            return Ok(143);
        }
        atomic_write(&root.join("attempt"), id.as_bytes()).await?;
        atomic_write(
            &root.join("resources.json"),
            &serde_json::to_vec(&plan["resources"])?,
        )
        .await?;
        let mut execution_plan = plan.clone();
        execution_plan["guestDataRoot"] = guest_data(plan, &self.data)
            .to_string_lossy()
            .into_owned()
            .into();
        let plan = &execution_plan;
        let runtime = plan["runtimeId"]
            .as_str()
            .ok_or_else(|| Error::bad("Missing pinned native runtime."))?;
        super::systemd::tools(runtime)?;
        let (from_base, mounted) = self
            .mount_after_release_observed(
                id,
                plan,
                &stop,
                DISK_RELEASE_WAIT,
                &trace,
                Some(owner),
                Some(&preparation),
                spare_guard,
            )
            .await?;
        let mounted = Arc::new(block::Handle::new(mounted));
        self.mounted
            .lock()
            .await
            .insert(text(plan, "runId").to_owned(), mounted.clone());
        if stop.is_cancelled() {
            return Ok(143);
        }
        let administrator = plan["nativeMode"]["mode"] == "administrator";
        trace.phase(Phase::IdentityProvision);
        let identity = super::identity::provision(&self.state, text(plan, "runId"), &stop).await;
        if stop.is_cancelled() {
            return Ok(143);
        }
        let mut identity = identity?;
        if administrator {
            identity.host_engine = Some((self.identity.uid, self.identity.gid));
        }
        let view = block::view(&self.state, text(plan, "runId")).await?;
        let data = view.filesystems[0].clone();
        let pool = workspace::data_root(&self.state);
        let mapping_supported = !administrator
            && super::quota::reflink(&std::fs::File::open(&pool)?)?
            && super::identity::idmapped_mounts_supported(&pool, &identity).await;
        let previous = if mapping_supported {
            super::identity::stored_owner(&data).await?
        } else {
            None
        };
        // Reassigning existing protected files to a different backing range
        // could map unrelated host IDs. Keep the original map. A previous
        // administrator owner first follows the journaled protected transition.
        let idmapped = mapping_supported
            && previous
                .as_ref()
                .is_none_or(|owner| owner.host_engine.is_none() || owner.is_storage());
        let storage_identity = if idmapped {
            previous.unwrap_or_else(|| {
                super::identity::Identity::storage((self.identity.uid, self.identity.gid))
            })
        } else {
            identity.clone()
        };
        let bindings = self
            .prepare_files(
                plan.clone(),
                identity.clone(),
                storage_identity.clone(),
                from_base,
                data.clone(),
                mounted,
                trace.clone(),
                stop.clone(),
                preparation,
                ready_usage,
            )
            .await?;
        let stage = self.data.join("runs").join(text(plan, "runId"));
        let temporary = block::temporary(&self.state, text(plan, "runId")).await?;
        let docker_data = if administrator {
            view.docker_data()?.with_file_name("docker-administrator")
        } else {
            view.docker_data()?
        };
        // Keep the existing prepared-base admission marker. The authoritative
        // ownership mapping is stored inside the local filesystem.
        atomic_write(
            &root.join("file-owner.json"),
            &serde_json::to_vec(&identity)?,
        )
        .await?;
        trace.phase(Phase::RuntimePrepare);

        let runtime = root.join("docker-runtime");
        if runtime.exists() {
            tokio::fs::remove_dir_all(&runtime).await?;
        }
        for name in ["runtime", "rootlesskit", "plugins"] {
            let path = runtime.join(name);
            private_dir(&path).await?;
            std::os::unix::fs::lchown(
                &path,
                Some(if administrator { 0 } else { identity.root_uid }),
                Some(if administrator {
                    self.identity.gid
                } else {
                    identity.root_gid
                }),
            )?;
            if administrator {
                tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o710)).await?;
            }
        }
        if administrator {
            if plan["environmentProtocol"] == 1 {
                let activation = runtime
                    .join("runtime")
                    .join(super::docker::administrator::ACTIVATION_DIRECTORY);
                private_dir(&activation).await?;
                std::os::unix::fs::lchown(&activation, Some(identity.uid()), Some(identity.gid()))?;
            }
            atomic_write(&runtime.join("runtime/daemon.json"), b"{}").await?;
            let wrapper = runtime.join("docker");
            atomic_write(&wrapper, super::docker::administrator::WRAPPER.as_bytes()).await?;
            tokio::fs::set_permissions(wrapper, std::fs::Permissions::from_mode(0o755)).await?;
        }
        {
            let _pool = disk::lock(&self.state).await?;
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
        if administrator {
            plan["nativeDockerGid"] = self.identity.gid.into();
        }
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
            // Preserve administrator's host execution and existing local relay.
            // Managed rootful Docker uses its own store and the workload cgroup.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let origin = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
            for field in MCP_FIELDS {
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
                mcp_stop.clone(),
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
        let auth = if plan["environmentProtocol"] == 1 {
            let relay_root = container.with_extension("r");
            private_dir(&relay_root).await?;
            tokio::fs::set_permissions(&relay_root, std::fs::Permissions::from_mode(0o711)).await?;
            let auth_proxy = if let Some(destination) = auth {
                let proxy = relay_root.join("auth.sock");
                let listener = tokio::net::UnixListener::bind(&proxy)?;
                std::os::unix::fs::chown(&proxy, Some(identity.uid()), Some(identity.gid()))?;
                tokio::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o600)).await?;
                tokio::spawn(relay_unix(listener, destination, mcp_stop.clone()));
                Some(proxy)
            } else {
                None
            };
            if !administrator {
                let proxy = relay_root.join("mcp.sock");
                let listener = tokio::net::UnixListener::bind(&proxy)?;
                std::os::unix::fs::chown(&proxy, Some(identity.root_uid), Some(identity.root_gid))?;
                tokio::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o600)).await?;
                tokio::spawn(relay_unix(listener, channel.clone(), mcp_stop.clone()));
            }
            auth_proxy
        } else {
            auth
        };
        let plan_file = container.with_extension("plan.json");
        let encoded_plan = serde_json::to_vec(&plan)?;
        trace.phase(Phase::PoolAccounting);
        if encoded_plan.len() as u64 > self.disk_growth().await? {
            return Err(Error::conflict(
                "Insufficient shared native disk budget for the execution plan; local work is retained.",
            ));
        }
        trace.phase(Phase::RuntimePrepare);
        atomic_write(&plan_file, &encoded_plan).await?;
        tokio::fs::set_permissions(&plan_file, std::fs::Permissions::from_mode(0o444)).await?;
        // The staging inbox is readonly in the workload's mount namespace.
        private_dir(&stage.join("chat-input")).await?;
        crate::nodes::workspace::prepare_native_inbox(&stage.join("chat-input")).await?;
        let mapped = if idmapped {
            Some(
                super::identity::Mapped::attach(
                    &self.state,
                    id,
                    &data,
                    &storage_identity,
                    &identity,
                )
                .await?,
            )
        } else {
            None
        };
        let mapped_view = mapped
            .as_ref()
            .map(|mounted| super::block::View::at(mounted.path()));
        let launch_view = mapped_view.as_ref().unwrap_or(&view);
        let docker_data = if idmapped {
            launch_view.docker_data()?
        } else {
            docker_data
        };
        let launch = systemd::Launch {
            id,
            plan: &plan,
            data: &self.data,
            root: &container,
            home: &launch_view.home,
            workspace: &launch_view.workspace,
            output: &launch_view.output,
            identity: &self.identity,
            conversation: (!administrator).then_some(&identity),
            docker_data: Some(docker_data.as_path()),
            docker_runtime: Some(runtime.as_path()),
            temporary: &temporary,
            auth: auth.as_deref(),
        };
        let environment = if plan["environmentProtocol"] == 1 {
            if stop.is_cancelled() {
                return Err(Error::conflict("Environment stopped before admission."));
            }
            let directory = container.with_extension("e");
            private_dir(&directory).await?;
            // Keep the directory controller-owned: another same-UID run must
            // not be able to replace the listener. Only the admitted UID may connect.
            tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o711)).await?;
            let socket = directory.join("control.sock");
            let listener = tokio::net::UnixListener::bind(&socket)?;
            let uid = if administrator {
                self.identity.uid
            } else {
                identity.uid()
            };
            std::os::unix::fs::chown(&socket, Some(uid), None)?;
            tokio::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).await?;
            let control = crate::background_jobs::controller::Control::default();
            let bindings: Vec<super::home::Binding> =
                serde_json::from_value(plan["nativeHomeBindings"].clone())?;
            control.bind_native_files(&bindings).await?;
            self.environments
                .lock()
                .await
                .insert(id.into(), control.clone());
            Some((listener, control, uid))
        } else {
            None
        };
        let arguments = systemd::arguments(&launch)?;
        let launch_fence = launch_guard.lock().await;
        if !self.leased(id).await || stop.is_cancelled() {
            return Ok(143);
        }
        if let Some(attempt) = self.active.lock().await.get_mut(id) {
            attempt.started = true;
        }
        trace.phase(Phase::WorkloadLaunch);
        systemd::start_guardian(id, &self.file(id, "lease")).await?;
        if stop.is_cancelled() {
            return Ok(143);
        }
        if let Some(mapped) = mapped {
            mapped.retain_for_unit();
        }
        let mut child = systemd::spawn_prepared(arguments)?;
        // Keep launch admission until systemd observed the submitted unit or
        // the submitting process has exited. Cancellation cannot attest an
        // absent unit while systemd-run may still enqueue it later.
        let startup_wait = CancellationToken::new();
        let started = tokio::select! {
            result = systemd::wait_started(id, &startup_wait) => result,
            result = child.wait() => result.map(|_| ()).map_err(Error::from),
        };
        if let Err(error) = started {
            let _ = child.kill().await;
            let _ = child.wait().await;
            drop(launch_fence);
            return Err(error);
        }
        drop(launch_fence);
        let environment_task = if let Some((listener, control, uid)) = environment {
            let controller = self.clone();
            let id = id.to_owned();
            let plan = plan.clone();
            let stop = stop.clone();
            Some(tokio::spawn(async move {
                if let Err(error) = controller
                    .environment(&id, &plan, uid, listener, control.clone(), stop.clone())
                    .await
                {
                    control.fail_with(error).await;
                    stop.cancel();
                }
            }))
        } else {
            None
        };
        // The submission has settled; coding-engine readiness and first
        // output retain their separate protocol observations.
        observation.submitted();
        if from_base && plan["nativeProjectArchives"].is_array() {
            let _ = self
                .base_warmer
                .enqueue(
                    self.bases(),
                    json!({
                        "archives": plan["nativeProjectArchives"],
                        "diskMiB": plan["resources"]["diskMiB"],
                    }),
                    self.budget.clone(),
                    self.stop.clone(),
                )
                .await;
        }
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
                    let (workspace_bytes, temporary_bytes) = block::usage(&self.state, text(&plan, "runId")).await?;
                    let conversation_bytes = workspace_bytes.saturating_add(temporary_bytes);
                    let conversation_limit = self.monitor_capacity(id, &plan, workspace_bytes, temporary_bytes).await?;
                    let node_bytes = disk::actual_used(&self.data, &self.state).await?;
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
        stop.cancel();
        if let Some(task) = environment_task {
            task.abort();
            let _ = task.await;
        }
        let control = self.environments.lock().await.remove(id);
        if let Some(control) = control {
            control.fail("Execution environment stopped.".into()).await;
        }
        let mounted = { self.mounted.lock().await.get(text(&plan, "runId")).cloned() };
        let consumer_result = if let Some(mounted) = mounted {
            mounted.fence_consumers().await
        } else {
            Ok(())
        };
        // A timeout never proves termination. Retain this execution's locks
        // while its exact supervisor child settles, even after SIGKILL.
        settle_client(&mut child, id).await;
        let stdout_result = if readers & 1 == 0 {
            settle_output(&mut out, Duration::from_secs(5)).await
        } else {
            Ok(())
        };
        let stderr_result = if readers & 2 == 0 {
            settle_output(&mut err, Duration::from_secs(5)).await
        } else {
            Ok(())
        };
        consumer_result?;
        stdout_result?;
        stderr_result?;
        let _ = tokio::fs::remove_file(plan_file).await;
        remove_environment_sockets(&container).await?;
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
        if let Some(mounted) = &mounted {
            // Apply current grants even while geometry growth retries. Managed
            // /tmp shares the task allowance and cannot finance journal writes.
            mounted.volume.limit_backing(allowance)?;
        }
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
            self.append(
                id,
                false,
                format!("{event}\n").as_bytes(),
                LogPurpose::Execution,
            )
            .await?;
        }
        if negotiated
            && allowance > filesystem
            && let Some(mounted) = mounted
        {
            let attempt = id.to_owned();
            // The lifecycle lock pins the mount through the metadata commit and
            // resize, while supervision remains responsive to lease/cancellation.
            let _ = super::cleanup::spawn(async move {
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
        let used = disk::actual_used(&self.data, &self.state).await?;
        Ok(json!({
            "memoryMiB": 0,
            "diskMiB": used.div_ceil(disk::MIB),
            "diskBytes": used,
        }))
    }

    /// Ceilings are not preallocated reservations. Charge actual retained
    /// payloads, then bound every additional copy by the remaining shared pool.
    async fn check_preparation_space(&self, plan: &Value) -> Result<PreparationSpace> {
        let run = text(plan, "runId");
        let root = workspace::directory(&self.state, run)?;
        let has_journal = block::exists(&self.state, run).await?;
        let current = if has_journal {
            0
        } else {
            disk::bytes_if_present(&root.join("data")).await?
        };
        let assigned_base = has_journal && root.join("file-owner.json").exists();
        let mut base_key = None;
        let mut spare_key = None;
        let mut project_bytes = None;
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
                    base_key = Some(base.key.clone());
                    if !has_journal && self.bases().claimable_spare(run, &base.key)? {
                        spare_key = Some(base.key);
                        0
                    } else {
                        project_bytes = Some(base.metadata_bytes);
                        base.allocated_for_clone(self.bases().reflink_enabled().await?)
                    }
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
        let private_bytes = current
            .saturating_add(project_bytes.unwrap_or(seed))
            .saturating_add(home);
        if private_bytes > self.disk_budget(plan) {
            return Err(Error::conflict(
                "Retained native work leaves insufficient disk allowance for this preparation.",
            ));
        }
        // Callers hold the storage gate, so no reservation can be published
        // between samples: one full scan suffices unless reclamation changed
        // the pool.
        let limit = self.disk_limit().await?;
        let observed = std::time::Instant::now();
        let mut used = disk::used(&self.data, &self.state).await?;
        let growth = disk::available(limit, used);
        let policy = storage::policy::for_node(&plan["storage"]["policy"])?;
        let headroom = || async {
            let (state, policy) = (self.state.clone(), policy.clone());
            let sample = tokio::task::spawn_blocking(move || {
                if workspace::data_root(&state).try_exists()? {
                    disk::data_pool_headroom(&state, &policy, None)
                } else {
                    Ok(u64::MAX)
                }
            });
            tokio::time::timeout(Duration::from_secs(5), sample)
                .await
                .map_err(|_| Error::unavailable("Native data pool capacity observation is stale."))?
                .map_err(Error::internal)?
        };
        let physical_need = incoming.saturating_add(disk::MIB);
        let mut physical = headroom().await?;
        if (incoming > growth || physical_need > physical)
            && super::spares::discard_unclaimed(&self.state, spare_key.as_deref()).await?
        {
            used = disk::used(&self.data, &self.state).await?;
            physical = headroom().await?;
        }
        if physical_need > physical {
            // Cache allocation can be shared with retained reflink clones.
            // Recheck actual free space before relying on an eviction's credit.
            let bases = self.bases();
            loop {
                let credited = bases
                    .trim(u64::MAX, physical_need - physical, base_key.as_deref())
                    .await?;
                physical = headroom().await?;
                if physical >= physical_need || credited == 0 {
                    break;
                }
            }
            if physical < physical_need {
                return Err(Error::new(
                    507,
                    "Native data pool lacks preparation headroom.",
                ));
            }
            used = disk::used(&self.data, &self.state).await?;
        }
        let growth = disk::available(limit, used);
        if incoming > growth {
            let reclaimed =
                crate::project_archives::reclaim_for_foreground(&self.data, incoming - growth, &[])
                    .await?;
            if reclaimed > 0 {
                used = disk::used(&self.data, &self.state).await?;
            }
        }
        if incoming > disk::available(limit, used) || used > limit {
            return Err(Error::conflict(
                "Retained native work leaves insufficient shared disk budget for this preparation.",
            ));
        }
        Ok(PreparationSpace {
            incoming,
            spare: spare_key.is_some(),
            usage_sample: (used, observed),
        })
    }

    /// Admission is global before it is attempt-scoped: a queued pool waiter
    /// must never retain the guard needed by an unrelated physical close.
    async fn with_live_access<T: Send + 'static, F, Fut>(
        &self,
        id: &str,
        requested_run: Value,
        forbidden: &'static str,
        operation: F,
    ) -> Result<T>
    where
        F: FnOnce(Self, String, CancellationToken, tokio::sync::OwnedMutexGuard<()>) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        uuid(id)?;
        let pool = disk::lock(&self.state).await?;
        let live_access = {
            let active = self.active.lock().await;
            let attempt = active
                .get(id)
                .ok_or_else(|| Error::conflict("Native attempt is not active."))?;
            if !attempt.started {
                return Err(Error::conflict(
                    "Native attempt is still preparing its filesystem.",
                ));
            }
            if requested_run != attempt.run {
                return Err(Error::forbidden(forbidden));
            }
            attempt.live_access.clone()
        };
        drop(pool);
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
        operation(self.clone(), run, stop, admitted).await
    }

    async fn reserve_copy(
        &self,
        maximum: u64,
        filesystem: &Path,
        policy: &storage::policy::Policy,
    ) -> Result<(disk::Preparation, u64)> {
        let _pool = disk::lock(&self.state).await?;
        let physical = disk::physical_headroom(&self.state, filesystem, policy, None)?;
        let budget = maximum
            .min(self.disk_growth().await?)
            .min(physical.saturating_sub(disk::MIB));
        let reservation =
            disk::Preparation::reserve_on(&self.state, &crate::config::id(), budget, filesystem)
                .await?;
        Ok((reservation, budget))
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
            move |controller, run, stop, admitted| async move {
                let plan = controller.plan(&attempt).await?;
                let view = block::view(&controller.state, &run).await?;
                let path = Path::new(text(&value, "path"));
                let project_root = Path::new(text(&plan, "projectRoot"));
                let (source, allowed) = if let Ok(relative) = path.strip_prefix(project_root) {
                    (view.workspace.join(relative), view.workspace.clone())
                } else if let Ok(relative) = path.strip_prefix(
                    guest_data(&plan, &controller.data)
                        .join("runs")
                        .join(&run)
                        .join("output"),
                ) {
                    (view.output.join(relative), view.output.clone())
                } else if let Ok(relative) = path.strip_prefix("/tmp") {
                    let temporary = block::temporary(&controller.state, &run).await?;
                    (temporary.join(relative), temporary)
                } else {
                    return Err(Error::forbidden(
                        "Native artifacts must be in this conversation's workspace, output directory or /tmp.",
                    ));
                };
                let exports = controller.state.join("exports");
                let policy = storage::policy::for_node(&plan["storage"]["policy"])?;
                let limit = controller.disk_limit().await?;
                let mut admission = super::ingress::Admission::new(&controller.data, &controller.state, &exports, &controller.state, limit, &policy)?;
                admission.admit(5120)?;
                private_dir(&exports).await?;
                let (reservation, budget) = controller.reserve_copy(crate::artifacts::file::MAX_FILE, &exports, &policy).await?;
                let mut admission = super::ingress::Admission::new(&controller.data, &controller.state, &exports, &exports, limit, &policy)?.with_reservation(&reservation, false)?;
                let copying_stop = stop.clone();
                let (snapshot, size) = super::ingress::write((reservation, admitted), async move {
                    crate::artifacts::file::snapshot_in(&source, &allowed, &exports, budget, |bytes| {
                        if copying_stop.is_cancelled() { return Err(Error::forbidden("Native artifact export stopped.")); }
                        admission.admit(bytes)
                    }).await
                }).await?;
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
            move |controller, run, stop, admitted| {
                super::ingress::write(admitted, async move {
                    let plan = controller.plan(&attempt).await?;
                    if stop.is_cancelled() {
                        return Err(Error::conflict("Native project import stopped."));
                    }
                    let manager = guest_data(&plan, &controller.data);
                    let expected = crate::execution::project_seed_path(
                        &manager.join("runs").join(&run),
                        &plan,
                        &project,
                    )?;
                    if value["source"] != expected.to_string_lossy().as_ref()
                        || value["target"] != expected.to_string_lossy().as_ref()
                    {
                        return Err(Error::forbidden("Invalid native project path."));
                    }
                    let relative = expected.strip_prefix(&manager).map_err(Error::internal)?;
                    let source = controller.data.join(relative);
                    if tokio::fs::canonicalize(&source).await? != source {
                        return Err(Error::bad("Native project seed changed location."));
                    }
                    let view = block::view(&controller.state, &run).await?;
                    let target = view.workspace.join(&project);
                    if !target.exists() {
                        let data = view.filesystem_of(&view.workspace)?;
                        // Stage on the mounted filesystem so installation is an atomic
                        // rename, and do not walk all lazy files to estimate free space.
                        let staged = tempfile::tempdir_in(data)?;
                        let received = staged.path().join("project");
                        let remaining = block::free_bytes(data)?;
                        let pool = workspace::data_root(&controller.state);
                        let policy = storage::policy::for_node(&plan["storage"]["policy"])?;
                        let (reservation, budget) =
                            controller.reserve_copy(remaining, &pool, &policy).await?;
                        let mut admission = super::ingress::Admission::new(
                            &controller.data,
                            &controller.state,
                            staged.path(),
                            &workspace::data_root(&controller.state),
                            controller.disk_limit().await?,
                            &policy,
                        )?
                        .with_reservation(&reservation, true)?;
                        workspace::copy_admitted(&source, &received, budget, &mut |bytes| {
                            if stop.is_cancelled() {
                                return Err(Error::conflict("Native project import stopped."));
                            }
                            admission.admit(bytes)
                        })
                        .await?;
                        if stop.is_cancelled() {
                            return Err(Error::conflict("Native project import stopped."));
                        }
                        let mut identity =
                            super::identity::provision(&controller.state, &run, &stop).await?;
                        if plan["nativeMode"]["mode"] == "administrator" {
                            identity.host_engine =
                                Some((controller.identity.uid, controller.identity.gid));
                        }
                        let stored = super::identity::stored_owner(data)
                            .await?
                            .unwrap_or(identity);
                        workspace::own_tree(&received, stored.uid(), stored.gid()).await?;
                        if stop.is_cancelled() {
                            return Err(Error::conflict("Native project import stopped."));
                        }
                        tokio::fs::rename(received, &target).await?;
                    }
                    Ok(json!({ "ok": true }))
                })
            },
        )
        .await
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
        {
            let _admission = self.admission.lock().await;
        }
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
    fn wait(&self, id: &str, observe_retirement: bool) -> Response {
        let stream = futures_util::stream::unfold(
            (self.clone(), id.to_owned(), false),
            move |(controller, id, done)| async move {
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
                    let result = code
                        .parse::<i32>()
                        .map(|code| json!({ "StatusCode": code }))
                        .or_else(|_| serde_json::from_str::<Value>(&code));
                    let result = match result {
                        Ok(value) if observe_retirement => Ok(Bytes::from(
                            controller
                                .reconcile_wait_result(&id, value)
                                .await
                                .to_string(),
                        )),
                        Ok(value) => Ok(Bytes::from(value.to_string())),
                        Err(error) => Err(std::io::Error::other(error)),
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

/// The chat plan fields where coding agents carry MCP server URLs.
const MCP_FIELDS: [&str; 5] = [
    "codexConfig",
    "claudeMcps",
    "opencodeConfig",
    "antigravityMcp",
    "args",
];

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

/// A stable, controller-owned socket reconnects each request to the current
/// turn's broker. The destination can be rebound without a stale bind inode.
async fn relay_unix(listener: tokio::net::UnixListener, target: PathBuf, stop: CancellationToken) {
    let _ = crate::microvm::listener::serve(
        || listener.accept(),
        |(mut client, _)| {
            let target = target.clone();
            let stop = stop.clone();
            async move {
                if let Ok(mut server) = tokio::net::UnixStream::connect(target).await {
                    tokio::select! {
                        () = stop.cancelled() => {},
                        _ = tokio::io::copy_bidirectional(&mut client,&mut server) => {},
                    }
                }
            }
        },
        32,
        stop.clone(),
    )
    .await;
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
        let sample = async {
            let limit = controller.disk_limit().await?;
            let used = disk::used(&controller.data, &controller.state).await?;
            Ok::<_, Error>(disk::StagingSample {
                available_bytes: disk::available(limit, used),
                used_bytes: Some(used),
            })
        }
        .await;
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_secs(1) || sample.is_err() {
            tracing::info!(
                target: "cairn_performance",
                operation = "native_staging_budget",
                elapsed_ms = elapsed.as_millis() as u64,
                status = sample.as_ref().map_or_else(|error| error.status, |_| 200),
                "Authoritative native staging budget observation finished"
            );
        }
        return Ok(Json(sample?).into_response());
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
                super::spares::discard_unclaimed(&controller.state, None).await?;
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
            controller.stopped_result(id).await
        }
        ("POST", ["runs", id, "wait"]) => {
            uuid(id)?;
            let observe_retirement =
                request.uri().query() == Some(crate::runner::NATIVE_CLEANUP_RETIREMENT_QUERY);
            return Ok(controller.wait(id, observe_retirement));
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
            let mut status = block::status(&controller.state, run).await?;
            if let Some(mounted) = controller.mounted.lock().await.get(*run) {
                status["paused"] = mounted.volume.paused().into();
                status["waitingFor"] = mounted.volume.cpu_pause_reason()?.into();
            }
            status
        }
        ("POST", ["disks", run, action @ ("delete" | "prune")]) => {
            let _disk = disk::lock(&controller.state).await?;
            uuid(run)?;
            let _staging = if *action == "delete" {
                Some(disk::lock_staging(&controller.state, run).await?)
            } else {
                None
            };
            super::cleanup::check(&controller.state, run)?;
            let value = input(request, 4096).await?;
            let retirement = if *action == "delete" {
                crate::storage::retirement::Retirement::begin(&controller.state, run, &value)
                    .await?
            } else {
                None
            };
            if let Some(retirement) = &retirement
                && retirement.completed().await?
            {
                erase_native_attempts(&controller, run, retirement.attempts(), true).await?;
                return Ok(
                    Json(json!({ "deleted": true, "retirementId": retirement.id() }))
                        .into_response(),
                );
            }
            let root = workspace::directory(&controller.state, run)?;
            private_dir(&root).await?;
            let _owner = crate::file_lock::exclusive(
                &root.join("owner.lock"),
                "Native workspace is active.",
            )?;
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
                    "workload",
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
                    "directory.json",
                    "directory-conversion",
                    "directory-conversion-verified",
                    "base.partial",
                    "view.json",
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
                if stage.exists()
                    && !stage
                        .join(crate::conversation_deletion::retired::MANAGER_NAMESPACE)
                        .exists()
                {
                    tokio::fs::remove_dir_all(&stage).await?;
                    tokio::fs::File::open(stage.parent().unwrap())
                        .await?
                        .sync_all()
                        .await?;
                }
                erase_native_attempts(
                    &controller,
                    run,
                    retirement.as_ref().map_or(&[], |value| value.attempts()),
                    false,
                )
                .await?;
            }
            for directory in [
                controller.state.join("roots"),
                controller.data.join("runner-plans"),
            ] {
                if directory.is_dir() {
                    tokio::fs::File::open(directory).await?.sync_all().await?;
                }
            }
            if let Some(retirement) = &retirement {
                retirement.commit(&root).await?;
            }
            json!({
                "deleted": *action == "delete",
                "pruned": *action == "prune",
                "retiredGrants": retired_grants,
                "retirementId": retirement.as_ref().map(crate::storage::retirement::Retirement::id),
            })
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
        ("POST", ["runs", id, "environment"]) => {
            let value = input(request, 2_000_000).await?;
            let operation = serde_json::from_value(value)?;
            let control = controller
                .environments
                .lock()
                .await
                .get(*id)
                .cloned()
                .ok_or_else(|| Error::unavailable("Execution environment is not active."))?;
            serde_json::to_value(control.request(operation).await?)?
        }
        ("GET", ["runs", id, "turn", turn]) | ("GET", ["runs", id, "turn", turn, "start"]) => {
            crate::validation::uuid(turn)?;
            let control = controller
                .environments
                .lock()
                .await
                .get(*id)
                .cloned()
                .ok_or_else(|| Error::unavailable("Execution environment is not active."))?;
            if parts.last() == Some(&"start") {
                match tokio::time::timeout(Duration::from_secs(20), control.start_offset(turn))
                    .await
                {
                    Ok(offset) => json!({ "offset": offset? }),
                    Err(_) => {
                        return Ok((
                            axum::http::StatusCode::ACCEPTED,
                            Json(json!({"pending":true})),
                        )
                            .into_response());
                    }
                }
            } else {
                match tokio::time::timeout(Duration::from_secs(20), control.wait(turn)).await {
                    Ok(receipt) => serde_json::to_value(receipt?)?,
                    Err(_) => {
                        return Ok((
                            axum::http::StatusCode::ACCEPTED,
                            Json(json!({"pending":true})),
                        )
                            .into_response());
                    }
                }
            }
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
// Base cache cleanup follows pin recovery so link counts show only durable intents.
async fn recover_preparations(data: &Path, state: &Path) -> Result<()> {
    storage::environment::recover(state).await?;
    super::bases::recover_pins(data).await?;
    super::bases::recover_cache(state).await
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

fn interrupted_attempt(pending: &mut HashMap<String, String>, run: String, attempt: String) {
    // Published before execute writes any workspace files; it remains valid
    // after a crash before the first mount. An existing quarantine wins.
    pending.entry(run).or_insert(attempt);
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
    super::quota::require(&state)?;
    super::quota::migration::require_settled(&state).await?;
    disk::recover_exports(&state).await?;
    atomic_write(
        &state.join("native-accounting.json"),
        &serde_json::to_vec(&json!({ "data": data }))?,
    )
    .await?;
    super::spares::discard_unclaimed(&state, None).await?;
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
            interrupted.push((attempt, run, entry.path()));
        }
    }
    // Read-only attestation and private namespace preparation precede serving;
    // physical retirement is owned by per-run workers, never by startup.
    let cleanup = super::cleanup::Registry::default();
    let mut pending = super::cleanup::records(&state)?
        .into_iter()
        .map(|record| (record.run, record.attempt))
        .collect::<HashMap<_, _>>();
    for (attempt, run, _) in interrupted {
        interrupted_attempt(&mut pending, run, attempt);
    }
    for run in storage::ublk::stale_runs(&state)? {
        if let std::collections::hash_map::Entry::Vacant(entry) = pending.entry(run) {
            let root = workspace::directory(&state, entry.key())?;
            let attempt = tokio::fs::read_to_string(root.join("attempt")).await?;
            uuid(&attempt)?;
            entry.insert(attempt);
        }
    }
    disk::recover_preparations(&data, &state, &pending)?;
    for (run, attempt) in &pending {
        super::cleanup::quarantine(&state, run, attempt)?;
    }
    let recovery_state = state.clone();
    let recovery_cleanup = cleanup.clone();
    std::thread::Builder::new()
        .name("native-storage-recovery".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!(%error, "Native recovery executor could not start");
                    return;
                }
            };
            runtime.block_on(async move {
                for (run, attempt) in pending {
                    let progress = Arc::new(Mutex::new(cleanup::RecoveryProgress::default()));
                    let work = cleanup::RecoveryWork {
                        state: recovery_state.clone(),
                        run: run.clone(),
                        attempt: attempt.clone(),
                        unmounted: false,
                        retired: false,
                        owner: None,
                        disk: None,
                        fenced: None,
                        progress: progress.clone(),
                    };
                    match recovery_cleanup.start(&recovery_state, &run, &attempt, work) {
                        Ok(ticket) => {
                            if let Err(error) = cleanup::observe_recovery(recovery_state.clone(), attempt, progress, ticket) {
                                tracing::error!(%error, %run, "Native recovery observer could not start");
                            }
                        }
                        Err(error) => tracing::error!(%error, %run, "Native disk recovery remains quarantined"),
                    }
                }
            });
        })
        .map_err(Error::internal)?;
    // Ownership recovery only touches journal metadata, not native consumers.
    recover_preparations(&data, &state).await?;
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
        cleanup,
        environments: Arc::default(),
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
    let mut stopping = tokio::task::JoinSet::new();
    for id in active {
        let controller = controller.clone();
        stopping.spawn(async move { controller.stop_attempt(&id).await });
    }
    let drained = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(result) = stopping.join_next().await {
            if !matches!(result, Ok(Ok(()))) {
                tracing::error!("Native shutdown left a retained recovery marker");
            }
        }
    })
    .await;
    if drained.is_err() {
        stopping.abort_all();
    }
    exclusive_monitor.await.map_err(Error::internal)?;
    telemetry_monitor.abort();
    let _ = telemetry_monitor.await;
    controller.base_warmer.wait_idle().await;
    match result {
        Some(result) => result.map_err(Error::internal)??,
        None => {
            if tokio::time::timeout(Duration::from_secs(2), &mut server)
                .await
                .is_err()
            {
                server.abort();
            }
        }
    }
    tokio::fs::remove_file(socket).await?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod mcp_field_tests;
