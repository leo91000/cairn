//! Publish the current disk to S3, then collect unreachable generations.
//! Persisted record and object names remain compatible with existing disks.
use super::snapshots;
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::Duration,
};

// Versioned binary blocks avoid both JSON/base64 layers used by legacy records.
const BINARY_BLOCK_HEADER: &[u8] = b"LEOBLK\x01\0";

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Settings {
    pub interval_seconds: u64,
    pub budget_mi_b: u64,
    pub disconnect_timeout_seconds: u64,
    pub shutdown_timeout_seconds: u64,
    pub max_capacity_wait_seconds: u64,
    #[serde(skip_serializing)]
    pub s3_configured: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            interval_seconds: 60,
            budget_mi_b: 102400,
            disconnect_timeout_seconds: 60,
            shutdown_timeout_seconds: 300,
            max_capacity_wait_seconds: 3600,
            s3_configured: None,
        }
    }
}

impl Settings {
    fn validate(&self) -> Result<()> {
        if !(5..=3600).contains(&self.interval_seconds)
            || !(128..=1_048_576).contains(&self.budget_mi_b)
            || !(10..=300).contains(&self.disconnect_timeout_seconds)
            || !(30..=300).contains(&self.shutdown_timeout_seconds)
            || self.max_capacity_wait_seconds > 3600
        {
            return Err(Error::bad("Invalid synchronization settings."));
        }
        Ok(())
    }
}

pub async fn settings(s: &Service) -> Result<Value> {
    let mut value = serde_json::to_value(Settings::default())?;
    if let Some(saved) = s.store.kv("node-backup-settings").await? {
        // Import only supported settings. Old destination/retention values have no effect.
        for (key, field) in value.as_object_mut().unwrap() {
            if let Some(saved) = saved.get(key) {
                *field = saved.clone();
            }
        }
    }
    Ok(value)
}

pub async fn configure(s: &Service, input: Value) -> Result<Value> {
    let settings: Settings =
        serde_json::from_value(input).map_err(|e| Error::bad(e.to_string()))?;
    settings.validate()?;
    let value = serde_json::to_value(settings)?;
    s.store
        .set("node-backup-settings", value.clone(), None)
        .await?;
    s.store
        .audit("node.synchronization.configured", value.clone())
        .await?;
    Ok(value)
}

fn root(s: &Service, run: &str) -> PathBuf {
    s.config.data_dir.join("node-backups").join(run)
}

fn key(run: &str, hash: &str) -> String {
    format!("node-backups/{run}/blocks/{hash}")
}

pub async fn capture(s: &Service, run: &Value) -> Result<Value> {
    let result = publish(s, run).await;
    tracing::info!(target: "leo_performance", operation = "s3_totals", id = text(run, "id"),
        success = result.is_ok(), metrics = %s.hot_s3.performance());
    // A capture continuing from a baseline may rely on blocks the master no longer
    // holds: forget the baseline so the next capture copies the whole disk.
    if result.as_ref().is_err_and(|error| error.status != 425)
        && run["backup"]["snapshotId"].is_string()
    {
        let _ = forget_baseline(s, text(run, "id")).await;
    }
    result
}

async fn forget_baseline(s: &Service, run_id: &str) -> Result<()> {
    update_status(s, run_id, json!({"snapshotId": null})).await
}

