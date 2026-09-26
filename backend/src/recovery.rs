use crate::{
    config::now,
    error::{Error, Result},
    execution::secret,
    rpc::Session,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
pub async fn process_identity(pid: u32) -> Result<Option<Value>> {
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
    Ok(Some(json!({
    "pid":pid,"start":start,"boot":boot.trim()}
    )))
}
async fn matches(identity: &Value) -> Result<bool> {
    Ok(
        process_identity(identity["pid"].as_u64().unwrap_or(0) as u32)
            .await?
            .is_some_and(|current| {
                current["start"] == identity["start"] && current["boot"] == identity["boot"]
            }),
    )
}
pub async fn fence_process(identity: &Value) -> Result<()> {
    let pid = identity["pid"]
        .as_u64()
        .filter(|pid| *pid > 1 && *pid <= i32::MAX as u64)
        .ok_or_else(|| Error::bad("Invalid saved process identity."))?;
    if !matches(identity).await? {
        return Ok(());
    }
    let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    let deadline = now() + 5000;
    while matches(identity).await? {
        if now() >= deadline {
            return Err(Error::new(
                503,
                "Waiting for the previous run process to stop.",
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}
pub async fn fence(s: &Service, run: &Value) -> Result<()> {
    let key = format!("run-checkpoint:{}", text(run, "id"));
    let checkpoint = s.store.kv(&key).await?;
    if let Some(checkpoint) = &checkpoint
        && checkpoint["process"].is_object()
    {
        fence_process(&checkpoint["process"]).await?;
    }
    if run["isolated"] == true
        || checkpoint
            .as_ref()
            .is_some_and(|c| c["prepared"]["isolated"] == true)
    {
        if crate::nodes::transport::url(s, text(run, "id"))
            .await?
            .is_empty()
        {
            return Err(Error::new(
                503,
                "Waiting for the isolated runner before recovering this run.",
            ));
        }
        let id = checkpoint
            .as_ref()
            .and_then(|c| c["runnerId"].as_str())
            .unwrap_or_else(|| text(run, "id"));
        let response = s
            .http
            .delete(format!(
                "{}/runs/{id}",
                crate::nodes::transport::url(s, text(run, "id")).await?
            ))
            .bearer_auth(secret(&s.config.data_dir, "runner-secret").await?)
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        let confirmed = response
            .is_ok_and(|response| response.status().is_success() || response.status() == 404);
        if !confirmed {
            let owned = s.store.get("node-attempts", id).await?.unwrap_or_default();
            let leased = owned["leaseRequired"] == true
                || owned["nodeId"]
                    .as_str()
                    .is_some_and(|node| node != crate::nodes::LOCAL_NODE_ID);
            let deadline = s
                .node_lease_deadlines
                .lock()
                .await
                .get(id)
                .copied()
                .unwrap_or(
                    s.started
                        + Duration::from_millis(
                            owned["leaseDurationMs"]
                                .as_u64()
                                .unwrap_or(60000)
                                .min(300000)
                                + 3000,
                        ),
                );
            if !leased || tokio::time::Instant::now() < deadline + Duration::from_secs(20) {
                return Err(Error::new(
                    503,
                    "Waiting for the previous VM execution lease to expire.",
                ));
            }
        }
        crate::nodes::placement::release(s, id).await?;
    }
    if let Some(mut checkpoint) = checkpoint {
        checkpoint.as_object_mut().unwrap().remove("process");
        s.store.set(&key, checkpoint, None).await?;
    }
    Ok(())
}
pub async fn session(s: &Service, run: &Value, home: &Path, cwd: &Path) -> Result<String> {
    if crate::provider::Provider::of_run(run) == crate::provider::Provider::Claude {
        return run["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::new(
                    409,
                    "The saved Claude session is unavailable. Working files were preserved.",
                )
            });
    }
    if run["isolated"] == true || !s.config.runner_url.is_empty() {
        // The authoritative session is on the retained guest disk. The guest's
        // thread/resume verifies it; asking a host Codex process cannot do so.
        if let Some(id) = run["sessionId"].as_str().filter(|id| !id.is_empty()) {
            return Ok(id.to_owned());
        }
    }
    let result = async {
        let mut rpc = Session::codex(&s.config, home, &[], None).await?;
        let result = async {
            if let Some(id) = run["sessionId"].as_str() {
                let result = rpc
                    .request(
                        "thread/read",
                        json!({
                        "threadId":id,"includeTurns":false}
                        ),
                    )
                    .await?;
                if result["thread"]["id"] == id {
                    return Ok(id.to_owned());
                }
                return Err(Error::bad("Session mismatch"));
            }
            let result = rpc
                .request(
                    "thread/list",
                    json!({
                    "limit":2,"cwd":cwd,"sourceKinds":[if run["chatExecution"].is_object(){
                    "appServer"}
                    else{
                    "exec"}
                    ],"archived":false}
                    ),
                )
                .await?;
            let matches = result["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| {
                    t["cwd"] == cwd.to_string_lossy().as_ref() && t["parentThreadId"].is_null()
                })
                .collect::<Vec<_>>();
            if matches.len() == 1 && !text(matches[0], "id").is_empty() {
                return Ok(text(matches[0], "id").to_owned());
            }
            Err(Error::bad("No unique session"))
        }
        .await;
        rpc.close().await;
        result
    }
    .await;
    result.map_err(|_| Error::new(409, "The saved Codex conversation is unavailable. Working files were preserved; review them before starting a new run."))
}
