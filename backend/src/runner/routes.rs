//! HTTP interface of the VM controller.
use super::broker::{Broker, ProjectImport, erase_attempt_content};
use crate::{
    auth::safe_equal,
    error::{Error, Result},
    execution::Backend,
    microvm::host,
    skills::private_dir,
    validation::{text, uuid},
};
use axum::{
    Json,
    body::Body,
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{io::AsyncReadExt, sync::Mutex};
use tokio_util::sync::CancellationToken;

/// Protocol version of the controller API spoken with the manager.
const NODE_PROTOCOL: u32 = 2;
/// Longest node lease a single renewal may grant.
const MAX_LEASE_MS: u64 = 300_000;

async fn body(request: Request, limit: usize, message: &'static str) -> Result<Bytes> {
    axum::body::to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| Error::bad(message))
}

/// Parses a JSON body; a well-formed body of the wrong shape is reported as `message`.
fn decode<T: DeserializeOwned>(bytes: &[u8], message: &'static str) -> Result<T> {
    let value: Value = serde_json::from_slice(bytes)?;
    serde_json::from_value(value).map_err(|_| Error::bad(message))
}

fn require_post(request: &Request) -> Result<()> {
    if request.method() != "POST" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    Ok(())
}

fn json_response(value: Value) -> Result<Response> {
    Ok(Json(value).into_response())
}

async fn remove_entry(path: &Path, is_dir: bool) -> Result<()> {
    if is_dir {
        tokio::fs::remove_dir_all(path).await?;
    } else {
        tokio::fs::remove_file(path).await?;
    }
    Ok(())
}

pub(super) async fn handler(State(broker): State<Broker>, request: Request) -> Result<Response> {
    if request.uri().path() == "/health" {
        return health(&broker).await;
    }
    authorize(&broker, request.headers()).await?;
    let path = request.uri().path().trim_start_matches('/').to_owned();
    let segments = path.split('/').collect::<Vec<_>>();
    match segments.as_slice() {
        ["node-budget"] => node_budget(&broker, request).await,
        ["storage-policy"] => storage_policy(&broker, request).await,
        ["snapshots", snapshot, hash] => snapshot_file(&broker, request, snapshot, hash).await,
        ["disks", run, "snapshot"] => disk_snapshot(&broker, request, run).await,
        ["disks", run, operation @ ("storage-status" | "published")] => {
            disk_storage(&broker, request, run, operation).await
        }
        ["disks", run, "restore"] => disk_restore(&broker, request, run).await,
        ["disks", run, action] => disk_action(&broker, request, run, action).await,
        _ => run_route(broker, request, &segments).await,
    }
}

