use crate::{
    config::now,
    error::{Error, Result},
    execution::secret,
    provider::Provider,
    service::Service,
    validation::text,
    worker::checkpoint::RunCheckpoint,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, time::Duration};

/// Identifies one process instance across PID reuse and host reboots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u64,
    pub start: String,
    pub boot: String,
}

pub async fn process_identity(pid: u32) -> Result<Option<ProcessIdentity>> {
    let stat = match tokio::fs::read_to_string(format!("/proc/{pid}/stat")).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let fields = stat
        .rsplit_once(") ")
        .map(|(_, s)| s.split(' ').collect::<Vec<_>>())
        .ok_or_else(|| Error::internal("Invalid process status"))?;
    if fields.first() == Some(&"Z") {
        return Ok(None);
    }
    let start = fields
        .get(19)
        .ok_or_else(|| Error::internal("Invalid process status"))?;
    let boot = tokio::fs::read_to_string("/proc/sys/kernel/random/boot_id").await?;
    Ok(Some(ProcessIdentity {
        pid: pid.into(),
        start: (*start).to_owned(),
        boot: boot.trim().to_owned(),
    }))
}

async fn is_running(pid: u32, identity: &ProcessIdentity) -> Result<bool> {
    Ok(process_identity(pid)
        .await?
        .is_some_and(|current| current.start == identity.start && current.boot == identity.boot))
}

