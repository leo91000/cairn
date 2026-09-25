//! A destination mounts the current published disk through an authorized grant.
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
use std::{path::Path, time::Duration};

pub async fn start(s: &Service, run: &Value, node: &str, backup: &Value) -> Result<()> {
    let _operation = s.node_backup_operation.lock().await;
    let manifest = super::publication::manifest(s, backup).await?;
    let record = s.get("nodes", node).await?;
    if record["capabilities"]["fuse"] != true {
        return Err(Error::new(409, "Destination has no FUSE device."));
    }
    let policy = crate::storage::policy::Policy::for_node(&record["storage"])?;
    let credential = super::disk_grants::issue(s, run, node, backup).await?;
    let base = if node == super::LOCAL_NODE_ID {
        s.config.runner_url.clone()
    } else {
        format!("{}/internal/execution/{node}", s.config.public_url)
    };
    let response = s
        .http
        .post(format!("{base}/disks/{}/restore", text(run, "id")))
        .bearer_auth(crate::execution::secret(&s.config.data_dir, "runner-secret").await?)
        .json(&json!({
            "manifest": manifest,
            "master": s.config.public_url,
            "grant": credential,
            "backupId": backup["id"],
            "policy": policy
        }))
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|_| Error::new(503, "VM restore connection interrupted."))?;
    if !response.status().is_success() {
        return Err(Error::new(
            503,
            "Destination could not restore this VM runtime and disk.",
        ));
    }
    Ok(())
}

pub async fn handle(State(app): State<App>, request: Request) -> Result<Response> {
    let s = &app.service;
    if request.method() != "GET"
        && !(request.method() == "POST" && request.uri().path().ends_with("/renew"))
    {
        return Err(Error::new(405, "Method not allowed."));
    }
    let credential = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let _read = s.node_disk_reads.read().await;
    if let Some(grant) = super::disk_grants::authorize(s, credential).await? {
        if request.method() == "POST" {
            return Ok(axum::Json(json!({"renewed": true})).into_response());
        }
        let hash = request
            .uri()
            .path()
            .trim_start_matches("/internal/node-restore/");
        if !super::snapshots::valid_hash(hash) {
            return Err(Error::bad("Invalid block digest."));
        }
        // Grant transitions cannot remove a dependency during this read.
        for id in grant["backups"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let backup = s.get("node-backups", id).await?;
            let manifest = super::publication::manifest(s, &backup).await?;
            if manifest["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["hash"] == hash)
            {
                let bytes = super::publication::read_block(s, &backup, hash).await?;
                return Ok((
                    [("content-length", bytes.len().to_string())],
                    Body::from(bytes),
                )
                    .into_response());
            }
        }
        return Err(Error::new(403, "Block outside disk scope."));
    }
    Err(Error::new(401, "Disk grant expired."))
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
    let _lock = crate::file_lock::exclusive(&directory.join("lock"), "VM disk is active.")?;
    if directory.join("data.ext4").exists() {
        return Err(Error::new(
            409,
            "Destination still has a legacy local disk.",
        ));
    }
    let recovery = directory.join("recovery.json");
    if recovery.exists()
        && serde_json::from_slice::<Value>(&tokio::fs::read(&recovery).await?)?["backupId"]
            == value["backupId"]
        && crate::storage::runtime::exists(&directory)
        && !directory.join("restore.pending").exists()
    {
        return Ok(json!({"ready": true}));
    }
    let origin = super::connector::master(text(&value, "master"))?;
    let _replacement = crate::storage::runtime::replacement(&directory).await?;
    crate::skills::atomic_write(&directory.join("restore.pending"), b"restoring").await?;
    if directory.join("lazy").exists() {
        tokio::fs::rename(
            directory.join("lazy"),
            directory.join(format!("stale-{}-lazy", crate::config::id())),
        )
        .await?;
    }
    if !Path::new("/dev/fuse").exists() {
        return Err(Error::new(409, "Node has no FUSE device."));
    }
    let context =
        json!({"master": origin.as_str(),"grant": value["grant"],"policy": value["policy"]});
    let root = directory.join("lazy");
    drop(crate::storage::runtime::create(&root, manifest, &context).await?);
    crate::skills::atomic_write(
        &directory.join("runtime.json"),
        &serde_json::to_vec(&manifest["runtime"])?,
    )
    .await?;
    crate::skills::atomic_write(
        &recovery,
        &serde_json::to_vec(&json!({"backupId": value["backupId"]}))?,
    )
    .await?;
    tokio::fs::remove_file(directory.join("restore.pending")).await?;
    tokio::fs::File::open(&directory).await?.sync_all().await?;
    Ok(json!({"ready": true}))
}
