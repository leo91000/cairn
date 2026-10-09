use crate::{
    config::now,
    error::{Error, Result},
    execution::secret,
    provider::Provider,
    rpc::Session,
    service::Service,
    validation::text,
    worker::checkpoint::RunCheckpoint,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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

/// Stops every trace of the run's previous execution before it may be relaunched.
pub async fn fence(s: &Service, run: &Value) -> Result<()> {
    let run_id = text(run, "id");
    let checkpoint = RunCheckpoint::load(&s.store, run_id).await?;
    if let Some(process) = checkpoint.as_ref().and_then(RunCheckpoint::process) {
        fence_process(process).await?;
    }
    let isolated =
        run["isolated"] == true || checkpoint.as_ref().is_some_and(RunCheckpoint::isolated);
    if isolated {
        let attempt = checkpoint
            .as_ref()
            .and_then(RunCheckpoint::runner_id)
            .unwrap_or(run_id);
        fence_runner(s, run_id, attempt).await?;
    }
    if let Some(mut checkpoint) = checkpoint {
        checkpoint.process = None;
        checkpoint.store(&s.store, run_id).await?;
    }
    Ok(())
}

async fn fence_runner(s: &Service, run_id: &str, attempt: &str) -> Result<()> {
    let runner_url = crate::nodes::transport::url(s, run_id).await?;
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
    crate::nodes::placement::release(s, attempt).await
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
    let saved = run["sessionId"].as_str().filter(|id| !id.is_empty());
    if Provider::of_run(run) == Provider::Claude {
        return saved.map(str::to_owned).ok_or_else(|| {
            Error::conflict(
                "The saved Claude session is unavailable. Working files were preserved.",
            )
        });
    }
    // The authoritative session is on the retained guest disk. The guest's
    // thread/resume verifies it; asking a host Codex process cannot do so.
    if (run["isolated"] == true || !s.config.runner_url.is_empty())
        && let Some(id) = saved
    {
        return Ok(id.to_owned());
    }
    codex_session(s, run, home, cwd).await.map_err(|_| {
        Error::conflict(
            "The saved Codex conversation is unavailable. Working files were preserved; review them before starting a new run.",
        )
    })
}

async fn codex_session(s: &Service, run: &Value, home: &Path, cwd: &Path) -> Result<String> {
    let mut rpc = Session::codex(&s.config, home, &[], None).await?;
    let result = find_codex_session(&mut rpc, run, cwd).await;
    rpc.close().await;
    result
}

async fn find_codex_session(rpc: &mut Session, run: &Value, cwd: &Path) -> Result<String> {
    if let Some(id) = run["sessionId"].as_str() {
        let params = json!({ "threadId": id, "includeTurns": false });
        let result = rpc.request("thread/read", params).await?;
        if result["thread"]["id"] == id {
            return Ok(id.to_owned());
        }
        return Err(Error::bad("Session mismatch"));
    }
    let source_kind = if run["chatExecution"].is_object() {
        "appServer"
    } else {
        "exec"
    };
    let params = json!({
        "limit": 2,
        "cwd": cwd,
        "sourceKinds": [source_kind],
        "archived": false,
    });
    let result = rpc.request("thread/list", params).await?;
    let cwd = cwd.to_string_lossy();
    let matches = result["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["cwd"] == cwd.as_ref() && t["parentThreadId"].is_null())
        .collect::<Vec<_>>();
    if let [thread] = matches.as_slice()
        && !text(thread, "id").is_empty()
    {
        return Ok(text(thread, "id").to_owned());
    }
    Err(Error::bad("No unique session"))
}
