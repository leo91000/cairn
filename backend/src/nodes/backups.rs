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
    let response = s
        .http
        .post(format!("{base}/runs/{attempt}/snapshot"))
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
                let encrypted=s.vault.encrypt(&key(run_id,hash),&json!(STANDARD.encode(&bytes)))?;
                let encoded=serde_json::to_vec(&encrypted)?;
                // Serialize space accounting and publication across concurrent backups.
                let _guard=s.node_backup_lock.lock().await;
                if occupied.saturating_add(encoded.len() as u64)>budget {return Err(Error::new(507,"Backup storage budget exhausted; previous recovery points are retained."));}
                crate::skills::atomic_write(&file,&encoded).await?;
                occupied+=encoded.len() as u64;
                uploaded+=bytes.len() as u64;
            }
            let encoded:Value=serde_json::from_slice(&tokio::fs::read(&file).await?)?;
            let plaintext=s.vault.decrypt(&key(run_id,hash),&encoded)?;
            let verified=STANDARD.decode(plaintext.as_str().unwrap_or("")).map_err(|_|Error::bad("Backup ciphertext is invalid."))?;
            if verified.len() as u64!=block["size"].as_u64().unwrap() || hex::encode(Sha256::digest(&verified))!=hash {return Err(Error::bad("Cached backup block failed integrity verification."));}
            if let Some(storage)=&storage {
                let mark=file.with_extension(format!("s3-{}",storage.bucket));
                if !mark.exists() {upload_verified(storage,&file,&key(run_id,hash)).await?;crate::skills::atomic_write(&mark,b"uploaded").await?;}
            }
        }
        let backup_id=id();
        let value=json!({"id":backup_id,"runId":run_id,"nodeId":checkpoint["nodeId"],"createdAt":now(),"capturedAt":manifest["capturedAt"],"sessionId":run["sessionId"],"destination":destination,"bucket":storage.as_ref().map(|s|s.bucket.clone()),"uploadedBytes":uploaded,"pauseMs":manifest["pauseMs"],"indexMs":manifest["indexMs"],"localBytesRead":manifest["localBytesRead"],"manifest":s.vault.encrypt(&format!("backup:{backup_id}"),manifest)?});
        let path=directory.join(format!("{backup_id}.json"));
        let encoded=serde_json::to_vec(&value)?;
        if occupied.saturating_add(encoded.len() as u64)>budget {return Err(Error::new(507,"Backup storage budget exhausted; previous recovery points are retained."));}
        crate::skills::atomic_write(&path,&encoded).await?;
        if let Some(storage)=&storage {upload_verified(storage,&path,&format!("node-backups/{run_id}/{backup_id}.json")).await?;}
        s.store.put("node-backups",value.clone()).await?;
        s.store.patch_run(run_id,json!({"backup":{"id":backup_id,"capturedAt":manifest["capturedAt"],"uploadedBytes":uploaded,"status":"ready"}})).await?;
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
    if !path.exists() {
        let storage = storage_for(s, backup)?;
        crate::skills::private_dir(path.parent().unwrap()).await?;
        let temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        storage.download(&key(run, hash), temp.path()).await?;
        temp.persist_noclobber(&path).map_err(Error::internal)?;
    }
    let encoded: Value = serde_json::from_slice(&tokio::fs::read(path).await?)?;
    let plaintext = s.vault.decrypt(&key(run, hash), &encoded)?;
    let bytes = STANDARD
        .decode(plaintext.as_str().unwrap_or(""))
        .map_err(|_| Error::bad("Invalid backup ciphertext."))?;
    if hex::encode(Sha256::digest(&bytes)) != hash {
        return Err(Error::bad("Backup integrity check failed."));
    }
    Ok(bytes)
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
        for block in manifest(s, point).await?["blocks"].as_array().unwrap() {
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
                let mut storage = crate::archive_storage::Storage::configured(s)?;
                storage.bucket = bucket.to_owned();
                storage.purge(&key(run, hash)).await?;
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