async fn publish(s: &Service, run: &Value) -> Result<Value> {
    let mut timing =
        crate::performance::Operation::new("disk_publication", text(run, "id"), "queue");
    let _operation = s.node_backup_operation.lock().await;
    timing.next("collect_before");
    let run_id = text(run, "id");
    crate::validation::uuid(run_id)?;
    let checkpoint = s
        .store
        .kv(&format!("run-checkpoint:{run_id}"))
        .await?
        .unwrap_or_default();
    let attempt = text(&checkpoint, "runnerId");
    crate::validation::uuid(attempt)?;
    let settings = settings(s).await?;
    let storage = crate::object_storage::Storage::configured(s)?;
    collect_unused(s, run_id).await?;
    timing.next("snapshot");
    let base = super::transport::url(s, run_id).await?;
    let credential = crate::execution::secret(&s.config.data_dir, "runner-secret").await?;
    let capture_path = if run["status"] != "running" || run["moveRequest"]["idle"] == true {
        format!("{base}/disks/{run_id}/snapshot")
    } else {
        format!("{base}/runs/{attempt}/snapshot")
    };
    let response = s
        .http
        .post(capture_path)
        .bearer_auth(&credential)
        // The node copies only blocks written since this published point if it still tracks it.
        .json(&json!({"baseline": run["backup"]["snapshotId"]}))
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|_| Error::new(503, "Snapshot capture interrupted."))?;
    if response.status() == reqwest::StatusCode::CONFLICT {
        // Startup, shutdown and an overlapping capture are temporary ownership
        // conflicts. Keep them distinct from broken storage or S3 configuration.
        return Err(Error::new(
            425,
            "Waiting for the VM to be ready for synchronization.",
        ));
    }
    if !response.status().is_success() {
        return Err(Error::new(503, "Unable to capture a coherent VM snapshot."));
    }
    let snapshot: Value = response.json().await.map_err(Error::internal)?;
    timing.next("prepare_upload");
    let snapshot_id = text(&snapshot, "id");
    crate::validation::uuid(snapshot_id)?;
    let result = async {
        let manifest = &snapshot["manifest"];
        snapshots::validate(manifest)?;
        let directory = root(s, run_id);
        crate::skills::private_dir(&directory.join("blocks")).await?;
        let published = published_blocks(s, run_id, &storage).await?;
        let mut seen = HashSet::new();
        let mut uploaded = 0u64;
        let occupied = used(s).await?;
        let budget = settings["budgetMiB"].as_u64().unwrap_or(102400) * 1024 * 1024;
        // Durable upload intent: the complete block inventory exists before any PUT.
        // A restart can collect an interrupted first publication without S3 bucket scans.
        let backup_id = id();
        let mut value = json!({
            "id": backup_id,
            "snapshotId": snapshot_id,
            "runId": run_id,
            "nodeId": checkpoint["nodeId"],
            "createdAt": now(),
            "capturedAt": manifest["capturedAt"],
            "diskGeneration": manifest["generation"],
            "sessionId": run["sessionId"],
            "destination": "s3",
            "bucket": storage.bucket,
            "endpoint": storage.endpoint,
            "uploadedBytes": 0,
            "pauseMs": manifest["pauseMs"],
            "indexMs": manifest["indexMs"],
            "localBytesRead": manifest["localBytesRead"],
            "incremental": manifest["incremental"],
            "manifest": s.vault.encrypt(&format!("backup:{backup_id}"),manifest)?
        });
        let path = directory.join(format!("{backup_id}.json"));
        let intent = serde_json::to_vec(&value)?;
        if occupied.saturating_add(intent.len() as u64) > budget {
            return Err(Error::new(
                507,
                "Publication cache budget exhausted; the current disk remains available.",
            ));
        }
        crate::skills::atomic_write(&path, &intent).await?;
        // Persist the rename and newly created run/root directories before remote writes.
        for parent in directory.ancestors().take(3) {
            tokio::fs::File::open(parent).await?.sync_all().await?;
        }
        let mut uploads = tokio::task::JoinSet::<Result<()>>::new();
        timing.next("transfer_blocks");
        for block in manifest["blocks"].as_array().unwrap() {
            let Some(hash) = block["hash"].as_str() else {
                continue;
            };
            if !seen.insert(hash.to_owned()) {
                continue;
            }
            let file = directory.join("blocks").join(hash);
            if published.get(hash).copied() == block["size"].as_u64() {
                // A committed immutable S3 copy remains usable after local
                // cache and receipt eviction.
                continue;
            }
            let mut verified = false;
            if !file.exists() {
                let response = s
                    .http
                    .get(format!("{base}/snapshots/{snapshot_id}/{hash}"))
                    .bearer_auth(&credential)
                    .timeout(Duration::from_secs(120))
                    .send()
                    .await
                    .map_err(|_| Error::new(503, "Backup block transfer interrupted."))?;
                if !response.status().is_success() {
                    return Err(Error::new(503, "Backup block unavailable."));
                }
                let bytes = super::snapshots::response_block(response).await?;
                if bytes.len() as u64 != block["size"].as_u64().unwrap()
                    || hex::encode(Sha256::digest(&bytes)) != hash
                {
                    return Err(Error::bad("Backup block failed integrity verification."));
                }
                let vault = s.vault.clone();
                let scope = key(run_id, hash);
                let length = bytes.len() as u64;
                let encoded = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                    let mut encoded = BINARY_BLOCK_HEADER.to_vec();
                    encoded.extend(vault.encrypt_bytes(&scope, &bytes)?);
                    Ok(encoded)
                })
                .await
                .map_err(Error::internal)??;
                enqueue_verified_block(&mut uploads, &storage, &file, key(run_id, hash), encoded)
                    .await?;
                uploaded += length;
                continue;
            } else if published.get(hash).copied() != block["size"].as_u64() {
                // Orphan files from interrupted publication have no retained proof.
                // Published blocks are immutable and already validated at receipt.
                let bytes = decode_block(s, run_id, hash, tokio::fs::read(&file).await?).await?;
                if bytes.len() as u64 != block["size"].as_u64().unwrap() {
                    return Err(Error::bad("Cached backup block has the wrong size."));
                }
                verified = true;
            }
            {
                let location = json!({"destination": "s3","bucket": storage.bucket,"endpoint": storage.endpoint});
                let mark = receipt(&file, &location)?;
                if !mark.exists() {
                    // Verify before copying an existing block to a new destination.
                    let encoded = tokio::fs::read(&file).await?;
                    if !verified {
                        decode_block(s, run_id, hash, encoded.clone()).await?;
                    }
                    enqueue_verified_block(
                        &mut uploads,
                        &storage,
                        &file,
                        key(run_id, hash),
                        encoded,
                    )
                    .await?;
                }
            }
        }
        while let Some(upload) = uploads.join_next().await {
            upload.map_err(Error::internal)??;
        }
        tracing::info!(target: "leo_performance", operation = "disk_publication", id = run_id, uploaded_bytes = uploaded, referenced_blocks = seen.len());
        timing.next("publish_manifest");
        value["uploadedBytes"]=uploaded.into();
        crate::skills::atomic_write(&path,&serde_json::to_vec(&value)?).await?;
        upload_verified(&storage,&path,&format!("node-backups/{run_id}/{backup_id}.json")).await?;
        let patch=json!({
            "backup": {
                "id": backup_id,
                "snapshotId": snapshot_id,
                "capturedAt": manifest["capturedAt"],
                "uploadedBytes": uploaded,
                "status": "ready",
                "error": null
            }
        });
        let (owner_run,owner_attempt,owner_node,point)=(run_id.to_owned(),attempt.to_owned(),checkpoint["nodeId"].clone(),value.clone());
        s.store.transaction(move |db| {
            let current=db.kv(&format!("run-checkpoint:{owner_run}"))?.unwrap_or_default();
            if current["runnerId"]!=owner_attempt || current["nodeId"]!=owner_node {return Err(Error::new(409,"Disk owner changed during publication."));}
            db.put("node-backups",&point)?;db.patch_run(&owner_run,&patch)?;Ok(())
        }).await?;
        timing.next("acknowledge_journal");
        if manifest["onDemand"]==true {
            super::disk_grants::extend(s,text(&snapshot,"grantId"),&value).await?;
            let response=s.http.post(format!("{base}/disks/{run_id}/published")).bearer_auth(&credential).json(&json!({
                "generation": manifest["generation"],
                "backupId": backup_id,
                "grantId": snapshot["grantId"]
            })).timeout(Duration::from_secs(120)).send().await.map_err(|_|Error::new(503,"Disk publication acknowledgement interrupted."))?;
            if !response.status().is_success(){return Err(Error::new(503,"Node could not acknowledge the published disk."));}
            super::disk_grants::acknowledged(s,text(&snapshot,"grantId"),&value).await?;
            // Completed runs leave the active-run monitor. Read back the final
            // journal counters so their UI does not retain an old dirty count.
            let _ = super::storage::refresh(s, run).await;
        }
        timing.next("collect_after");
        collect_unused(s,run_id).await?;
        Ok(public(value))
    }.await;
    if result.is_ok() {
        timing.next("discard_snapshot");
    }
    let _ = s
        .http
        .delete(format!("{base}/snapshots/{snapshot_id}/discard"))
        .bearer_auth(credential)
        .timeout(Duration::from_secs(20))
        .send()
        .await;
    if result.is_ok() {
        timing.finish();
    }
    result
}