async fn health(broker: &Broker) -> Result<Response> {
    let mut runtimes = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(broker.state.join("images")).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let complete =
                entry.path().join("root.ext4").exists() && entry.path().join("vmlinux").exists();
            if complete {
                runtimes.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    let runtime_id = std::env::var("APP_RUNTIME_ID").unwrap_or_else(|_| "development".into());
    let active_runs = broker.active.lock().await.len();
    let mut capabilities = crate::nodes::connector::capabilities(&broker.state).ok();
    if let (Some(capabilities), Some(hardware)) = (&mut capabilities, broker.pool.hardware()) {
        capabilities["cpu"] = hardware["cpu"].clone();
        capabilities["memoryMiB"] = hardware["memoryMiB"].clone();
    }
    let (usage, pressure) = broker.pool.usage().await?;
    json_response(json!({
        "status": "ok",
        "backend": Backend::Firecracker,
        "runtimeId": runtime_id,
        "nodeProtocol": NODE_PROTOCOL,
        "sharedResources": broker.pool.budget().await.is_some(),
        "runtimes": runtimes,
        "dataRoot": broker.data,
        "activeRuns": active_runs,
        "capabilities": capabilities,
        "pool": broker.pool.health().await,
        "budget": broker.pool.budget().await,
        "usage": usage,
        "pressure": pressure,
    }))
}

async fn authorize(broker: &Broker, headers: &axum::http::HeaderMap) -> Result<()> {
    let credential = tokio::fs::read_to_string(broker.data.join("runner-secret")).await?;
    let authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !safe_equal(authorization, &format!("Bearer {}", credential.trim())) {
        return Err(Error::unauthorized("Invalid runner credential."));
    }
    Ok(())
}

async fn node_budget(broker: &Broker, request: Request) -> Result<Response> {
    require_post(&request)?;
    let bytes = body(request, 16384, "Invalid node budget.").await?;
    let budget = decode(&bytes, "Invalid node budget.")?;
    broker.pool.configure(budget).await?;
    json_response(json!({ "ready": true }))
}

async fn storage_policy(broker: &Broker, request: Request) -> Result<Response> {
    require_post(&request)?;
    let bytes = body(request, 16384, "Invalid storage policy.").await?;
    let policy: crate::storage::policy::Policy = serde_json::from_slice(&bytes)?;
    policy.validate()?;
    let state = broker.state.clone();
    tokio::task::spawn_blocking(move || crate::storage::fuse::probe(&state))
        .await
        .map_err(Error::internal)??;
    crate::skills::atomic_write(
        &broker.state.join("storage-policy.json"),
        &serde_json::to_vec(&policy)?,
    )
    .await?;
    json_response(json!({ "ready": true }))
}

/// Body of `POST /snapshots/{id}/blocks`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockBatch {
    hashes: Vec<String>,
}

async fn snapshot_file(
    broker: &Broker,
    request: Request,
    snapshot: &str,
    hash: &str,
) -> Result<Response> {
    uuid(snapshot)?;
    let directory = broker.state.join("snapshots").join(snapshot);
    if request.method() == "DELETE" && hash == "discard" {
        tokio::fs::remove_dir_all(directory).await?;
        return json_response(json!({ "ok": true }));
    }
    if request.method() == "POST" && hash == "blocks" {
        let bytes = body(request, 4096, "Invalid snapshot block batch.").await?;
        let batch: BlockBatch = serde_json::from_slice(&bytes)
            .map_err(|_| Error::bad("Invalid snapshot block batch."))?;
        let (length, blocks) =
            crate::nodes::snapshots::served_batch(&directory, batch.hashes).await?;
        return Ok(([("content-length", length.to_string())], blocks).into_response());
    }
    if request.method() != "GET" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let bytes = crate::nodes::snapshots::served(&directory, hash).await?;
    Ok(([("content-length", bytes.len().to_string())], bytes).into_response())
}

/// Recovery point of a stopped conversation's disk.
async fn disk_snapshot(broker: &Broker, request: Request, run: &str) -> Result<Response> {
    uuid(run)?;
    require_post(&request)?;
    let run = run.to_owned();
    let baseline = snapshot_baseline(request).await?;
    if broker.run_is_active(&run).await {
        return Err(Error::conflict(
            "Use the active attempt for a running VM snapshot.",
        ));
    }
    let state = broker.state.clone();
    let stop = broker.stop.child_token();
    let task = tokio::spawn(async move {
        crate::nodes::checkpoint::capture(
            &state,
            &run,
            None,
            Arc::new(Mutex::new(())),
            stop,
            &run,
            baseline.as_deref(),
        )
        .await
    });
    Ok(Json(task.await.map_err(Error::internal)??).into_response())
}

/// Receipt of a disk generation the master has durably published.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Publication {
    generation: Option<i64>,
    #[serde(default)]
    grant_id: Option<String>,
    #[serde(default)]
    backup_id: String,
}

