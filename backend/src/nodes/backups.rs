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
use std::{collections::HashSet, path::PathBuf, time::Duration};
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
    if !run["sessionId"].is_string() {
        return Err(Error::new(
            409,
            "No resumable provider session has been recorded yet.",
        ));
    }
    let settings = settings(s).await?;
    let destination = text(&settings, "destination");
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
    let capture_path = if run["status"] == "succeeded" || run["moveRequest"]["idle"] == true {
        format!("{base}/disks/{run_id}/snapshot")
    } else {
        format!("{base}/runs/{attempt}/snapshot")
    };
    let response = s
        .http
        .post(capture_path)
        .bearer_auth(&credential)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|_| Error::new(503, "Snapshot capture interrupted."))?;
    if !response.status().is_success() {
        return Err(Error::new(503, "Unable to capture a coherent VM snapshot."));
    }
    let snapshot: Value = response.json().await.map_err(Error::internal)?;
    let snapshot_id = text(&snapshot, "id");
    crate::validation::uuid(snapshot_id)?;
    let result=async {
        let manifest=&snapshot["manifest"];snapshots::validate(manifest)?;
        let directory=root(s,run_id);crate::skills::private_dir(&directory.join("blocks")).await?;
        let mut seen=HashSet::new();let mut uploaded=0u64;let mut occupied=used(s).await?;
        let budget=settings["budgetMiB"].as_u64().unwrap_or(102400)*1024*1024;
        for block in manifest["blocks"].as_array().unwrap() {
            let Some(hash)=block["hash"].as_str() else {continue};
            if !seen.insert(hash.to_owned()) {continue;}
            let file=directory.join("blocks").join(hash);
            if !file.exists() {
                let response=s.http.get(format!("{base}/snapshots/{snapshot_id}/{hash}")).bearer_auth(&credential).timeout(Duration::from_secs(120)).send().await.map_err(|_|Error::new(503,"Backup block transfer interrupted."))?;
                if !response.status().is_success() {return Err(Error::new(503,"Backup block unavailable."));}
                let bytes=super::snapshots::response_block(response).await?;
                if bytes.len() as u64!=block["size"].as_u64().unwrap() || hex::encode(Sha256::digest(&bytes))!=hash {return Err(Error::bad("Backup block failed integrity verification."));}
                let vault=s.vault.clone(); let scope=key(run_id,hash); let length=bytes.len() as u64;
                let encoded=tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                    serde_json::to_vec(&vault.encrypt(&scope,&json!(STANDARD.encode(&bytes)))?).map_err(Into::into)
                }).await.map_err(Error::internal)??;
                // Serialize space accounting and publication across concurrent backups.
                let _guard=s.node_backup_lock.lock().await;
                if occupied.saturating_add(encoded.len() as u64)>budget {return Err(Error::new(507,"Backup storage budget exhausted; previous recovery points are retained."));}
                crate::skills::atomic_write(&file,&encoded).await?;
                occupied+=encoded.len() as u64;
                uploaded+=length;
            }
            let verified=decode_block(s,run_id,hash,tokio::fs::read(&file).await?).await?;
            if verified.len() as u64!=block["size"].as_u64().unwrap() || hex::encode(Sha256::digest(&verified))!=hash {return Err(Error::bad("Cached backup block failed integrity verification."));}
            if let Some(storage)=&storage {
                let location=json!({"destination":"s3","bucket":storage.bucket,"endpoint":storage.endpoint});
                let mark=file.with_extension(format!("s3-{}",hex::encode(Sha256::digest(serde_json::to_vec(&location)?))));
                if !mark.exists() {upload_verified(storage,&file,&key(run_id,hash)).await?;crate::skills::atomic_write(&mark,&serde_json::to_vec(&location)?).await?;}
            }
        }
        let backup_id=id();
        let value=json!({"id":backup_id,"runId":run_id,"nodeId":checkpoint["nodeId"],"createdAt":now(),"capturedAt":manifest["capturedAt"],"sessionId":run["sessionId"],"destination":destination,"bucket":storage.as_ref().map(|s|s.bucket.clone()),"endpoint":storage.as_ref().and_then(|s|s.endpoint.clone()),"uploadedBytes":uploaded,"pauseMs":manifest["pauseMs"],"indexMs":manifest["indexMs"],"localBytesRead":manifest["localBytesRead"],"manifest":s.vault.encrypt(&format!("backup:{backup_id}"),manifest)?});
        let path=directory.join(format!("{backup_id}.json"));
        let encoded=serde_json::to_vec(&value)?;
        if occupied.saturating_add(encoded.len() as u64)>budget {return Err(Error::new(507,"Backup storage budget exhausted; previous recovery points are retained."));}
        crate::skills::atomic_write(&path,&encoded).await?;
        if let Some(storage)=&storage {upload_verified(storage,&path,&format!("node-backups/{run_id}/{backup_id}.json")).await?;}
        s.store.put("node-backups",value.clone()).await?;
        s.store.patch_run(run_id,json!({"backup":{"id":backup_id,"capturedAt":manifest["capturedAt"],"uploadedBytes":uploaded,"status":"ready","error":null}})).await?;
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
    if path.exists() {
        match decode_block(s, run, hash, tokio::fs::read(&path).await?).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) if backup["destination"] != "s3" => return Err(error),
            Err(_) => {}
        }
    }
    let storage = storage_for(s, backup)?;
    crate::skills::private_dir(path.parent().unwrap()).await?;
    let temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    storage.download(&key(run, hash), temp.path()).await?;
    let encoded = tokio::fs::read(temp.path()).await?;
    let bytes = decode_block(s, run, hash, encoded.clone()).await?;
    // Restoration remains possible when the master cache budget is full.
    let _guard = s.node_backup_lock.lock().await;
    let budget = settings(s).await?["budgetMiB"].as_u64().unwrap_or(102400) * 1024 * 1024;
    if used(s).await?.saturating_add(encoded.len() as u64) <= budget {
        crate::skills::atomic_write(&path, &encoded).await?;
    }
    Ok(bytes)
}

