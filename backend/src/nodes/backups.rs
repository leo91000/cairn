//! Encrypted content-addressed recovery points. Publish only after every dependency is durable.
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
pub async fn settings(s: &Service) -> Result<Value> {
    let mut value = json!({"destination":"master","intervalSeconds":60,"retention":3,"budgetMiB":102400,"disconnectTimeoutSeconds":60,"shutdownTimeoutSeconds":300,"maxCapacityWaitSeconds":3600});
    if let Some(saved) = s.store.kv("node-backup-settings").await? {
        crate::store::merge(&mut value, &saved);
    }
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
    // A capture continuing from a baseline may rely on blocks the master no longer
    // holds: forget the baseline so the next capture copies the whole disk.
    if result.is_err() && run["backup"]["snapshotId"].is_string() {
        let _ = forget_baseline(s, text(run, "id")).await;
    }
    result
}
async fn forget_baseline(s: &Service, run_id: &str) -> Result<()> {
    let mut backup = s.store.run(run_id).await?["backup"].clone();
    if backup.is_object() {
        backup["snapshotId"] = Value::Null;
        s.store.patch_run(run_id, json!({"backup":backup})).await?;
    }
    Ok(())
}
async fn publish(s: &Service, run: &Value) -> Result<Value> {
    let _operation = s.node_backup_operation.lock().await;
    let run_id = text(run, "id");
    crate::validation::uuid(run_id)?;
    let checkpoint = s
        .store
        .kv(&format!("run-checkpoint:{run_id}"))
        .await?
        .unwrap_or_default();
    let attempt = text(&checkpoint, "runnerId");
    crate::validation::uuid(attempt)?;
    if !run["sessionId"].is_string()
        && run["storage"]["mode"] != "on-demand"
        && run["storageRequested"] != true
    {
        return Err(Error::new(
            409,
            "No resumable provider session has been recorded yet.",
        ));
    }
    let settings = settings(s).await?;
    let destination = if run["storage"]["mode"] == "on-demand" || run["storageRequested"] == true {
        "s3"
    } else {
        text(&settings, "destination")
    };
    let storage = if destination == "s3" {
        Some(crate::archive_storage::Storage::configured(s)?)
    } else {
        None
    };
    retain(
        s,
        run_id,
        settings["retention"].as_u64().unwrap_or(3) as usize,
    )
    .await?;
    let base = super::transport::url(s, run_id).await?;
    let credential = crate::execution::secret(&s.config.data_dir, "runner-secret").await?;
    let capture_path = if run["status"] != "running" || run["moveRequest"]["idle"] == true {
        format!(
            "{base}/disks/{run_id}/{}",
            if run["storageMigrationCapture"] == true {
                "migration-snapshot"
            } else {
                "snapshot"
            }
        )
    } else {
        format!("{base}/runs/{attempt}/snapshot")
    };
    let response = s
        .http
        .post(capture_path)
        .bearer_auth(&credential)
        // The node copies only blocks written since this published point if it still tracks it.
        .json(&json!({"baseline":run["backup"]["snapshotId"]}))
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|_| Error::new(503, "Snapshot capture interrupted."))?;
    // A missing guest capability is actionable; never expose arbitrary controller bodies.
    if response.status() == reqwest::StatusCode::PRECONDITION_FAILED {
        return Err(Error::new(
            412,
            super::checkpoint::ACTIVE_CAPTURE_UNSUPPORTED,
        ));
    }
    if !response.status().is_success() {
        return Err(Error::new(503, "Unable to capture a coherent VM snapshot."));
    }
    let snapshot: Value = response.json().await.map_err(Error::internal)?;
    let snapshot_id = text(&snapshot, "id");
    crate::validation::uuid(snapshot_id)?;
    let result=async {
        let manifest=&snapshot["manifest"];snapshots::validate(manifest)?;
        let directory=root(s,run_id);crate::skills::private_dir(&directory.join("blocks")).await?;
        let published=published_blocks(s,run_id).await?;
        let mut seen=HashSet::new();let mut uploaded=0u64;let mut occupied=used(s).await?;
        let budget=settings["budgetMiB"].as_u64().unwrap_or(102400)*1024*1024;
        let memory_only=storage.is_some() && local_demand(s).await?;
        let mut uploads = tokio::task::JoinSet::<Result<()>>::new();
        for block in manifest["blocks"].as_array().unwrap() {
            let Some(hash)=block["hash"].as_str() else {continue};
            if !seen.insert(hash.to_owned()) {continue;}
            let file=directory.join("blocks").join(hash);
            let remote_mark=storage.as_ref().map(|storage|receipt(&file,&json!({"destination":"s3","bucket":storage.bucket,"endpoint":storage.endpoint}))).transpose()?;
            if published.get(hash).copied()==block["size"].as_u64() && remote_mark.as_ref().is_some_and(|mark|mark.exists()) {
                // A verified immutable S3 copy remains usable after cache eviction.
                continue;
            }
            let mut verified=false;
            if !file.exists() {
                let response=s.http.get(format!("{base}/snapshots/{snapshot_id}/{hash}")).bearer_auth(&credential).timeout(Duration::from_secs(120)).send().await.map_err(|_|Error::new(503,"Backup block transfer interrupted."))?;
                if !response.status().is_success() {return Err(Error::new(503,"Backup block unavailable."));}
                let bytes=super::snapshots::response_block(response).await?;
                if bytes.len() as u64!=block["size"].as_u64().unwrap() || hex::encode(Sha256::digest(&bytes))!=hash {return Err(Error::bad("Backup block failed integrity verification."));}
                let vault=s.vault.clone(); let scope=key(run_id,hash); let length=bytes.len() as u64;
                let encoded=tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                    let mut encoded = BINARY_BLOCK_HEADER.to_vec();
                    encoded.extend(vault.encrypt_bytes(&scope, &bytes)?);
                    Ok(encoded)
                }).await.map_err(Error::internal)??;
                if memory_only {
                    enqueue_verified_block(&mut uploads, storage.as_ref().unwrap(), &file, key(run_id,hash), encoded).await?;
                    uploaded+=length;
                    continue;
                }
                // Serialize space accounting and publication across concurrent backups.
                let _guard=s.node_backup_lock.lock().await;
                occupied=make_room(s,occupied,encoded.len() as u64,budget).await?;
                crate::skills::atomic_write(&file,&encoded).await?;
                occupied+=encoded.len() as u64;
                uploaded+=length;
                verified=true;
            } else if published.get(hash).copied()!=block["size"].as_u64() {
                // Orphan files from interrupted publication have no retained proof.
                // Published blocks are immutable and already validated at receipt.
                let bytes=decode_block(s,run_id,hash,tokio::fs::read(&file).await?).await?;
                if bytes.len() as u64!=block["size"].as_u64().unwrap() {return Err(Error::bad("Cached backup block has the wrong size."));}
                verified=true;
            }
            if let Some(storage)=&storage {
                let location=json!({"destination":"s3","bucket":storage.bucket,"endpoint":storage.endpoint});
                let mark=receipt(&file,&location)?;
                if !mark.exists() {
                    // Verify before copying an existing block to a new destination.
                    let encoded=tokio::fs::read(&file).await?;
                    if !verified {decode_block(s,run_id,hash,encoded.clone()).await?;}
                    enqueue_verified_block(&mut uploads, storage, &file, key(run_id,hash), encoded).await?;
                }

            }
        }
        while let Some(upload) = uploads.join_next().await {
            upload.map_err(Error::internal)??;
        }
        let backup_id=id();
        let value=json!({"id":backup_id,"snapshotId":snapshot_id,"runId":run_id,"nodeId":checkpoint["nodeId"],"createdAt":now(),"capturedAt":manifest["capturedAt"],"diskGeneration":manifest["generation"],"sessionId":run["sessionId"],"destination":destination,"bucket":storage.as_ref().map(|s|s.bucket.clone()),"endpoint":storage.as_ref().and_then(|s|s.endpoint.clone()),"uploadedBytes":uploaded,"pauseMs":manifest["pauseMs"],"indexMs":manifest["indexMs"],"localBytesRead":manifest["localBytesRead"],"incremental":manifest["incremental"],"manifest":s.vault.encrypt(&format!("backup:{backup_id}"),manifest)?});
        let path=directory.join(format!("{backup_id}.json"));
        let encoded=serde_json::to_vec(&value)?;
        if occupied.saturating_add(encoded.len() as u64)>budget {return Err(Error::new(507,"Backup storage budget exhausted; previous recovery points are retained."));}
        crate::skills::atomic_write(&path,&encoded).await?;
        if let Some(storage)=&storage {upload_verified(storage,&path,&format!("node-backups/{run_id}/{backup_id}.json")).await?;}
        let patch=json!({"backup":{"id":backup_id,"snapshotId":snapshot_id,"capturedAt":manifest["capturedAt"],"uploadedBytes":uploaded,"status":"ready","error":null}});
        let (owner_run,owner_attempt,owner_node,point)=(run_id.to_owned(),attempt.to_owned(),checkpoint["nodeId"].clone(),value.clone());
        s.store.transaction(move |db| {
            let current=db.kv(&format!("run-checkpoint:{owner_run}"))?.unwrap_or_default();
            if current["runnerId"]!=owner_attempt || current["nodeId"]!=owner_node {return Err(Error::new(409,"Disk owner changed during publication."));}
            db.put("node-backups",&point)?;db.patch_run(&owner_run,&patch)?;Ok(())
        }).await?;
        if manifest["onDemand"]==true {
            super::disk_grants::extend(s,text(&snapshot,"grantId"),&value).await?;
            let response=s.http.post(format!("{base}/disks/{run_id}/published")).bearer_auth(&credential).json(&json!({"generation":manifest["generation"],"backupId":backup_id,"grantId":snapshot["grantId"]})).timeout(Duration::from_secs(120)).send().await.map_err(|_|Error::new(503,"Disk publication acknowledgement interrupted."))?;
            if !response.status().is_success(){return Err(Error::new(503,"Node could not acknowledge the published disk."));}
            super::disk_grants::acknowledged(s,text(&snapshot,"grantId"),&value).await?;
        }
        retain(s,run_id,settings["retention"].as_u64().unwrap_or(3) as usize).await?;
        Ok(public(value))
    }.await;
    let _ = s
        .http
        .delete(format!("{base}/snapshots/{snapshot_id}/discard"))
        .bearer_auth(credential)
        .timeout(Duration::from_secs(20))
        .send()
        .await;
    result
}