async fn disk_storage(
    broker: &Broker,
    request: Request,
    run: &str,
    operation: &str,
) -> Result<Response> {
    uuid(run)?;
    require_post(&request)?;
    let directory = broker.state.join("disks").join(run);
    if !crate::storage::runtime::exists(&directory) {
        return Err(Error::conflict("Conversation has no S3-backed journal."));
    }
    let volume = crate::storage::runtime::load(&directory).await?;
    if operation == "storage-status" {
        return Ok(Json(volume.inspect().await?).into_response());
    }
    let bytes = body(request, 16384, "Invalid publication receipt.").await?;
    let receipt: Publication = decode(&bytes, "Missing generation.")?;
    let generation = receipt
        .generation
        .ok_or_else(|| Error::bad("Missing generation."))?;
    if receipt.grant_id != Some(volume.source.grant_id()) {
        return Err(Error::conflict("Publication belongs to a replaced disk."));
    }
    let backup_id = receipt.backup_id;
    uuid(&backup_id)?;
    let disk = volume.disk.clone();
    tokio::task::spawn_blocking(move || disk.commit_published(generation, &backup_id))
        .await
        .map_err(Error::internal)??;
    json_response(json!({ "committed": true }))
}

async fn disk_restore(broker: &Broker, request: Request, run: &str) -> Result<Response> {
    require_post(&request)?;
    let bytes = body(
        request,
        crate::nodes::snapshots::MAX_MANIFEST_BYTES,
        "Restore manifest too large.",
    )
    .await?;
    let manifest = serde_json::from_slice(&bytes)?;
    let restored = crate::nodes::restore::controller(&broker.state, run, manifest).await?;
    Ok(Json(restored).into_response())
}

/// `prune` drops stale disk copies; `delete` erases a conversation's disk and attempt data.
async fn disk_action(
    broker: &Broker,
    request: Request,
    run: &str,
    action: &str,
) -> Result<Response> {
    if request.method() != "POST" || !["delete", "prune"].contains(&action) {
        return Err(Error::method_not_allowed("Invalid workspace operation."));
    }
    uuid(run)?;
    let bytes = body(request, 4096, "Invalid workspace request.").await?;
    let _: serde::de::IgnoredAny = serde_json::from_slice(&bytes)?;
    let active = broker.active.lock().await;
    if active.values().any(|a| a.plan.run_id() == run) {
        return Err(Error::conflict("The workspace still has an active agent."));
    }
    let directory = broker.state.join("disks").join(run);
    private_dir(&directory).await?;
    let _lock =
        crate::file_lock::exclusive(&directory.join("lock"), "The workspace is still in use.")?;
    if action == "prune" {
        let retired_grants = prune_stale_disks(&directory).await?;
        return json_response(json!({ "pruned": true, "retiredGrants": retired_grants }));
    }
    match tokio::fs::remove_file(directory.join("data.ext4")).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    erase_attempt_content(broker, run).await?;
    // Preserve the lock inode: an overlapping boot must contend on it.
    let mut leftovers = tokio::fs::read_dir(&directory).await?;
    while let Some(entry) = leftovers.next_entry().await? {
        if entry.file_name() == "lock" {
            continue;
        }
        remove_entry(&entry.path(), entry.file_type().await?.is_dir()).await?;
    }
    json_response(json!({ "deleted": true }))
}

/// Removes older copies set aside when a recovery point replaced this disk and
/// returns the digests of their storage grants.
async fn prune_stale_disks(directory: &Path) -> Result<Vec<String>> {
    let mut retired_grants = Vec::new();
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_name().to_string_lossy().starts_with("stale-") {
            continue;
        }
        let is_dir = entry.file_type().await?.is_dir();
        if is_dir
            && (entry.path().join("journal.sqlite").exists()
                || entry.path().join("journal-v2.sqlite").exists())
        {
            let context = crate::storage::LazyDisk::context(&entry.path())?;
            retired_grants.push(crate::auth::digest(text(&context, "grant")));
        }
        remove_entry(&entry.path(), is_dir).await?;
    }
    Ok(retired_grants)
}