/// Every upload path publishes its receipt only after remote read-back verification.
async fn enqueue_verified_block(
    uploads: &mut tokio::task::JoinSet<Result<()>>,
    storage: &crate::object_storage::Storage,
    file: &std::path::Path,
    key: String,
    encoded: Vec<u8>,
) -> Result<()> {
    let location =
        json!({"destination": "s3","bucket": storage.bucket,"endpoint": storage.endpoint});
    let mark = receipt(file, &location)?;
    let receipt_bytes = serde_json::to_vec(&location)?;
    let storage = storage.clone();
    uploads.spawn(async move {
        storage.upload_bytes(encoded, &key).await?;
        crate::skills::atomic_write(&mark, &receipt_bytes).await
    });
    if uploads.len() >= crate::object_storage::HOT_WRITE_CONCURRENCY {
        uploads
            .join_next()
            .await
            .unwrap()
            .map_err(Error::internal)??;
    }
    Ok(())
}

/// A retained, authenticated manifest proves these immutable blocks were validated
/// before publication. Read manifests, not payloads, on the incremental path.
async fn published_blocks(
    s: &Service,
    run: &str,
    storage: &crate::object_storage::Storage,
) -> Result<HashMap<String, u64>> {
    let mut blocks = HashMap::new();
    if s.store.run(run).await?["backup"]["snapshotId"].is_null() {
        // A failed remote read invalidates the previous publication as a
        // deduplication source. The next capture must re-upload every block.
        return Ok(blocks);
    }
    for point in s.store.node_backups_for_run(run).await? {
        if point["destination"] != "s3"
            || point["bucket"].as_str() != Some(storage.bucket.as_str())
            || point["endpoint"].as_str() != storage.endpoint.as_deref()
        {
            continue;
        }
        if let Ok(manifest) = manifest(s, &point).await {
            for block in manifest["blocks"].as_array().unwrap() {
                if let Some(hash) = block["hash"].as_str() {
                    blocks.insert(hash.to_owned(), block["size"].as_u64().unwrap());
                }
            }
        }
    }
    Ok(blocks)
}

