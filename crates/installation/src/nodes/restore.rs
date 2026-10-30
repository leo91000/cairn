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
use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

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
    let record = s.node_boundary(node).await?;
    let native = crate::native::policy::node_native(&record);
    if native != matches!(manifest["kind"].as_str(), Some("native-ext4-v1")) {
        return Err(Error::conflict(
            "Conversation moves cannot change execution backend.",
        ));
    }
    let policy = crate::storage::policy::for_node(&record["storage"])?;
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
        && !(request.method() == "POST"
            && (request.uri().path().ends_with("/renew")
                || request.uri().path().ends_with("/report")))
    {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let credential = super::bearer(request.headers()).unwrap_or("");
    if request.method() == "POST" && request.uri().path().ends_with("/report") {
        let credential = credential.to_owned();
        return integrity_report(s, &credential, request).await;
    }
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
        if block["envelope"] == 3 {
            if request.uri().query() != Some("direct=1") {
                return Err(Error::conflict(
                    "This node must support direct storage reads.",
                ));
            }
            let storage = super::publication::storage_for(s, &backup)?;
            let object = text(block, "object");
            let ticket = storage
                .direct_read(
                    &s.vault,
                    hash,
                    object,
                    block["size"]
                        .as_u64()
                        .ok_or_else(|| Error::bad("Invalid block size."))?,
                )
                .await?;
            let (object, until) = (
                object.to_owned(),
                ticket.expires_at + crate::storage::direct::TRANSFER_SECONDS as i64 * 1000,
            );
            let grant_id = crate::auth::digest(credential);
            let hash = hash.to_owned();
            let run_id = text(&grant, "runId").to_owned();
            // Keep the object beyond every issued read and its bounded transfer.
            s.store
                .transaction(move |db| {
                    if super::disk_grants::authorize_id_db(db, &grant_id)?.is_none() {
                        return Err(Error::unauthorized("Disk grant expired."));
                    }
                    let pending:bool=db.0.query_row("SELECT EXISTS(SELECT 1 FROM direct_object_quarantines WHERE object=?1)",[&object],|r|r.get(0))?;
                    if pending {return Err(Error::unavailable("Shared block quarantine is being reconciled."));}
                    db.0.execute(
                        "UPDATE shared_objects SET retry_at=MAX(retry_at,?2) WHERE id=?1",
                        rusqlite::params![object, until],
                    )?;
                    // Retain only historical identity provenance, independently
                    // of pointer/grant removal and payload collection. This can
                    // authorize quarantine, never a GET/key or renewed access.
                    db.0.execute(
                        "INSERT OR IGNORE INTO direct_read_authorizations(grant_id,object,hash,run_id) VALUES(?1,?2,?3,?4)",
                        rusqlite::params![grant_id, object, hash, run_id],
                    )?;
                    Ok(())
                })
                .await?;
            return Ok((
                [(crate::storage::direct::RESPONSE_HEADER, "v3")],
                axum::Json(ticket),
            )
                .into_response());
        }
        let metrics = crate::performance::publication::restore_context(
            request.headers(),
            text(&grant, "runId"),
        );
        let bytes = crate::performance::publication::scope(
            metrics,
            super::publication::read_manifest_block(s, &backup, block),
        )
        .await?;
        return Ok((
            [("content-length", bytes.len().to_string())],
            Body::from(bytes),
        )
            .into_response());
    }
    Err(Error::forbidden("Block outside disk scope."))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IntegrityReport {
    object: String,
    hash: String,
}

async fn integrity_report(
    s: &Arc<Service>,
    credential: &str,
    request: Request,
) -> Result<Response> {
    let body = axum::body::to_bytes(request.into_body(), 1024)
        .await
        .map_err(|_| Error::bad("Invalid direct storage failure report."))?;
    let report: IntegrityReport = serde_json::from_slice(&body)
        .map_err(|_| Error::bad("Invalid direct storage failure report."))?;
    crate::validation::uuid(&report.object)?;
    if !super::snapshots::valid_hash(&report.hash) {
        return Err(Error::bad("Invalid direct storage failure report."));
    }
    let grant = crate::auth::digest(credential);
    let s = s.clone();
    // Once accepted, caller cancellation cannot abandon remote acknowledgement
    // fencing after its durable preparation marker has been written.
    tokio::spawn(async move { reconcile_integrity(&s, grant, report).await })
        .await
        .map_err(Error::internal)?
}

async fn reconcile_integrity(
    s: &Service,
    grant: String,
    report: IntegrityReport,
) -> Result<Response> {
    let (scope_grant, scope_object, scope_hash) =
        (grant.clone(), report.object.clone(), report.hash.clone());
    let _run = s
        .store
        .read(move |db| {
            let run = db
                .0
                .query_row(
                    "SELECT run_id FROM direct_read_authorizations WHERE grant_id=?1 AND object=?2 AND hash=?3",
                    rusqlite::params![scope_grant, scope_object, scope_hash],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            run.ok_or_else(|| Error::forbidden("Reported block outside historical disk scope."))
        })
        .await?;
    super::journal_acknowledgements::quarantine(s, &report.object).await?;
    Ok(axum::Json(json!({ "invalidated": true })).into_response())
}

pub async fn controller(state: &Path, run: &str, value: Value) -> Result<Value> {
    crate::validation::uuid(run)?;
    let manifest = &value["manifest"];
    super::snapshots::validate(manifest)?;
    let _images = crate::microvm::images::IMAGE_LIFECYCLE.read().await;
    let image = state.join("images").join(super::component_runtime());
    if !image.join("root.ext4").exists() || !image.join("vmlinux").exists() {
        return Err(Error::conflict(
            "Current VM runtime is not installed on this node.",
        ));
    }
    restore_journal(state, run, value).await
}

/// Shared journal restore; native hosts validate their pinned tools instead of
/// VM images. Both backends attach the restored journal through ublk.
pub(crate) async fn restore_journal(state: &Path, run: &str, value: Value) -> Result<Value> {
    let manifest = &value["manifest"];
    let lease =
        crate::storage::environment::lock(state, run, "Conversation disk is active.").await?;
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
    let origin = super::connector::disk_master(text(&value, "master"))?;
    let _replacement = crate::storage::runtime::replacement(directory).await?;
    crate::skills::atomic_write(&directory.join("restore.pending"), b"restoring").await?;
    if directory.join("lazy").exists() {
        tokio::fs::rename(
            directory.join("lazy"),
            directory.join(format!("stale-{}-lazy", crate::config::id())),
        )
        .await?;
    }
    let context = json!({
        "master": origin.as_str(),
        "grant": value["grant"],
        "policy": value["policy"],
    });
    let root = directory.join("lazy");
    drop(crate::storage::runtime::create(&root, manifest, &context).await?);
    let mut runtime = manifest["runtime"].clone();
    runtime["runtimeId"] = super::component_runtime().into();
    crate::skills::atomic_write(
        &directory.join("runtime.json"),
        &serde_json::to_vec(&runtime)?,
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