async fn run_route(broker: Broker, request: Request, segments: &[&str]) -> Result<Response> {
    let id = segments
        .get(1)
        .filter(|_| segments.first() == Some(&"runs"))
        .ok_or_else(|| Error::not_found("Not found"))?;
    uuid(id)?;
    match (request.method().as_str(), segments) {
        ("POST", ["runs", id, "snapshot"]) => attempt_snapshot(&broker, request, id).await,
        ("POST", ["runs", id, "lease"]) => renew_lease(&broker, request, id).await,
        ("POST", ["runs", id, "artifact"]) => export_artifact(&broker, request, id).await,
        ("POST", ["runs", id]) => {
            broker.start(id).await?;
            json_response(json!({}))
        }
        ("POST", ["runs", id, "projects", project_id]) => {
            let bytes = body(request, 16384, "Invalid project request.").await?;
            let import: ProjectImport =
                decode(&bytes, "Project import is outside its private workspace.")?;
            Ok(Json(broker.open_project(id, project_id, import).await?).into_response())
        }
        ("DELETE", ["runs", id]) => {
            broker.stop(id).await?;
            json_response(json!({}))
        }
        ("GET", ["runs", id, "logs"]) => Ok(logs(broker, (*id).to_owned())),
        ("POST", ["runs", id, "wait"]) => Ok(wait(broker, (*id).to_owned())),
        _ => Err(Error::method_not_allowed("Method not allowed.")),
    }
}