fn receipt(file: &std::path::Path, location: &Value) -> Result<PathBuf> {
    let location = json!({
        "destination": "s3",
        "bucket": location["bucket"],
        "endpoint": location["endpoint"]
    });
    Ok(file.with_extension(format!(
        "s3-{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&location)?))
    )))
}

pub fn public(mut value: Value) -> Value {
    value.as_object_mut().unwrap().remove("manifest");
    value
}

pub async fn manifest(s: &Service, backup: &Value) -> Result<Value> {
    let value = s.vault.decrypt(
        &format!("backup:{}", text(backup, "id")),
        &backup["manifest"],
    )?;
    snapshots::validate(&value)?;
    Ok(value)
}

pub async fn read_block(s: &Service, backup: &Value, hash: &str) -> Result<Vec<u8>> {
    if !snapshots::valid_hash(hash) {
        return Err(Error::bad("Invalid backup block."));
    }
    let run = text(backup, "runId");
    let path = root(s, run).join("blocks").join(hash);
    if let Ok(encoded) = tokio::fs::read(&path).await {
        match decode_block(s, run, hash, encoded).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                // A failed restore must not leave known-bad bytes in the cache.
                tokio::fs::remove_file(&path).await?;
                if backup["destination"] != "s3" {
                    forget_baseline(s, run).await?;
                    return Err(error);
                }
            }
        }
    }
    if backup["destination"] != "s3" {
        forget_baseline(s, run).await?;
        return Err(Error::new(503, "Local recovery block is missing."));
    }
    let storage = storage_for(s, backup)?;
    crate::skills::private_dir(path.parent().unwrap()).await?;
    let recovered = async {
        let encoded = storage
            .download_bytes(&key(run, hash), snapshots::BLOCK * 3 + 4096)
            .await?;
        let bytes = decode_block(s, run, hash, encoded.clone()).await?;
        Ok::<_, Error>((encoded, bytes))
    }
    .await;
    let (_, bytes) = match recovered {
        Ok(recovered) => recovered,
        Err(error) => {
            // An outage or access refusal says nothing about the integrity of a verified copy.
            if matches!(error.status, 400 | 409) {
                let mark = receipt(&path, backup)?;
                if mark.exists() {
                    tokio::fs::remove_file(mark).await?;
                }
                forget_baseline(s, run).await?;
            }
            return Err(error);
        }
    };
    Ok(bytes)
}

