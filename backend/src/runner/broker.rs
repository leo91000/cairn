//! Attempt lifecycle on the VM controller.
use super::CONTROLLER_INTERRUPTED;
use crate::{
    config::now,
    error::{Error, Result},
    execution::Sandbox,
    microvm::{
        host,
        plan::{CHAT_INBOX, HOME, Plan},
        pool::{Pool, Reservation},
        protocol::Event,
        vm::AttemptRecord,
        wire,
    },
    skills::atomic_write,
    validation::{text, uuid},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, OnceCell, watch},
};
use tokio_util::sync::CancellationToken;

const MAX_PLAN_BYTES: usize = 8_000_000;
/// Longest attempt deadline a plan may request.
const MAX_ATTEMPT_MS: i64 = 13 * 3_600_000;

pub(super) struct Attempt {
    pub stop: CancellationToken,
    pub done: watch::Receiver<bool>,
    pub socket: Arc<OnceCell<PathBuf>>,
    pub plan: Plan,
    pub imports: Arc<Mutex<()>>,
    pub control: Arc<Mutex<()>>,
}

#[derive(Clone)]
pub(super) struct Broker {
    pub data: PathBuf,
    pub state: PathBuf,
    pub pool: Arc<Pool>,
    pub active: Arc<Mutex<HashMap<String, Attempt>>>,
    pub stop: CancellationToken,
    /// Node lease deadline of each attempt, in controller boot milliseconds.
    pub leases: Arc<Mutex<HashMap<String, u64>>>,
}

/// Body of `POST /runs/{id}/projects/{project}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProjectImport {
    #[serde(default)]
    pub run_id: Value,
    #[serde(default)]
    pub source: PathBuf,
    #[serde(default)]
    pub target: String,
}

