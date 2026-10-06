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
use serde::Serialize;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

/// What a destination controller needs to mount a published disk on demand.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RestoreRequest<'a> {
    manifest: Value,
    master: &'a str,
    grant: String,
    backup_id: &'a Value,
    policy: crate::storage::policy::Policy,
}

fn ready() -> Value {
    json!({ "ready": true })
}

pub async fn start(s: &Service, run: &Value, node: &str, backup: &Value) -> Result<()> {
    let _operation = s.node_backup_operation.lock(text(run, "id")).await;
    let manifest = super::publication::manifest(s, backup).await?;
    let record = s.get("nodes", node).await?;
    if record["capabilities"]["fuse"] != true {
        return Err(Error::conflict("Destination has no FUSE device."));
    }
    let policy = crate::storage::policy::Policy::for_node(&record["storage"])?;
    let grant = super::disk_grants::issue(s, run, node, backup).await?;
    let request = RestoreRequest {
        manifest,
        // The local controller needs the internal origin. For a remote node,
        // the outbound transport replaces it with its authenticated origin.
        master: &s.config.public_url,
        grant,
        backup_id: &backup["id"],
        policy,
    };
    let response = s
        .http
        .post(format!(
            "{}/disks/{}/restore",
            super::controller_url(s, node),
            text(run, "id")
        ))
        .bearer_auth(super::runner_secret(s).await?)
        .json(&request)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|_| Error::unavailable("VM restore connection interrupted."))?;
    if !response.status().is_success() {
        return Err(Error::unavailable(
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
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let credential = super::bearer(request.headers()).unwrap_or("");
    let grant = s
        .store
        .get("node-disk-grants", &crate::auth::digest(credential))
        .await?
        .ok_or_else(|| Error::unauthorized("Disk grant expired."))?;
    let _read = s.node_backup_operation.read(text(&grant, "runId")).await;

    let Some(grant) = super::disk_grants::authorize(s, credential).await? else {
        return Err(Error::unauthorized("Disk grant expired."));
    };
    if request.method() == "POST" {
        return Ok(axum::Json(json!({ "renewed": true })).into_response());
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
        let Some(block) = super::snapshots::find_block(&manifest, hash) else {
            continue;
        };
        let bytes = super::publication::read_manifest_block(s, &backup, block).await?;
        return Ok((
            [("content-length", bytes.len().to_string())],
            Body::from(bytes),
        )
            .into_response());
    }
    Err(Error::forbidden("Block outside disk scope."))
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
        return Err(Error::conflict(
            "Required VM runtime is not installed on this node.",
        ));
    }
    let lease = crate::storage::environment::lock(state, run, "VM disk is active.").await?;
    let directory = &lease.directory;
    if directory.join("data.ext4").exists() {
        return Err(Error::conflict(
            "Destination still has a legacy local disk.",
        ));
    }
    let recovery = directory.join("recovery.json");
    let restored = recovery.exists()
        && serde_json::from_slice::<Value>(&tokio::fs::read(&recovery).await?)?["backupId"]
            == value["backupId"];
    if restored
        && crate::storage::runtime::exists(directory)
        && !directory.join("restore.pending").exists()
    {
        return Ok(ready());
    }
    let origin = super::connector::master(text(&value, "master"))?;
    let _replacement = crate::storage::runtime::replacement(directory).await?;
    crate::skills::atomic_write(&directory.join("restore.pending"), b"restoring").await?;
    if directory.join("lazy").exists() {
        tokio::fs::rename(
            directory.join("lazy"),
            directory.join(format!("stale-{}-lazy", crate::config::id())),
        )
        .await?;
    }
    if !Path::new("/dev/fuse").exists() {
        return Err(Error::conflict("Node has no FUSE device."));
    }
    let context = json!({
        "master": origin.as_str(),
        "grant": value["grant"],
        "policy": value["policy"],
    });
    let root = directory.join("lazy");
    drop(crate::storage::runtime::create(&root, manifest, &context).await?);
    crate::skills::atomic_write(
        &directory.join("runtime.json"),
        &serde_json::to_vec(&manifest["runtime"])?,
    )
    .await?;
    crate::skills::atomic_write(
        &recovery,
        &serde_json::to_vec(&json!({ "backupId": value["backupId"] }))?,
    )
    .await?;
    tokio::fs::remove_file(directory.join("restore.pending")).await?;
    tokio::fs::File::open(&directory).await?.sync_all().await?;
    Ok(ready())
}