async fn decode_block(s: &Service, run: &str, hash: &str, encoded: Vec<u8>) -> Result<Vec<u8>> {
    let vault = s.vault.clone();
    let scope = key(run, hash);
    let hash = hash.to_owned();
    tokio::task::spawn_blocking(move || {
        let bytes = if let Some(ciphertext) = encoded.strip_prefix(BINARY_BLOCK_HEADER) {
            vault.decrypt_bytes(&scope, ciphertext)?
        } else {
            // Published S3 blocks remain readable independently of archive removal.
            let value: Value = serde_json::from_slice(&encoded)?;
            let plaintext = vault.decrypt(&scope, &value)?;
            STANDARD
                .decode(plaintext.as_str().unwrap_or(""))
                .map_err(|_| Error::bad("Invalid backup ciphertext."))?
        };
        if hex::encode(Sha256::digest(&bytes)) != hash {
            return Err(Error::bad("Backup integrity check failed."));
        }
        Ok(bytes)
    })
    .await
    .map_err(Error::internal)?
    // Decoding has no network or filesystem effects. Invalid envelopes,
    // authentication failures and digest mismatches all require repair.
    .map_err(|_| Error::new(409, "Recovery block integrity check failed."))
}

/// The local controller owns the configured node cache. Retire duplicate S3
/// payloads on the master.
pub async fn maintain_local_cache(s: &Service) -> Result<()> {
    let Ok(_operation) = s.node_backup_operation.try_lock() else {
        return Ok(());
    };
    let _guard = s.node_backup_lock.lock().await;
    let budget = settings(s).await?["budgetMiB"].as_u64().unwrap_or(102400) * 1048576;
    make_room(s, used(s).await?, 0, budget).await?;
    Ok(())
}

async fn make_room(s: &Service, mut occupied: u64, incoming: u64, budget: u64) -> Result<u64> {
    // Keep this admission policy separate from storage::cache: publication may
    // use the emergency reserve so a dirty journal can still make progress.
    let node = s
        .store
        .get("nodes", super::LOCAL_NODE_ID)
        .await?
        .unwrap_or_default();
    let policy = crate::storage::policy::Policy::for_node(&node["storage"])?;
    let (total, _) = crate::storage::policy::space(&s.config.data_dir)?;
    let reserve = policy.reserve(total);
    let required = occupied
        .saturating_add(incoming)
        .saturating_sub(budget)
        .max(
            reserve
                .saturating_add(incoming.saturating_mul(3))
                .saturating_sub(crate::storage::policy::space(&s.config.data_dir)?.1),
        );
    let mut candidates = Vec::new();
    let root = s.config.data_dir.join("node-backups");
    if root.exists() {
        let mut runs = tokio::fs::read_dir(root).await?;
        while let Some(run) = runs.next_entry().await? {
            if !run.file_type().await?.is_dir() {
                continue;
            }
            let blocks = run.path().join("blocks");
            if !blocks.exists() {
                continue;
            }
            let mut entries = tokio::fs::read_dir(&blocks).await?;
            while let Some(entry) = entries.next_entry().await? {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some((hash, _)) = name.split_once(".s3-") else {
                    continue;
                };
                if !snapshots::valid_hash(hash) {
                    continue;
                }
                let file = blocks.join(hash);
                if let Ok(meta) = tokio::fs::metadata(&file).await {
                    candidates.push((
                        file,
                        meta.len(),
                        meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                    ));
                }
            }
        }
    }
    candidates.sort_by_key(|v| v.2);
    let mut removed = 0;
    for (file, size, _) in candidates {
        if tokio::fs::try_exists(&file).await? {
            tokio::fs::remove_file(file).await?;
            removed += size;
            occupied = occupied.saturating_sub(size);
        }
    }
    if removed < required
        && (occupied.saturating_add(incoming) > budget
            || crate::storage::policy::space(&s.config.data_dir)?.1
                < incoming.saturating_mul(3).saturating_add(32 * 1024 * 1024))
    {
        return Err(Error::new(
            507,
            "Backup cache and free-space reserve are exhausted; unsaved work is retained.",
        ));
    }
    Ok(occupied)
}