/// Recovery point of an attempt; a running VM is captured consistently through its guest.
async fn attempt_snapshot(broker: &Broker, request: Request, id: &str) -> Result<Response> {
    let id = id.to_owned();
    let baseline = snapshot_baseline(request).await?;
    let (run, socket, control, stop) = {
        let active = broker.active.lock().await;
        if let Some(attempt) = active.get(&id) {
            if attempt.stop.is_cancelled() {
                return Err(Error::conflict("VM is stopping."));
            }
            let socket = attempt
                .socket
                .get()
                .cloned()
                .ok_or_else(|| Error::conflict("VM is still starting."))?;
            (
                attempt.plan.run_id().to_owned(),
                Some(socket),
                attempt.control.clone(),
                attempt.stop.clone(),
            )
        } else {
            let run = match tokio::fs::read_to_string(broker.state_file(&id, "run")).await {
                Ok(run) => run,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // The manager records the attempt before the node accepts it.
                    // Publication may arrive in that gap; no disk is safe to capture yet.
                    return Err(Error::conflict(
                        "VM attempt is not ready for synchronization.",
                    ));
                }
                Err(error) => return Err(error.into()),
            };
            (
                run,
                None,
                Arc::new(Mutex::new(())),
                CancellationToken::new(),
            )
        }
    };
    let state = broker.state.clone();
    let task = tokio::spawn(async move {
        crate::nodes::checkpoint::capture(
            &state,
            &run,
            socket,
            control,
            stop,
            &id,
            baseline.as_deref(),
        )
        .await
    });
    Ok(Json(task.await.map_err(Error::internal)??).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lease {
    remaining_ms: Option<u64>,
}

async fn renew_lease(broker: &Broker, request: Request, id: &str) -> Result<Response> {
    let bytes = body(request, 1024, "Invalid lease.").await?;
    let lease: Lease = decode(&bytes, "Invalid lease duration.")?;
    let remaining = lease
        .remaining_ms
        .filter(|v| *v > 0 && *v <= MAX_LEASE_MS)
        .ok_or_else(|| Error::bad("Invalid lease duration."))?;
    if broker.has_stopped(id) {
        return Err(Error::conflict("Attempt stopped."));
    }
    broker
        .leases
        .lock()
        .await
        .insert(id.to_owned(), crate::nodes::boot_ms() + remaining);
    json_response(json!({ "ok": true }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactRequest {
    #[serde(default)]
    run_id: Value,
    #[serde(default)]
    path: String,
}

async fn export_artifact(broker: &Broker, request: Request, id: &str) -> Result<Response> {
    let bytes = body(request, 16384, "Invalid artifact request.").await?;
    let artifact: ArtifactRequest = decode(&bytes, "Invalid artifact request.")?;
    let (socket, run, stop) = {
        let active = broker.active.lock().await;
        let attempt = active
            .get(id)
            .ok_or_else(|| Error::conflict("VM is not active."))?;
        if artifact.run_id != attempt.plan.run_id() {
            return Err(Error::forbidden("Wrong artifact scope."));
        }
        let socket = attempt
            .socket
            .get()
            .cloned()
            .ok_or_else(|| Error::conflict("VM is not ready."))?;
        (
            socket,
            attempt.plan.run_id().to_owned(),
            attempt.stop.clone(),
        )
    };
    let root = broker.data.join("runs").join(run);
    let export = host::export_artifact(&socket, &artifact.path, &root);
    let (stream, size) = tokio::select! {
        () = stop.cancelled() => return Err(Error::conflict("VM stopped.")),
        result = tokio::time::timeout(Duration::from_secs(10), export) => {
            result.map_err(|_| Error::timeout("Artifact export timed out."))??
        }
    };
    let stream = tokio_util::io::ReaderStream::new(stream.take(size))
        .take_until(async move { stop.cancelled().await });
    Ok((
        [(axum::http::header::CONTENT_LENGTH, size.to_string())],
        Body::from_stream(stream),
    )
        .into_response())
}

/// Reads newly appended complete log lines from `offset`, waiting for output.
async fn next_log_lines(
    broker: &Broker,
    id: &str,
    offset: &mut u64,
    pending: &mut Vec<u8>,
) -> std::io::Result<Option<Bytes>> {
    use tokio::io::AsyncSeekExt;
    loop {
        if let Ok(mut file) = tokio::fs::File::open(broker.state_file(id, "log")).await {
            file.seek(std::io::SeekFrom::Start(*offset)).await?;
            let mut chunk = vec![0; 65536];
            let count = file.read(&mut chunk).await?;
            pending.extend_from_slice(&chunk[..count]);
            *offset += count as u64;
        }
        if let Some(end) = pending.iter().rposition(|b| *b == b'\n') {
            let rest = pending.split_off(end + 1);
            return Ok(Some(Bytes::from(std::mem::replace(pending, rest))));
        }
        if broker.state_file(id, "exit").exists() || broker.stop.is_cancelled() {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        if pending.is_empty() {
            return Ok(Some(Bytes::from_static(b"{\"type\":\"heartbeat\"}\n")));
        }
    }
}

/// NDJSON attempt output until its exit is recorded, with heartbeats while idle.
fn logs(broker: Broker, id: String) -> Response {
    let stream = futures_util::stream::try_unfold(
        (broker, id, 0_u64, Vec::new()),
        |(broker, id, mut offset, mut pending)| async move {
            let lines = next_log_lines(&broker, &id, &mut offset, &mut pending).await?;
            Ok::<_, std::io::Error>(lines.map(|lines| (lines, (broker, id, offset, pending))))
        },
    );
    (
        [("content-type", "application/x-ndjson")],
        Body::from_stream(stream),
    )
        .into_response()
}

/// Docker-compatible wait body; spaces keep the connection alive until exit.
fn wait(broker: Broker, id: String) -> Response {
    let stream =
        futures_util::stream::unfold((broker, id, false), |(broker, id, done)| async move {
            if done {
                return None;
            }
            // The exit marker is durable before attempt cleanup. Do not
            // invite a resume while start() would still reject its workspace.
            let released = !broker.active.lock().await.contains_key(&id);
            if released
                && let Ok(code) = tokio::fs::read_to_string(broker.state_file(&id, "exit")).await
            {
                let status = json!({ "StatusCode": code.parse::<i32>().unwrap_or(1) });
                return Some((
                    Ok::<_, std::io::Error>(Bytes::from(status.to_string())),
                    (broker, id, true),
                ));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            let stop = broker.stop.is_cancelled();
            Some((Ok(Bytes::from_static(b" ")), (broker, id, stop)))
        });
    (
        [("content-type", "application/json")],
        Body::from_stream(stream),
    )
        .into_response()
}

/// The latest recovery point the master published, which a capture may continue from.
async fn snapshot_baseline(request: Request) -> Result<Option<String>> {
    let bytes = body(request, 1024, "Invalid snapshot request.").await?;
    let baseline = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|body| body["baseline"].as_str().map(str::to_owned));
    Ok(baseline)
}