/// Every upload path publishes its receipt only after remote read-back verification.
async fn enqueue_verified_block(
    uploads: &mut tokio::task::JoinSet<Result<()>>,
    storage: &crate::archive_storage::Storage,
    file: &std::path::Path,
    key: String,
    encoded: Vec<u8>,
) -> Result<()> {
    let location = json!({"destination":"s3","bucket":storage.bucket,"endpoint":storage.endpoint});
    let mark = receipt(file, &location)?;
    let receipt_bytes = serde_json::to_vec(&location)?;
    let storage = storage.clone();
    uploads.spawn(async move {
        storage.upload_bytes(encoded, &key).await?;
        crate::skills::atomic_write(&mark, &receipt_bytes).await
    });
    if uploads.len() >= crate::archive_storage::HOT_WRITE_CONCURRENCY {
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
async fn published_blocks(s: &Service, run: &str) -> Result<HashMap<String, u64>> {
    let mut blocks = HashMap::new();
    for point in s.store.node_backups_for_run(run).await? {
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
    let location =
        json!({"destination":"s3","bucket":location["bucket"],"endpoint":location["endpoint"]});
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
    let (encoded, bytes) = match recovered {
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
    // Enabled local nodes keep their only clean disk cache in the controller.
    if local_demand(s).await? {
        return Ok(bytes);
    }
    // Never evict a payload concurrently with publication to another destination.
    let Ok(_operation) = s.node_backup_operation.try_lock() else {
        return Ok(bytes);
    };
    // Restoration remains possible when the master cache budget is full.
    let _guard = s.node_backup_lock.lock().await;
    let budget = settings(s).await?["budgetMiB"].as_u64().unwrap_or(102400) * 1024 * 1024;
    if make_room(s, used(s).await?, encoded.len() as u64, budget)
        .await
        .is_ok()
    {
        crate::skills::atomic_write(&path, &encoded).await?;
    }
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
            // Existing recovery points remain readable without a bulk migration.
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

async fn local_demand(s: &Service) -> Result<bool> {
    Ok(s.store
        .get("nodes", super::LOCAL_NODE_ID)
        .await?
        .is_some_and(|node| node["storage"]["enabled"] == true))
}
/// The local controller owns the configured node cache. Retire duplicate S3
/// payloads on the master, while preserving authoritative master-only points.
pub async fn maintain_local_cache(s: &Service) -> Result<()> {
    if !local_demand(s).await? {
        return Ok(());
    }
    let Ok(_operation) = s.node_backup_operation.try_lock() else {
        return Ok(());
    };
    let _guard = s.node_backup_lock.lock().await;
    let budget = settings(s).await?["budgetMiB"].as_u64().unwrap_or(102400) * 1048576;
    make_room(s, used(s).await?, 0, budget).await?;
    Ok(())
}
async fn make_room(s: &Service, mut occupied: u64, incoming: u64, budget: u64) -> Result<u64> {
    // Keep this admission policy separate from storage::cache: candidates require
    // remote verification and master-only points are protected. Enabled local
    // nodes retire every duplicate; publication may use the emergency reserve
    // below the normal floor so a dirty journal can make progress.
    let policy = s
        .store
        .get("nodes", super::LOCAL_NODE_ID)
        .await?
        .and_then(|v| {
            serde_json::from_value::<crate::storage::policy::Policy>(v["storage"].clone()).ok()
        })
        .unwrap_or_default();
    let reserve = if policy.enabled {
        let (total, _) = crate::storage::policy::space(&s.config.data_dir)?;
        policy.reserve(total)
    } else {
        0
    };
    let required = occupied
        .saturating_add(incoming)
        .saturating_sub(budget)
        .max(
            reserve
                .saturating_add(incoming.saturating_mul(3))
                .saturating_sub(crate::storage::policy::space(&s.config.data_dir)?.1),
        );
    if required == 0 && !policy.enabled {
        return Ok(occupied);
    }
    let mut protected = HashSet::new();
    for point in s
        .store
        .list("node-backups")
        .await?
        .into_iter()
        .filter(|p| p["destination"] != "s3")
    {
        let manifest = manifest(s, &point).await?;
        for block in manifest["blocks"].as_array().unwrap() {
            if let Some(hash) = block["hash"].as_str() {
                protected.insert((text(&point, "runId").to_owned(), hash.to_owned()));
            }
        }
    }
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
            let run_id = run.file_name().to_string_lossy().into_owned();
            let mut entries = tokio::fs::read_dir(&blocks).await?;
            while let Some(entry) = entries.next_entry().await? {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some((hash, _)) = name.split_once(".s3-") else {
                    continue;
                };
                if !snapshots::valid_hash(hash)
                    || protected.contains(&(run_id.clone(), hash.to_owned()))
                {
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
        if removed >= required && !policy.enabled {
            break;
        }
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
async fn retain(s: &Service, run: &str, count: usize) -> Result<()> {
    let _readers = s.node_disk_reads.write().await;
    let pinned = super::disk_grants::pinned(s, run).await?;
    let _guard = s.node_backup_lock.lock().await;
    let mut points = s.store.node_backups_for_run(run).await?;
    points.sort_by_key(|p| std::cmp::Reverse(p["createdAt"].as_i64().unwrap_or(0)));
    let mut needed = HashSet::new();
    let keep = points
        .iter()
        .take(count.max(1))
        .map(|p| text(p, "id").to_owned())
        .chain(pinned)
        .collect::<HashSet<_>>();
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
    for point in points.iter().filter(|p| !keep.contains(text(p, "id"))) {
        let point_id = text(point, "id").to_owned();
        if point["destination"] == "s3" {
            storage_for(s, point)?
                .purge(&format!("node-backups/{run}/{}.json", text(point, "id")))
                .await?;
        }
        s.store
            .write(move |db| db.remove("node-backups", &point_id))
            .await?;
        let _ =
            tokio::fs::remove_file(root(s, run).join(format!("{}.json", text(point, "id")))).await;
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
                    .unwrap_or_else(|_| json!({"destination":"s3","bucket":bucket}));
                storage_for(s, &location)?.purge(&key(run, hash)).await?;
            }
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    Ok(())
}

/// Continuous recovery points guard against losing a remote node. The master runner fails
/// together with the master, so its conversations are captured only when they move.
pub fn protected(run: &Value) -> bool {
    run["storage"]["mode"] == "on-demand"
        || run["nodeId"]
            .as_str()
            .is_some_and(|node| node != super::LOCAL_NODE_ID)
}
pub async fn attempt(s: &Service, run: &Value) {
    if !protected(run) {
        return;
    }
    let mut status = run["backup"]
        .as_object()
        .cloned()
        .map(Value::Object)
        .unwrap_or_else(|| json!({}));
    status["status"] = "saving".into();
    let _ = s
        .store
        .patch_run(text(run, "id"), json!({"backup":status}))
        .await;
    if let Err(error) = capture(s, run).await {
        // Nothing to save yet, or an older guest image whose limitation is shown in the conversation.
        if error.status != 409 && error.status != 412 {
            let _ = super::alerts::raise(
                s,
                text(run, "id"),
                "backup-failed",
                "Recovery point failed",
                &format!("A new recovery point could not be saved: {}", error.message),
            )
            .await;
        }
        status["status"] = "error".into();
        status["error"] = error.message.into();
        status["snapshotId"] = Value::Null;
        let _ = s
            .store
            .patch_run(text(run, "id"), json!({"backup":status}))
            .await;
    }
}
pub async fn maintain(s: std::sync::Arc<Service>) {
    let mut last = std::collections::HashMap::<String, i64>::new();
    loop {
        tokio::select! {_=s.shutdown.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(5))=>{}}
        let settings = settings(&s).await.unwrap_or_default();
        let interval = settings["intervalSeconds"].as_i64().unwrap_or(60) * 1000;
        if let Ok(runs) = s.store.read(|db| db.active()).await {
            for run in runs {
                let id = text(&run, "id");
                if run["status"] != "running"
                    || !protected(&run)
                    || run["isolated"] != true
                    || (!run["sessionId"].is_string() && run["storage"]["mode"] != "on-demand")
                    || run["moveRequest"].is_object()
                    || (run["storage"]["mode"] == "on-demand" && run["storage"]["dirtyBytes"] == 0)
                    || now() - last.get(id).copied().unwrap_or(0)
                        < run["storage"]["backupSeconds"]
                            .as_i64()
                            .map(|v| v * 1000)
                            .unwrap_or(interval)
                {
                    continue;
                }
                last.insert(id.into(), now());
                tokio::select! {_=s.shutdown.cancelled()=>return,_=attempt(&s,&run)=>{}}
            }
        }
    }
}

fn storage_for(s: &Service, backup: &Value) -> Result<crate::archive_storage::Storage> {
    if backup["destination"] != "s3" {
        return Err(Error::new(503, "Local recovery block is missing."));
    }
    let mut storage = crate::archive_storage::Storage::configured(s)?;
    if storage.endpoint.as_deref() != backup["endpoint"].as_str() {
        return Err(Error::new(
            409,
            "This recovery point belongs to a different S3 endpoint. Restore its storage configuration before accessing it.",
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
    storage: &crate::archive_storage::Storage,
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
    let blocks = root(s, run).join("blocks");
    if blocks.exists() {
        let mut entries = tokio::fs::read_dir(&blocks).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some((_, bucket)) = name.split_once(".s3-") {
                locations.push(
                    serde_json::from_slice(&tokio::fs::read(entry.path()).await?)
                        .unwrap_or_else(|_| json!({"destination":"s3","bucket":bucket})),
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