async fn decode_block(s: &Service, run: &str, hash: &str, encoded: Vec<u8>) -> Result<Vec<u8>> {
    let vault = s.vault.clone();
    let scope = key(run, hash);
    let hash = hash.to_owned();
    tokio::task::spawn_blocking(move || {
        let value: Value = serde_json::from_slice(&encoded)?;
        let plaintext = vault.decrypt(&scope, &value)?;
        let bytes = STANDARD
            .decode(plaintext.as_str().unwrap_or(""))
            .map_err(|_| Error::bad("Invalid backup ciphertext."))?;
        if hex::encode(Sha256::digest(&bytes)) != hash {
            return Err(Error::bad("Backup integrity check failed."));
        }
        Ok(bytes)
    })
    .await
    .map_err(Error::internal)?
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
    let _guard = s.node_backup_lock.lock().await;
    let mut points = s
        .store
        .list("node-backups")
        .await?
        .into_iter()
        .filter(|p| p["runId"] == run)
        .collect::<Vec<_>>();
    points.sort_by_key(|p| std::cmp::Reverse(p["createdAt"].as_i64().unwrap_or(0)));
    let mut needed = HashSet::new();
    for point in points.iter().take(count.max(1)) {
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
    for point in points.iter().skip(count.max(1)) {
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

pub async fn attempt(s: &Service, run: &Value) {
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
        status["status"] = "error".into();
        status["error"] = error.message.into();
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
                    || run["isolated"] != true
                    || !run["sessionId"].is_string()
                    || run["moveRequest"].is_object()
                    || now() - last.get(id).copied().unwrap_or(0) < interval
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
    storage.upload(path, key).await?;
    let verification = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or_else(|| Error::bad("Invalid backup path."))?,
    )?;
    storage.download(key, verification.path()).await?;
    if crate::archive_storage::hash(path.to_owned()).await?
        != crate::archive_storage::hash(verification.path().to_owned()).await?
    {
        return Err(Error::bad(
            "Remote backup checksum mismatch; the previous point remains available.",
        ));
    }
    Ok(())
}

/// Explicit conversation purge removes every recovery dependency, including orphan uploads.
pub async fn purge(s: &Service, run: &str) -> Result<()> {
    if run.is_empty() {
        return Ok(());
    }
    crate::validation::uuid(run)?;
    let _operation = s.node_backup_operation.lock().await;
    let points = s
        .store
        .list("node-backups")
        .await?
        .into_iter()
        .filter(|p| p["runId"] == run)
        .collect::<Vec<_>>();
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
    s.store
        .transaction(move |db| {
            for point in points {
                db.remove("node-backups", text(&point, "id"))?;
            }
            Ok(())
        })
        .await
}