pub async fn fence_process(identity: &ProcessIdentity) -> Result<()> {
    let pid = i32::try_from(identity.pid)
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| Error::bad("Invalid saved process identity."))?;
    let unsigned_pid = pid.unsigned_abs();
    if !is_running(unsigned_pid, identity).await? {
        return Ok(());
    }
    let result = unsafe { libc::kill(pid, libc::SIGTERM) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    let deadline = now() + 5000;
    while is_running(unsigned_pid, identity).await? {
        if now() >= deadline {
            return Err(Error::unavailable(
                "Waiting for the previous run process to stop.",
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

/// Observe the existing attempt before stopping it or deciding whether to replay.
/// This never acquires a workspace, starts an agent, or replays retained output.
#[derive(Clone, Copy)]
pub(crate) enum ObservationAction {
    Reserve,
    Release,
    Exhausted,
    Record(Option<i32>),
}

pub(crate) async fn terminal_code<F, Fut>(
    s: &Service,
    attempt: &str,
    node: Option<&str>,
    cancel: &tokio_util::sync::CancellationToken,
    limit: Duration,
    mut persist: F,
) -> Result<()>
where
    F: FnMut(ObservationAction) -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let runner_url = crate::nodes::transport::url_for_node(s, node);
    if runner_url.is_empty() {
        persist(ObservationAction::Exhausted).await?;
        return Ok(());
    }
    let token = secret(&s.config.data_dir, "runner-secret").await?;
    let url = format!("{runner_url}/runs/{attempt}");
    let deadline = tokio::time::Instant::now() + limit.min(Duration::from_secs(5));
    loop {
        if s.shutdown.is_cancelled() || cancel.is_cancelled() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            persist(ObservationAction::Exhausted).await?;
            return Ok(());
        }
        // Do not drop an in-flight transaction: it may still commit after its
        // caller is cancelled, leaving an active checkpoint's memory stale.
        if !persist(ObservationAction::Reserve).await? {
            persist(ObservationAction::Exhausted).await?;
            return Ok(());
        }
        if s.shutdown.is_cancelled()
            || cancel.is_cancelled()
            || tokio::time::Instant::now() >= deadline
        {
            persist(ObservationAction::Release).await?;
            if !s.shutdown.is_cancelled() && !cancel.is_cancelled() {
                persist(ObservationAction::Record(None)).await?;
            }
            return Ok(());
        }
        let wait = crate::runner::wait_for_exit(&s.http, &url, &token);
        let result = tokio::select! {
            () = s.shutdown.cancelled() => None,
            () = cancel.cancelled() => None,
            result = tokio::time::timeout_at(deadline, wait) => Some(result),
        };
        match result {
            None => {
                persist(ObservationAction::Release).await?;
                return Ok(());
            }
            Some(Ok(Ok(code))) => {
                persist(ObservationAction::Record(Some(code))).await?;
                return Ok(());
            }
            Some(Err(_)) => {
                persist(ObservationAction::Record(None)).await?;
                return Ok(());
            }
            Some(Ok(Err(_))) => {
                persist(ObservationAction::Release).await?;
            }
        }
        let next_read = (tokio::time::Instant::now() + Duration::from_millis(200)).min(deadline);
        tokio::select! {
            () = s.shutdown.cancelled() => return Ok(()),
            () = cancel.cancelled() => return Ok(()),
            () = tokio::time::sleep_until(next_read) => {}
        }
    }
}

/// Stops every trace of the run's previous execution before it may be relaunched.
pub async fn fence(s: &Service, run: &Value) -> Result<()> {
    let run_id = text(run, "id");
    let checkpoint = RunCheckpoint::load(&s.store, run_id).await?;
    fence_attempt(s, run, checkpoint.as_ref()).await
}

/// Compare the physical owner, not the checkpoint's mutable evidence.
pub(crate) fn same_execution(left: Option<&RunCheckpoint>, right: Option<&RunCheckpoint>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.runner_id() == right.runner_id()
                && left.node_id == right.node_id
                && left.process() == right.process()
        }
        _ => false,
    }
}

/// Fence only the attempt selected by the caller, including its process and node.
pub(crate) async fn fence_attempt(
    s: &Service,
    run: &Value,
    expected: Option<&RunCheckpoint>,
) -> Result<()> {
    let run_id = text(run, "id");
    let checkpoint = RunCheckpoint::load(&s.store, run_id).await?;
    if !same_execution(checkpoint.as_ref(), expected) {
        return Err(Error::conflict(
            "Execution attempt changed during recovery.",
        ));
    }
    let process = checkpoint
        .as_ref()
        .and_then(RunCheckpoint::process)
        .cloned();
    let node = checkpoint
        .as_ref()
        .and_then(|saved| saved.node_id.as_ref())
        .and_then(|node| node.as_deref())
        .map(str::to_owned);
    if let Some(process) = &process {
        fence_process(process).await?;
    }
    let isolated =
        run["isolated"] == true || checkpoint.as_ref().is_some_and(RunCheckpoint::isolated);
    if isolated {
        let attempt = expected
            .and_then(RunCheckpoint::runner_id)
            .unwrap_or(run_id);
        let runner_url = crate::nodes::transport::url_for_node(s, node.as_deref());
        fence_runner(s, &runner_url, attempt).await?;
    }
    let id = run_id.to_owned();
    let expected = expected
        .and_then(RunCheckpoint::runner_id)
        .map(str::to_owned);
    let release = isolated.then(|| expected.clone().unwrap_or_else(|| id.clone()));
    s.store
        .transaction(move |db| {
            let mut current = RunCheckpoint::load_in(db, &id)?;
            let current_attempt = current.as_ref().and_then(RunCheckpoint::runner_id);
            let current_node = current
                .as_ref()
                .and_then(|saved| saved.node_id.as_ref())
                .and_then(|node| node.as_deref());
            let current_process = current.as_ref().and_then(RunCheckpoint::process);
            if current_attempt != expected.as_deref()
                || current_node != node.as_deref()
                || current_process != process.as_ref()
            {
                return Err(Error::conflict(
                    "Execution attempt changed during recovery.",
                ));
            }
            if let Some(saved) = &mut current {
                saved.process = None;
                saved.store_in(db, &id)?;
            }
            if let Some(attempt) = release {
                crate::nodes::placement::release_in(db, &attempt)?;
            }
            Ok(())
        })
        .await
}

async fn fence_runner(s: &Service, runner_url: &str, attempt: &str) -> Result<()> {
    if runner_url.is_empty() {
        return Err(Error::unavailable(
            "Waiting for the isolated runner before recovering this run.",
        ));
    }
    let response = s
        .http
        .delete(format!("{runner_url}/runs/{attempt}"))
        .bearer_auth(secret(&s.config.data_dir, "runner-secret").await?)
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    let confirmed =
        response.is_ok_and(|response| response.status().is_success() || response.status() == 404);
    if !confirmed && !node_lease_expired(s, attempt).await? {
        return Err(Error::unavailable(
            "Waiting for the previous VM execution lease to expire.",
        ));
    }
    Ok(())
}

/// An unreachable runner is only presumed stopped once its node lease has lapsed.
async fn node_lease_expired(s: &Service, attempt: &str) -> Result<bool> {
    let owned = s
        .store
        .get("node-attempts", attempt)
        .await?
        .unwrap_or_default();
    let leased = owned["leaseRequired"] == true
        || owned["nodeId"]
            .as_str()
            .is_some_and(|node| node != crate::nodes::LOCAL_NODE_ID);
    if !leased {
        return Ok(false);
    }
    let lease_ms = owned["leaseDurationMs"]
        .as_u64()
        .unwrap_or(60000)
        .min(300000);
    let deadline = s
        .node_lease_deadlines
        .lock()
        .await
        .get(attempt)
        .copied()
        .unwrap_or_else(|| s.started + Duration::from_millis(lease_ms + 3000));
    Ok(tokio::time::Instant::now() >= deadline + Duration::from_secs(20))
}

pub async fn session(s: &Service, run: &Value, home: &Path, cwd: &Path) -> Result<String> {
    Provider::of_run(run)?
        .adapter()
        .resume_session(s, run, home, cwd)
        .await
}