async fn used(s: &Service) -> Result<u64> {
    let mut total = 0u64;
    let mut pending = vec![s.config.data_dir.join("node-backups")];
    while let Some(path) = pending.pop() {
        if !path.exists() {
            continue;
        }
        let mut entries = tokio::fs::read_dir(path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let meta = entry.metadata().await?;
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

/// Remove superseded publications only after every disk has released its read grant.
/// This also serves idle disks: cleanup must not depend on another guest write.
pub async fn collect(s: &Service, run: &str) -> Result<()> {
    let _operation = s.node_backup_operation.lock().await;
    collect_unused(s, run).await
}

async fn collect_unused(s: &Service, run: &str) -> Result<()> {
    let mut timing = crate::performance::Operation::new("disk_collection", run, "reader_lock");
    let _readers = s.node_disk_reads.write().await;
    timing.next("inventory");
    let pinned = super::disk_grants::pinned(s, run).await?;
    let _guard = s.node_backup_lock.lock().await;
    let points = s.store.node_backups_for_run(run).await?;
    let current = s.store.run(run).await?;
    let mut needed = HashSet::new();
    let mut keep = pinned;
    // The publication pointer is authoritative even if timestamps are equal or clocks regress.
    if let Some(head) = current["backup"]["id"].as_str() {
        keep.insert(head.to_owned());
    } else if !points.is_empty() {
        return Err(Error::new(
            409,
            "Published disk pointer missing; cleanup deferred.",
        ));
    }
    if let Some(id) = current["moveRequest"]["backupId"].as_str() {
        keep.insert(id.to_owned());
    }
    if keep
        .iter()
        .any(|id| !points.iter().any(|point| point["id"] == *id))
    {
        return Err(Error::new(
            409,
            "A referenced disk manifest is missing; cleanup deferred.",
        ));
    }
    for point in points.iter().filter(|p| keep.contains(text(p, "id"))) {
        // A damaged retained manifest must not prevent creating a fresh good point,
        // nor cause dependencies we cannot identify to be deleted.
        let Ok(manifest) = manifest(s, point).await else {
            return Ok(());
        };
        for block in manifest["blocks"].as_array().unwrap() {
            if let Some(hash) = block["hash"].as_str() {
                needed.insert(hash.to_owned());
            }
        }
    }
    // Readers of retired grants have drained. The operation lock still serializes
    // publication/restoration; release cache/read locks before remote deletion so
    // unrelated mounted disks can keep reading while S3 cleanup is slow.
    drop(_guard);
    drop(_readers);
    timing.next("delete_objects");
    for point in points.iter().filter(|p| !keep.contains(text(p, "id"))) {
        let point_id = text(point, "id").to_owned();
        if point["destination"] == "s3" && !root(s, run).join(format!("{point_id}.json")).exists() {
            storage_for(s, point)?
                .purge_key(&format!("node-backups/{run}/{}.json", text(point, "id")))
                .await?;
        }
        s.store
            .write(move |db| db.remove("node-backups", &point_id))
            .await?;
    }
    // Local manifests also serve as upload intents. They precede every remote write,
    // including first publication, and remain discoverable if the DB commit never happened.
    if root(s, run).exists() {
        let mut entries = tokio::fs::read_dir(root(s, run)).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(point_id) = name.strip_suffix(".json") else {
                continue;
            };
            if crate::validation::uuid(point_id).is_err() || keep.contains(point_id) {
                continue;
            }
            let point: Value = serde_json::from_slice(&tokio::fs::read(entry.path()).await?)?;
            if point["id"] != point_id || point["runId"] != run {
                return Err(Error::bad(
                    "Invalid publication upload inventory; cleanup deferred.",
                ));
            }
            let manifest = manifest(s, &point).await?;
            if point["destination"] == "s3" {
                let storage = storage_for(s, &point)?;
                let hashes = manifest["blocks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|block| block["hash"].as_str())
                    .collect::<HashSet<_>>();
                for hash in hashes {
                    if !needed.contains(hash)
                        && !receipt(&root(s, run).join("blocks").join(hash), &point)?.exists()
                    {
                        storage.purge_key(&key(run, hash)).await?;
                    }
                }
                storage
                    .purge_key(&format!("node-backups/{run}/{point_id}.json"))
                    .await?;
            }
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    let blocks = root(s, run).join("blocks");
    if blocks.exists() {
        let mut entries = tokio::fs::read_dir(&blocks).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let hash = name.split('.').next().unwrap_or("");
            if !snapshots::valid_hash(hash) || needed.contains(hash) {
                continue;
            }
            if let Some((_, bucket)) = name.split_once(".s3-") {
                let location = serde_json::from_slice(&tokio::fs::read(entry.path()).await?)
                    .unwrap_or_else(|_| json!({"destination": "s3","bucket": bucket}));
                storage_for(s, &location)?
                    .purge_key(&key(run, hash))
                    .await?;
            }
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    timing.finish();
    Ok(())
}

/// Every isolated conversation uses the same continuous S3 publication path.
pub fn protected(run: &Value) -> bool {
    run["isolated"] == true
}

pub async fn attempt(s: &Service, run: &Value) {
    if !protected(run) {
        return;
    }
    let _ = update_status(s, text(run, "id"), json!({"status": "saving"})).await;
    if let Err(error) = capture(s, run).await {
        if error.status == 425 {
            let _ = update_status(
                s,
                text(run, "id"),
                json!({"status": "pending","error": null}),
            )
            .await;
            return;
        }
        // Nothing to save yet, or an older guest image whose limitation is shown in the conversation.
        if error.status != 409 && error.status != 412 {
            let _ = super::alerts::raise(
                s,
                text(run, "id"),
                "backup-failed",
                "S3 synchronization failed",
                &format!(
                    "The current disk could not be synchronized: {}",
                    error.message
                ),
            )
            .await;
        }
        // Publication may have committed before its controller acknowledgement failed.
        // Patch the current state transactionally, never overwrite it with the caller's old pointer.
        let _ = update_status(
            s,
            text(run, "id"),
            json!({"status": "error","error": error.message,"snapshotId": null}),
        )
        .await;
    }
}

async fn update_status(s: &Service, run: &str, patch: Value) -> Result<()> {
    let run = run.to_owned();
    s.store
        .transaction(move |db| {
            let current = db
                .run(&run)?
                .ok_or_else(|| Error::new(404, "Conversation missing."))?;
            let mut status = current["backup"].as_object().cloned().unwrap_or_default();
            status.extend(patch.as_object().unwrap().clone());
            db.patch_run(&run, &json!({"backup": status}))?;
            Ok(())
        })
        .await
}

async fn collection_runs(s: &Service) -> Result<HashSet<String>> {
    let mut runs = s
        .store
        .list("node-backups")
        .await?
        .iter()
        .filter_map(|point| point["runId"].as_str().map(str::to_owned))
        .collect::<HashSet<_>>();
    let root = s.config.data_dir.join("node-backups");
    if root.exists() {
        let mut entries = tokio::fs::read_dir(root).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().await?.is_dir() && crate::validation::uuid(&name).is_ok() {
                runs.insert(name);
            }
        }
    }
    Ok(runs)
}

pub async fn maintain(s: std::sync::Arc<Service>) {
    let mut last = std::collections::HashMap::<String, i64>::new();
    let mut last_cleanup = 0;
    loop {
        tokio::select! {
            _ = s.shutdown.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        if now() - last_cleanup >= 60_000 {
            last_cleanup = now();
            if let Ok(runs) = collection_runs(&s).await {
                for run in runs {
                    tokio::select! {
                        _ = s.shutdown.cancelled() => return,
                        result = collect(&s, &run) => {
                            if let Err(error) = result {
                                let _ = s
                                    .store
                                    .audit("disk.cleanup_failed", json!({"runId": run,"message": error.message}))
                                    .await;
                            }
                        }
                    }
                }
            }
        }
        let settings = settings(&s).await.unwrap_or_default();
        let interval = settings["intervalSeconds"].as_i64().unwrap_or(60) * 1000;
        if let Ok(runs) = s
            .store
            .read(|db| {
                db.json_rows(
                    "SELECT data FROM runs WHERE status='running' OR (status='succeeded'
             AND json_extract(data,'$.storage.mode')='on-demand'
             AND (json_extract(data,'$.storage.dirtyBytes')>0
                  OR json_extract(data,'$.backup.status') IN ('pending','saving','error')))",
                    [],
                )
            })
            .await
        {
            for run in runs {
                let id = text(&run, "id");
                if !protected(&run)
                    || run["isolated"] != true
                    || (!run["sessionId"].is_string() && run["storage"]["mode"] != "on-demand")
                    || run["moveRequest"].is_object()
                    || (run["storage"]["mode"] == "on-demand"
                        && run["storage"]["dirtyBytes"] == 0
                        && run["backup"]["status"] == "ready")
                    || now() - last.get(id).copied().unwrap_or(0)
                        < run["storage"]["backupSeconds"]
                            .as_i64()
                            .map(|v| v * 1000)
                            .unwrap_or(interval)
                {
                    continue;
                }
                let run_id = id.to_owned();
                if s.store
                    .read(move |db| crate::conversation_lifecycle::require_active_run(db, &run_id))
                    .await
                    .is_err()
                {
                    continue;
                }
                last.insert(id.into(), now());
                tokio::select! {_ = s.shutdown.cancelled() => return,_ = attempt(&s,&run) => {}}
            }
        }
    }
}

fn storage_for(s: &Service, backup: &Value) -> Result<crate::object_storage::Storage> {
    if backup["destination"] != "s3" {
        return Err(Error::new(503, "Local recovery block is missing."));
    }
    let mut storage = crate::object_storage::Storage::configured(s)?;
    if storage.endpoint.as_deref() != backup["endpoint"].as_str() {
        return Err(Error::new(
            409,
            "This recovery point belongs to a different S3 endpoint. Restore its \
                storage configuration before accessing it.",
        ));
    }
    storage.bucket = backup["bucket"]
        .as_str()
        .filter(|b| {
            !b.is_empty()
                && b.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
        })
        .ok_or_else(|| Error::bad("Invalid backup bucket."))?
        .into();
    Ok(storage)
}

async fn upload_verified(
    storage: &crate::object_storage::Storage,
    path: &std::path::Path,
    key: &str,
) -> Result<()> {
    storage.upload_file_verified(path, key).await
}

/// Explicit conversation purge removes every recovery dependency, including orphan uploads.
pub async fn purge(s: &Service, run: &str) -> Result<()> {
    if run.is_empty() {
        return Ok(());
    }
    crate::validation::uuid(run)?;
    let _operation = s.node_backup_operation.lock().await;
    let _readers = s.node_disk_reads.write().await;
    let points = s.store.node_backups_for_run(run).await?;
    let mut locations = points
        .iter()
        .filter(|p| p["destination"] == "s3")
        .cloned()
        .collect::<Vec<_>>();
    if root(s, run).exists() {
        let mut entries = tokio::fs::read_dir(root(s, run)).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if crate::validation::uuid(id).is_err() {
                continue;
            }
            let point: Value = serde_json::from_slice(&tokio::fs::read(entry.path()).await?)?;
            if point["id"] != id || point["runId"] != run {
                return Err(Error::bad(
                    "Invalid publication upload inventory; purge deferred.",
                ));
            }
            if point["destination"] == "s3" {
                locations.push(point);
            }
        }
    }
    let blocks = root(s, run).join("blocks");
    if blocks.exists() {
        let mut entries = tokio::fs::read_dir(&blocks).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some((_, bucket)) = name.split_once(".s3-") {
                locations.push(
                    serde_json::from_slice(&tokio::fs::read(entry.path()).await?)
                        .unwrap_or_else(|_| json!({"destination": "s3","bucket": bucket})),
                );
            }
        }
    }
    let mut deleted = HashSet::new();
    for location in locations {
        let storage = storage_for(s, &location)?;
        if deleted.insert((storage.endpoint.clone(), storage.bucket.clone())) {
            storage.purge(&format!("node-backups/{run}/")).await?;
        }
    }
    let directory = root(s, run);
    if directory.exists() {
        tokio::fs::remove_dir_all(directory).await?;
    }
    // Old installations may still hold metadata from the removed periodic audit.
    s.store.delete(&format!("node-backup-audit:{run}")).await?;
    let run = run.to_owned();
    s.store
        .transaction(move |db| {
            for point in points {
                db.remove("node-backups", text(&point, "id"))?;
            }
            for grant in db.list("node-disk-grants")? {
                if grant["runId"] == run {
                    db.remove("node-disk-grants", text(&grant, "id"))?;
                }
            }
            Ok(())
        })
        .await
}