fn normalized(path: &Path) -> bool {
    let plain = path.components().all(|part| {
        !matches!(
            part,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    });
    path.is_absolute() && plain && !path.to_string_lossy().contains("//")
}

/// `null` means unlimited; otherwise a deadline in the next 13 hours.
fn valid_expiry(expires: Option<&Value>) -> bool {
    match expires {
        Some(Value::Null) => true,
        Some(value) => value
            .as_i64()
            .is_some_and(|n| n > now() && n <= now() + MAX_ATTEMPT_MS),
        None => false,
    }
}

fn import_allowed(source: &Path, target: &Path, run_root: &Path) -> bool {
    let target_allowed = target.starts_with(run_root)
        || target == Path::new(HOME)
        || target == Path::new(CHAT_INBOX);
    normalized(source) && source.starts_with(run_root) && normalized(target) && target_allowed
}

pub(super) fn validate(plan: &Plan, id: &str, data: &Path) -> Result<()> {
    uuid(plan.run_id())?;
    if plan.id() != id || !valid_expiry(plan.expires()) {
        return Err(Error::bad("Invalid or expired execution plan."));
    }
    let run_root = data.join("runs").join(plan.run_id());
    if !plan.has_imports() {
        return Err(Error::bad("Missing execution imports."));
    }
    for import in plan.imports() {
        if !import_allowed(import.source, Path::new(import.target), &run_root) {
            return Err(Error::bad(
                "Execution import is outside its private workspace.",
            ));
        }
    }
    if !plan.cwd().starts_with(&run_root) {
        return Err(Error::bad("Invalid guest workspace."));
    }
    if let Some(output) = plan.chat_output()
        && Path::new(output) != run_root.join("output/result.md")
    {
        return Err(Error::bad("Invalid result destination."));
    }
    Ok(())
}

async fn remove_if_present(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

impl Broker {
    pub fn new(data: PathBuf, state: PathBuf, pool: Arc<Pool>, stop: CancellationToken) -> Self {
        Self {
            data,
            state,
            pool,
            active: Arc::default(),
            stop,
            leases: Arc::default(),
        }
    }

    pub fn state_file(&self, attempt: &str, extension: &str) -> PathBuf {
        self.state.join(format!("{attempt}.{extension}"))
    }

    pub fn has_stopped(&self, attempt: &str) -> bool {
        self.state_file(attempt, "stopped").exists() || self.state_file(attempt, "exit").exists()
    }

    pub async fn run_is_active(&self, run: &str) -> bool {
        self.active
            .lock()
            .await
            .values()
            .any(|a| a.plan.run_id() == run)
    }

    async fn lease_valid(&self, attempt: &str) -> bool {
        self.leases
            .lock()
            .await
            .get(attempt)
            .is_some_and(|until| *until > crate::nodes::boot_ms())
    }

    async fn load_plan(&self, id: &str) -> Result<Plan> {
        let path = self.data.join("runner-plans").join(format!("{id}.json"));
        let file = tokio::fs::File::open(path).await?;
        let mut bytes = Vec::new();
        file.take(MAX_PLAN_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > MAX_PLAN_BYTES {
            return Err(Error::bad("Execution plan exceeds limit."));
        }
        let plan = Plan::new(serde_json::from_slice(&bytes)?);
        validate(&plan, id, &self.data)?;
        Ok(plan)
    }

    pub async fn start(&self, id: &str) -> Result<()> {
        if self.active.lock().await.contains_key(id) {
            return Ok(());
        }
        if self.has_stopped(id) {
            return Err(Error::conflict(
                "This execution attempt has already stopped.",
            ));
        }
        let plan = self.load_plan(id).await?;
        if plan.node_lease_required() && !self.lease_valid(id).await {
            return Err(Error::conflict(
                "Node execution lease is missing or expired.",
            ));
        }
        // Imports must resolve to regular directories in this run; a symlink is not a scope grant.
        for import in plan.imports() {
            if tokio::fs::canonicalize(import.source).await? != import.source {
                return Err(Error::bad("Execution import changed location."));
            }
        }
        if self.run_is_active(plan.run_id()).await {
            return Err(Error::conflict(
                "This workspace already has an active attempt.",
            ));
        }
        let reservation = self.pool.reserve(&plan).await?;
        // Reservation can wait for preparation teardown. Keep health, stop and imports responsive.
        let mut active = self.active.lock().await;
        if active.contains_key(id) {
            return Ok(());
        }
        if self.stop.is_cancelled() || self.has_stopped(id) {
            return Err(Error::conflict("This execution attempt has stopped."));
        }
        validate(&plan, id, &self.data)?;
        if active.values().any(|a| a.plan.run_id() == plan.run_id()) {
            return Err(Error::conflict(
                "This workspace already has an active attempt.",
            ));
        }
        // Keep the active attempt locked until this run is registered.
        let socket = Arc::new(OnceCell::new());
        let control = Arc::new(Mutex::new(()));
        let stop = self.stop.child_token();
        let (done, receiver) = watch::channel(false);
        atomic_write(&self.state_file(id, "run"), plan.run_id().as_bytes()).await?;
        atomic_write(&self.state_file(id, "active"), b"1").await?;
        active.insert(
            id.into(),
            Attempt {
                stop: stop.clone(),
                done: receiver,
                socket: socket.clone(),
                plan: plan.clone(),
                imports: Arc::default(),
                control: control.clone(),
            },
        );
        let execution = Execution {
            broker: self.clone(),
            id: id.to_owned(),
            plan,
            socket,
            control,
            stop,
            done,
        };
        tokio::spawn(execution.run(reservation));
        Ok(())
    }

    pub async fn open_project(
        &self,
        id: &str,
        project_id: &str,
        request: ProjectImport,
    ) -> Result<Value> {
        uuid(project_id)?;
        let (plan, stop, lock, socket) = {
            let active = self.active.lock().await;
            let attempt = active
                .get(id)
                .ok_or_else(|| Error::conflict("VM is not active."))?;
            (
                attempt.plan.clone(),
                attempt.stop.clone(),
                attempt.imports.clone(),
                attempt.socket.clone(),
            )
        };
        let _guard = lock.lock().await;
        if stop.is_cancelled() || request.run_id != plan.run_id() {
            return Err(Error::conflict("VM attempt changed."));
        }
        let root = self.data.join("runs").join(plan.run_id());
        let source = request.source.as_path();
        let outside = !normalized(source)
            || !source.starts_with(&root)
            || source != Path::new(&request.target)
            || source.file_name().and_then(|v| v.to_str()) != Some(project_id);
        if outside || tokio::fs::canonicalize(source).await? != source {
            return Err(Error::bad(
                "Project import is outside its private workspace.",
            ));
        }
        let socket = socket
            .get()
            .ok_or_else(|| Error::conflict("VM is still starting."))?;
        let read_only = plan.sandbox() == Some(Sandbox::ReadOnly);
        let import = host::import_project(socket, source, &request.target, read_only);
        tokio::select! {
            () = stop.cancelled() => Err(Error::conflict("VM stopped during project import.")),
            result = tokio::time::timeout(Duration::from_secs(290), import) => {
                result.map_err(|_| Error::unavailable("Project import timed out."))?
            }
        }
    }

    pub async fn stop(&self, id: &str) -> Result<()> {
        atomic_write(&self.state_file(id, "stopped"), b"").await?;
        let attempt = {
            let active = self.active.lock().await;
            active.get(id).map(|a| {
                a.stop.cancel();
                a.done.clone()
            })
        };
        if let Some(mut done) = attempt {
            let _ = tokio::time::timeout(Duration::from_secs(15), done.wait_for(|done| *done))
                .await
                .map_err(|_| Error::unavailable("Waiting for the previous VM to stop."))?;
        }
        Ok(())
    }
}

/// A registered attempt running in the background until its exit is recorded.
struct Execution {
    broker: Broker,
    id: String,
    plan: Plan,
    socket: Arc<OnceCell<PathBuf>>,
    control: Arc<Mutex<()>>,
    stop: CancellationToken,
    done: watch::Sender<bool>,
}

impl Execution {
    async fn run(self, reservation: Reservation) {
        let lease_expired = Arc::new(AtomicBool::new(false));
        let timer = tokio::spawn(enforce_limits(
            self.broker.clone(),
            self.id.clone(),
            self.plan.clone(),
            self.socket.clone(),
            self.control.clone(),
            self.stop.clone(),
            lease_expired.clone(),
        ));
        let result = reservation
            .execute(self.plan, self.socket, self.control, self.stop)
            .await;
        timer.abort();
        let broker = &self.broker;
        let id = &self.id;
        if let Err(error) = &result {
            log_failure(broker, id, &error.message).await;
        }
        let code = exit_code(result, lease_expired.load(Ordering::SeqCst));
        let exit = atomic_write(&broker.state_file(id, "exit"), code.to_string().as_bytes()).await;
        if let Err(error) = exit {
            tracing::warn!(attempt = %id, message = %error.message, "Could not record VM exit");
        }
        let _ = tokio::fs::remove_file(broker.state_file(id, "active")).await;
        broker.active.lock().await.remove(id);
        let _ = self.done.send(true);
    }
}

/// A lost lease or an unavailable VM (for example a guest killed by the
/// shared memory limit) keeps its disk: the manager resumes the attempt
/// instead of failing the run.
fn exit_code(result: Result<i32>, lease_expired: bool) -> i32 {
    match result {
        _ if lease_expired => CONTROLLER_INTERRUPTED,
        Err(error) if error.is_unavailable() => CONTROLLER_INTERRUPTED,
        Err(_) => 1,
        Ok(code) => code,
    }
}

/// Appends a controller failure to the attempt output so the manager can show it.
async fn log_failure(broker: &Broker, id: &str, message: &str) {
    let event = Event::Output {
        stderr: true,
        data: STANDARD.encode(format!("{message}\n")),
    };
    let log = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(broker.state_file(id, "log"))
        .await;
    if let Ok(mut log) = log {
        let _ = wire::write(&mut log, &event).await;
    }
}

/// Stops the attempt at its deadline or when its node lease lapses, and applies
/// disk limits while it runs.
async fn enforce_limits(
    broker: Broker,
    attempt: String,
    plan: Plan,
    socket: Arc<OnceCell<PathBuf>>,
    control: Arc<Mutex<()>>,
    expiry: CancellationToken,
    lease_expired: Arc<AtomicBool>,
) {
    let deadline = plan.deadline();
    let leased = plan.node_lease_required();
    loop {
        if expiry.is_cancelled() {
            return;
        }
        let disk_directory = crate::storage::environment::directory(&broker.state, plan.run_id());
        let lost_lease = leased && !broker.lease_valid(&attempt).await;
        if deadline.is_some_and(|d| now() >= d) || lost_lease {
            if expiry.is_cancelled() {
                return;
            }
            lease_expired.store(lost_lease, Ordering::SeqCst);
            // Cancellation releases an in-flight guest thaw before waiting
            // for its control lock. The timer never waits unboundedly.
            expiry.cancel();
            if let Ok(directory) = &disk_directory
                && let Some(volume) = crate::storage::runtime::live(directory)
            {
                volume.stop.cancel();
            }
            if leased {
                let _guard = tokio::time::timeout(Duration::from_secs(2), control.lock()).await;
                if let Err(error) = host::pause_attempt(&broker.state, &attempt).await {
                    tracing::warn!(%attempt, message = %error.message, "Could not pause VM after lease loss");
                }
            }
            return;
        }
        let directory = match disk_directory {
            Ok(directory) => Some(directory),
            // Attribution publishes two durable records before exposing the
            // guest socket. Its temporary incomplete state is not a VM fault.
            Err(_) if socket.get().is_none() => None,
            Err(error) => {
                tracing::error!(%attempt, message = %error.message, "Could not resolve conversation disk ownership");
                expiry.cancel();
                return;
            }
        };
        if let Some(volume) = directory.as_deref().and_then(crate::storage::runtime::live)
            && let Ok(_guard) = control.try_lock()
        {
            if expiry.is_cancelled() {
                return;
            }
            if volume
                .enforce_limits(&broker.state, &attempt, &expiry)
                .await
                .is_err()
            {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn plan_run_id(path: &Path) -> Result<String> {
    let plan: Value = serde_json::from_slice(&tokio::fs::read(path).await?)?;
    Ok(text(&plan, "runId").to_owned())
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use serde_json::json;

    async fn pending_claim(root: &Path) -> (Broker, String) {
        let state = root.join("state");
        let data = root.join("data");
        crate::skills::private_dir(&state).await.unwrap();
        let pool = Pool::new(state.clone(), root.into(), CancellationToken::new(), 1)
            .await
            .unwrap();
        let run = crate::config::id();
        let environment = crate::config::id();
        let logical = state.join("disks").join(&run);
        crate::skills::private_dir(&logical).await.unwrap();
        crate::skills::private_dir(&state.join("environments").join(&environment))
            .await
            .unwrap();
        std::fs::write(logical.join("environment"), environment).unwrap();
        (
            Broker::new(data, state, pool, CancellationToken::new()),
            run,
        )
    }

    #[tokio::test]
    async fn incomplete_attribution_is_tolerated_only_before_exposing_guest_socket() {
        let root = tempfile::tempdir().unwrap();
        let (broker, run) = pending_claim(root.path()).await;
        let socket = Arc::new(OnceCell::new());
        let stop = CancellationToken::new();
        let monitor = tokio::spawn(enforce_limits(
            broker,
            crate::config::id(),
            Plan::new(json!({ "runId": run })),
            socket.clone(),
            Arc::default(),
            stop.clone(),
            Arc::default(),
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !stop.is_cancelled(),
            "an in-progress claim is not a VM fault"
        );
        socket.set(root.path().join("guest.sock")).unwrap();
        tokio::time::timeout(Duration::from_secs(2), stop.cancelled())
            .await
            .unwrap();
        monitor.await.unwrap();
    }

    #[tokio::test]
    async fn incomplete_attribution_cannot_delay_the_attempt_deadline() {
        let root = tempfile::tempdir().unwrap();
        let (broker, run) = pending_claim(root.path()).await;
        let stop = CancellationToken::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            enforce_limits(
                broker,
                crate::config::id(),
                Plan::new(json!({ "runId": run, "expires": now() })),
                Arc::default(),
                Arc::default(),
                stop.clone(),
                Arc::default(),
            ),
        )
        .await
        .unwrap();
        assert!(stop.is_cancelled());
    }
}

/// Fences and deletes every attempt artifact of `run`: plans, logs and VM records.
pub(super) async fn erase_attempt_content(broker: &Broker, run: &str) -> Result<()> {
    let mut attempts = HashSet::new();
    let plans = broker.data.join("runner-plans");
    if plans.is_dir() {
        let mut entries = tokio::fs::read_dir(&plans).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(attempt) = name.strip_suffix(".json").filter(|v| uuid(v).is_ok()) else {
                continue;
            };
            if plan_run_id(&entry.path()).await? != run {
                continue;
            }
            // Fence a delayed start before removing its short-lived credentials.
            atomic_write(&broker.state_file(attempt, "stopped"), b"").await?;
            tokio::fs::remove_file(entry.path()).await?;
            attempts.insert(attempt.to_owned());
        }
    }
    let mut entries = tokio::fs::read_dir(&broker.state).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(attempt) = name.strip_suffix(".run").filter(|v| uuid(v).is_ok()) {
            if tokio::fs::read_to_string(entry.path()).await? == run {
                atomic_write(&broker.state_file(attempt, "stopped"), b"").await?;
                attempts.insert(attempt.to_owned());
                tokio::fs::remove_file(entry.path()).await?;
            }
            continue;
        }
        let Some(attempt) = name.strip_suffix(".vm.json").filter(|v| uuid(v).is_ok()) else {
            continue;
        };
        let record: AttemptRecord = serde_json::from_slice(&tokio::fs::read(entry.path()).await?)?;
        if record.run_id != run {
            continue;
        }
        atomic_write(&broker.state_file(attempt, "stopped"), b"").await?;
        attempts.insert(attempt.to_owned());
        if uuid(&record.vm_id).is_ok() {
            for suffix in ["boot.log", "vmm.log"] {
                remove_if_present(&broker.state_file(&record.vm_id, suffix)).await?;
            }
        }
        tokio::fs::remove_file(entry.path()).await?;
    }
    for attempt in attempts {
        remove_if_present(&broker.state_file(&attempt, "log")).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn plan(expires: &Value) -> Value {
        json!({
            "id": ID,
            "runId": ID,
            "expires": expires,
            "cwd": format!("/data/runs/{ID}/workspace"),
            "imports": []
        })
    }

    fn valid(plan: &Value) -> bool {
        validate(&Plan::new(plan.clone()), ID, Path::new("/data")).is_ok()
    }

    #[test]
    fn execution_plans_accept_unlimited_but_reject_invalid_or_expired_deadlines() {
        let mut plan = plan(&Value::Null);
        assert!(valid(&plan));
        for expiry in [
            json!(0),
            json!(now() - 1),
            json!(now() + 14 * 3_600_000),
            json!("unlimited"),
        ] {
            plan["expires"] = expiry;
            assert!(!valid(&plan));
        }
        plan.as_object_mut().unwrap().remove("expires");
        assert!(!valid(&plan));
    }

    #[test]
    fn an_unavailable_guest_is_reported_as_a_recoverable_interruption() {
        let disconnected =
            Error::unavailable("Guest disconnected. Its workspace disk has been preserved.");
        assert_eq!(exit_code(Err(disconnected), false), CONTROLLER_INTERRUPTED);
        assert_eq!(exit_code(Err(Error::bad("Invalid plan.")), false), 1);
        assert_eq!(exit_code(Ok(3), false), 3);
        assert_eq!(exit_code(Ok(0), true), CONTROLLER_INTERRUPTED);
        assert_eq!(
            exit_code(Err(Error::bad("Invalid plan.")), true),
            CONTROLLER_INTERRUPTED
        );
        let incompatible = Error::bad_gateway("Unsupported guest protocol.");
        assert_eq!(exit_code(Err(incompatible), false), 1);
    }

    #[test]
    fn imports_cannot_grant_host_or_other_run_access() {
        let mut plan = plan(&json!(now() + 60000));
        assert!(valid(&plan));
        for source in [
            "/data/private",
            "/home/node",
            "/data/runs/another/workspace",
            "/data/runs/../private",
        ] {
            plan["imports"] = json!([{ "source": source, "target": "/home/node" }]);
            assert!(!valid(&plan));
        }
    }
}
