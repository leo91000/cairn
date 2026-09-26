//! Restore grants expose only the blocks of one retained recovery point.
use crate::{
    error::{Error, Result},
    http::App,
    service::Service,
    validation::text,
};
use axum::{
    body::Body,
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{os::fd::AsRawFd, path::Path, time::Duration};
pub async fn start(s: &Service, run: &Value, node: &str, backup: &Value) -> Result<()> {
    let _operation = s.node_backup_operation.lock().await;
    let manifest = super::backups::manifest(s, backup).await?;
    let credential = crate::auth::token();
    let grant_key = format!("node-restore:{}", crate::auth::digest(&credential));
    s.store
        .set(
            &grant_key,
            json!({"runId":run["id"],"nodeId":node,"backupId":backup["id"]}),
            Some(crate::config::now() + 3600000),
        )
        .await?;
    let base = if node == super::LOCAL_NODE_ID {
        s.config.runner_url.clone()
    } else {
        format!("{}/internal/execution/{node}", s.config.public_url)
    };
    let result=async {
        let response=s.http.post(format!("{base}/disks/{}/restore",text(run,"id"))).bearer_auth(crate::execution::secret(&s.config.data_dir,"runner-secret").await?).json(&json!({"manifest":manifest,"master":s.config.public_url,"grant":credential,"backupId":backup["id"]})).timeout(Duration::from_secs(3600)).send().await.map_err(|_|Error::new(503,"VM restore connection interrupted."))?;
        if !response.status().is_success() {return Err(Error::new(503,"Destination could not restore this VM runtime and disk."));}
        Ok(())
    }.await;
    s.store.delete(&grant_key).await?;
    result
}
pub async fn handle(State(app): State<App>, request: Request) -> Result<Response> {
    let s = &app.service;
    if request.method() != "GET" {
        return Err(Error::new(405, "Method not allowed."));
    }
    let credential = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let grant = s
        .store
        .kv(&format!("node-restore:{}", crate::auth::digest(credential)))
        .await?
        .ok_or_else(|| Error::new(401, "Restore grant expired."))?;
    let backup = s.get("node-backups", text(&grant, "backupId")).await?;
    let run = s.store.run(text(&grant, "runId")).await?;
    let agent = s
        .get("agents", text(&run["snapshot"]["agent"], "id"))
        .await?;
    if s.get("nodes", text(&grant, "nodeId")).await?["revoked"] == true
        || !run["cancelRequestedAt"].is_null()
        || !crate::service::allowed(
            &crate::service::policy(&agent)["nodes"],
            text(&grant, "nodeId"),
        )
    {
        return Err(Error::new(403, "Restore no longer authorized."));
    }
    let hash = request
        .uri()
        .path()
        .trim_start_matches("/internal/node-restore/");
    let manifest = super::backups::manifest(s, &backup).await?;
    if !manifest["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["hash"] == hash)
    {
        return Err(Error::new(403, "Block outside restore scope."));
    }
    let bytes = super::backups::read_block(s, &backup, hash).await?;
    Ok((
        [("content-length", bytes.len().to_string())],
        Body::from(bytes),
    )
        .into_response())
}
pub async fn controller(state: &Path, run: &str, value: Value) -> Result<Value> {
    crate::validation::uuid(run)?;
    let manifest = &value["manifest"];
    super::snapshots::validate(manifest)?;
    let runtime = text(&manifest["runtime"], "runtimeId");
    if !super::valid_runtime(runtime) {
        return Err(Error::bad("Invalid runtime identity."));
    }
    let image = state.join("images").join(runtime);
    if !image.join("root.ext4").exists() || !image.join("vmlinux").exists() {
        return Err(Error::new(
            409,
            "Required VM runtime is not installed on this node.",
        ));
    }
    let directory = state.join("disks").join(run);
    crate::skills::private_dir(&directory).await?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::new(409, "VM disk is active."));
    }
    let recovery = directory.join("recovery.json");
    if recovery.exists()
        && serde_json::from_slice::<Value>(&tokio::fs::read(&recovery).await?)?["backupId"]
            == value["backupId"]
        && directory.join("data.ext4").exists()
        && !directory.join("restore.pending").exists()
    {
        return Ok(json!({"ready":true}));
    }
    let origin = super::connector::master(text(&value, "master"))?;
    crate::skills::atomic_write(&directory.join("restore.pending"), b"restoring").await?;
    if directory.join("data.ext4").exists() {
        let stale = crate::config::id();
        for name in ["runtime.json", "recovery.json"] {
            if directory.join(name).exists() {
                tokio::fs::copy(
                    directory.join(name),
                    directory.join(format!("stale-{stale}-{name}")),
                )
                .await?;
            }
        }
        tokio::fs::rename(
            directory.join("data.ext4"),
            directory.join(format!("stale-{stale}.ext4")),
        )
        .await?;
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(Error::internal)?;
    let grant = text(&value, "grant").to_owned();
    super::snapshots::restore(&directory.join("data.ext4"), manifest, |hash| {
        let (client, origin, grant) = (client.clone(), origin.clone(), grant.clone());
        async move {
            let response = client
                .get(
                    origin
                        .join(&format!("internal/node-restore/{hash}"))
                        .map_err(Error::internal)?,
                )
                .bearer_auth(grant)
                .send()
                .await
                .map_err(|_| Error::new(503, "Backup restore interrupted."))?;
            if !response.status().is_success() {
                return Err(Error::new(503, "Backup block unavailable."));
            }
            let bytes = super::snapshots::response_block(response).await?;
            Ok(bytes.to_vec())
        }
    })
    .await?;
    crate::skills::atomic_write(
        &directory.join("runtime.json"),
        &serde_json::to_vec(&manifest["runtime"])?,
    )
    .await?;
    crate::skills::atomic_write(
        &recovery,
        &serde_json::to_vec(&json!({"backupId":value["backupId"]}))?,
    )
    .await?;
    tokio::fs::remove_file(directory.join("restore.pending")).await?;
    std::fs::File::open(&directory)?.sync_all()?;
    Ok(json!({"ready":true}))
}
