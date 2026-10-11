//! One blocking, restartable upgrade. Normal recovery accepts only v2 envelopes.
//! Object identities and authenticated manifests remain unchanged; their new key
//! namespace prevents overwriting an object that a prior reader might have cached.
use super::{
    block_codec::{MAX_ENCODED_BYTES, MAX_LEGACY_BYTES},
    digest,
};
use crate::{
    error::{Error, Result},
    nodes::{publication, shared_blocks},
    object_storage::Storage,
    service::Service,
    validation::text,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Instant;
use tokio::io::AsyncReadExt;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS disk_block_migration(
    id TEXT PRIMARY KEY,plan TEXT NOT NULL,state INTEGER NOT NULL DEFAULT 0 CHECK(state BETWEEN 0 AND 2),
    old_bytes INTEGER NOT NULL DEFAULT 0,new_bytes INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS disk_block_migration_pending ON disk_block_migration(id) WHERE state<2;
";

const COMPLETE: &str = "migration:disk-blocks-v2";
const PLANNED: &str = "migration:disk-blocks-v2-planned";

#[derive(Clone, Serialize, Deserialize)]
struct Plan {
    source: String,
    target: String,
    hash: String,
    size: Option<u64>,
    location: Option<Value>,
    local_run: Option<String>,
    discard: bool,
}

impl Plan {
    fn id(&self) -> String {
        let destination = self
            .location
            .as_ref()
            .map(Value::to_string)
            .unwrap_or_default();
        hex::encode(Sha256::digest(format!(
            "{destination}\n{}\n{}",
            self.source,
            self.local_run.as_deref().unwrap_or("")
        )))
    }
}

/// Called by Service::new before any router, relay or publication worker exists.
/// Failures leave the service offline and the durable plan ready for the next start.
pub(crate) async fn run(s: &Service) -> Result<()> {
    let _lock = crate::file_lock::exclusive(
        &s.config.data_dir.join("disk-block-migration.lock"),
        "Another manager is migrating storage. Stop it before starting this manager.",
    )?;
    if s.store.kv(COMPLETE).await?.is_some() {
        return Ok(());
    }
    let started = Instant::now();
    tracing::info!("Migrating disk storage before accepting connections");
    if s.store.kv(PLANNED).await?.is_none() {
        plan(s).await?;
        s.store.set(PLANNED, json!(true), None).await?;
    }
    let mut completed_this_start = 0_u64;
    loop {
        let next = s.store.read(|db| {
            use rusqlite::OptionalExtension;
            db.0.query_row(
                "SELECT id,plan,state FROM disk_block_migration WHERE state<2 ORDER BY id LIMIT 1",
                [], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)),
            ).optional().map_err(Into::into)
        }).await?;
        let Some((id, encoded, state)) = next else {
            break;
        };
        let plan: Plan = serde_json::from_str(&encoded)?;
        migrate(s, &id, &plan, state).await?;
        completed_this_start += 1;
        if completed_this_start.is_multiple_of(32) {
            tracing::info!(
                completed_this_start,
                elapsed_seconds = started.elapsed().as_secs(),
                "Disk storage migration in progress; service remains offline"
            );
        }
    }
    let summary = s.store.transaction(move |db| {
        let (objects, old_bytes, new_bytes): (i64, i64, i64) = db.0.query_row(
            "SELECT COUNT(*),COALESCE(SUM(old_bytes),0),COALESCE(SUM(new_bytes),0) FROM disk_block_migration",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let (old_s3_bytes, new_s3_bytes): (i64, i64) = db.0.query_row(
            "SELECT COALESCE(SUM(old_bytes),0),COALESCE(SUM(new_bytes),0) FROM disk_block_migration WHERE json_extract(plan,'$.local_run') IS NULL AND json_extract(plan,'$.discard')=0",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let summary = json!({
            "complete": true,
            "objects": objects,
            "oldEncodedBytes": old_bytes,
            "newEncodedBytes": new_bytes,
            "oldS3EncodedBytes": old_s3_bytes,
            "newS3EncodedBytes": new_s3_bytes,
            "lastAttemptMs": started.elapsed().as_millis() as u64
        });
        db.set(COMPLETE, &summary, None)?;
        db.delete(PLANNED)?;
        db.0.execute("DELETE FROM disk_block_migration", [])?;
        Ok(summary)
    }).await?;
    tracing::info!(
        objects = summary["objects"].as_u64(),
        old_bytes = summary["oldEncodedBytes"].as_u64(),
        new_bytes = summary["newEncodedBytes"].as_u64(),
        elapsed_ms = summary["lastAttemptMs"].as_u64(),
        "Disk storage migration complete; normal reads accept only v2"
    );
    Ok(())
}

async fn insert(s: &Service, plans: Vec<Plan>) -> Result<()> {
    s.store
        .transaction(move |db| {
            let mut insert = db.0.prepare_cached(
                "INSERT OR IGNORE INTO disk_block_migration(id,plan) VALUES(?1,?2)",
            )?;
            for plan in plans {
                let id = plan.id();
                insert.execute(params![id, serde_json::to_string(&plan)?])?;
                let existing: String = db.0.query_row(
                    "SELECT plan FROM disk_block_migration WHERE id=?1",
                    [&id],
                    |row| row.get(0),
                )?;
                let existing: Plan = serde_json::from_str(&existing)?;
                if existing.target != plan.target
                    || (existing.size.is_some()
                        && plan.size.is_some()
                        && existing.size != plan.size)
                {
                    return Err(Error::bad("Conflicting storage migration references."));
                }
            }
            Ok(())
        })
        .await
}

fn remote(
    source: String,
    target: String,
    hash: String,
    size: Option<u64>,
    location: Value,
) -> Plan {
    Plan {
        source,
        target,
        hash,
        size,
        location: Some(location),
        local_run: None,
        discard: false,
    }
}

async fn plan(s: &Service) -> Result<()> {
    // Published records include current points, trash and every disk grant's retained
    // dependencies. Authenticate their manifests before using any locator or hash.
    let mut after_point = String::new();
    loop {
        let cursor = after_point.clone();
        let point = s.store.read(move |db| db.json_rows(
            "SELECT data FROM records WHERE kind='node-backups' AND id>?1 ORDER BY id LIMIT 1", [cursor],
        ).map(|mut rows| rows.pop())).await?;
        let Some(point) = point else { break };
        crate::validation::uuid(text(&point, "id"))?;
        after_point = text(&point, "id").into();
        let manifest = publication::manifest(s, &point).await?;
        let mut plans = Vec::new();
        for block in manifest["blocks"].as_array().unwrap() {
            let Some(hash) = block["hash"].as_str() else {
                continue;
            };
            if !publication::in_s3(&point) {
                let run = text(&point, "runId");
                crate::validation::uuid(run)?;
                if !point["blockFormat"].is_null() {
                    return Err(Error::bad(
                        "Shared storage migration requires its original S3 location.",
                    ));
                }
                plans.push(Plan {
                    source: format!("node-backups/{run}/blocks/{hash}"),
                    target: publication::key(run, hash),
                    hash: hash.into(),
                    size: block["size"].as_u64(),
                    location: None,
                    local_run: Some(run.into()),
                    discard: false,
                });
                if plans.len() == 256 {
                    insert(s, std::mem::take(&mut plans)).await?;
                }
                continue;
            }
            let (source, target) = if point["blockFormat"] == "shared-v1" {
                let object = text(block, "object");
                (
                    format!("shared-blocks/v1/{hash}/{object}"),
                    shared_blocks::key(hash, object),
                )
            } else {
                let run = text(&point, "runId");
                crate::validation::uuid(run)?;
                (
                    format!("node-backups/{run}/blocks/{hash}"),
                    publication::key(run, hash),
                )
            };
            plans.push(remote(
                source,
                target,
                hash.into(),
                block["size"].as_u64(),
                location(&point),
            ));
            if plans.len() == 256 {
                insert(s, std::mem::take(&mut plans)).await?;
            }
        }
        insert(s, plans).await?;
    }
    // Ready objects without a current owner are still reusable until GC claims them.
    // Pending/invalid/deleting objects have no reusable payload; remove old keys only
    // after every retained object has been planned (INSERT OR IGNORE preserves it).
    let mut after_object = String::new();
    loop {
        let cursor = after_object.clone();
        let inventory = s.store.read(move |db| {
            let mut stmt = db.0.prepare("SELECT id,hash,size,location,state FROM shared_objects WHERE id>?1 ORDER BY id LIMIT 256")?;
            let rows = stmt.query_map([cursor], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?, row.get::<_, String>(4)?,
            )))?.collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        }).await?;
        if inventory.is_empty() {
            break;
        }
        after_object = inventory.last().unwrap().0.clone();
        let mut plans = Vec::new();
        for (object, hash, size, encoded_location, state) in inventory {
            crate::validation::uuid(&object)?;
            if !digest::valid(&hash) || size <= 0 || size as u64 > crate::nodes::snapshots::BLOCK {
                return Err(Error::bad("Invalid storage migration inventory."));
            }
            let mut plan = remote(
                format!("shared-blocks/v1/{hash}/{object}"),
                shared_blocks::key(&hash, &object),
                hash,
                Some(size as u64),
                serde_json::from_str(&encoded_location)?,
            );
            plan.discard = state != "ready";
            plans.push(plan);
        }
        insert(s, plans).await?;
    }
    let root = s.config.data_dir.join("node-backups");
    if !tokio::fs::try_exists(&root).await? {
        return Ok(());
    }
    let mut runs = tokio::fs::read_dir(&root).await?;
    while let Some(run) = runs.next_entry().await? {
        if !run.file_type().await?.is_dir() {
            continue;
        }
        let run_id = run.file_name().to_string_lossy().into_owned();
        crate::validation::uuid(&run_id)?;
        let mut intents = tokio::fs::read_dir(run.path()).await?;
        while let Some(intent) = intents.next_entry().await? {
            if !intent.file_type().await?.is_file() {
                continue;
            }
            let name = intent.file_name().to_string_lossy().into_owned();
            let Some(point_id) = name.strip_suffix(".json") else {
                continue;
            };
            if crate::validation::uuid(point_id).is_err()
                || s.store.get("node-backups", point_id).await?.is_some()
            {
                continue;
            }
            let file = tokio::fs::File::open(intent.path()).await?;
            let limit = crate::nodes::snapshots::MAX_MANIFEST_BYTES as u64 * 3 + 4096;
            let mut bytes = Vec::new();
            file.take(limit + 1).read_to_end(&mut bytes).await?;
            if bytes.len() as u64 > limit {
                return Err(Error::bad("Oversized storage migration intent."));
            }
            let point: Value = serde_json::from_slice(&bytes)?;
            if point["id"] != point_id || point["runId"] != run_id {
                return Err(Error::bad("Invalid storage migration intent owner."));
            }
            if !publication::in_s3(&point) || !point["blockFormat"].is_null() {
                continue;
            }
            let manifest = publication::manifest(s, &point).await?;
            let mut obsolete = Vec::new();
            for hash in crate::nodes::snapshots::hashes(&manifest) {
                let mut plan = remote(
                    format!("node-backups/{run_id}/blocks/{hash}"),
                    publication::key(&run_id, hash),
                    hash.into(),
                    None,
                    location(&point),
                );
                plan.discard = true;
                obsolete.push(plan);
                if obsolete.len() == 256 {
                    insert(s, std::mem::take(&mut obsolete)).await?;
                }
            }
            insert(s, obsolete).await?;
        }
        let blocks = run.path().join("blocks");
        if !tokio::fs::try_exists(&blocks).await? {
            continue;
        }
        let mut entries = tokio::fs::read_dir(blocks).await?;
        let mut plans = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let hash = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type().await?.is_file() || !digest::valid(&hash) {
                continue;
            }
            plans.push(Plan {
                source: format!("node-backups/{run_id}/blocks/{hash}"),
                target: publication::key(&run_id, &hash),
                hash,
                size: None,
                location: None,
                local_run: Some(run_id.clone()),
                discard: false,
            });
            if plans.len() == 256 {
                insert(s, std::mem::take(&mut plans)).await?;
            }
        }
        insert(s, plans).await?;
    }
    Ok(())
}

fn location(point: &Value) -> Value {
    json!({
        "destination": "s3",
        "bucket": point["bucket"],
        "endpoint": point["endpoint"]
    })
}

async fn decode(s: &Service, plan: &Plan, bytes: Vec<u8>, migration: bool) -> Result<Vec<u8>> {
    let codec = &s.hot_s3.block_codec;
    if migration {
        codec
            .decode_for_migration(
                s.vault.clone(),
                plan.source.clone(),
                plan.hash.clone(),
                plan.size,
                bytes,
            )
            .await
    } else {
        codec
            .decode(
                s.vault.clone(),
                plan.target.clone(),
                plan.hash.clone(),
                plan.size,
                bytes,
            )
            .await
    }
}

async fn copied(s: &Service, id: &str, old_bytes: usize, new_bytes: usize) -> Result<()> {
    let id = id.to_owned();
    s.store
        .write(move |db| {
            db.0.execute(
                "UPDATE disk_block_migration SET state=1,old_bytes=CASE WHEN old_bytes=0 THEN ?2 ELSE old_bytes END,new_bytes=?3 WHERE id=?1",
                params![id, old_bytes as i64, new_bytes as i64],
            )?;
            Ok(())
        })
        .await
}

async fn migrate(s: &Service, id: &str, plan: &Plan, state: i64) -> Result<()> {
    if let Some(run) = &plan.local_run {
        let path = s
            .config
            .data_dir
            .join("node-backups")
            .join(run)
            .join("blocks")
            .join(&plan.hash);
        if state == 0 {
            let input = read_local(&path).await?;
            let old_bytes = input.len();
            // An atomic rename can finish before its checkpoint. Recognize that
            // already-migrated ciphertext by its new authenticated scope.
            let output = if decode(s, plan, input.clone(), false).await.is_ok() {
                input
            } else {
                let plaintext = decode(s, plan, input, true).await?;
                s.hot_s3
                    .block_codec
                    .encode(s.vault.clone(), plan.target.clone(), plaintext)
                    .await?
            };
            let checkpoint = id.to_owned();
            s.store
                .write(move |db| {
                    db.0.execute(
                        "UPDATE disk_block_migration SET old_bytes=?2 WHERE id=?1 AND old_bytes=0",
                        params![checkpoint, old_bytes as i64],
                    )?;
                    Ok(())
                })
                .await?;
            let new_bytes = output.len();
            crate::skills::atomic_write(&path, &output).await?;
            tokio::fs::File::open(path.parent().unwrap())
                .await?
                .sync_all()
                .await?;
            decode(s, plan, read_local(&path).await?, false).await?;
            copied(s, id, old_bytes, new_bytes).await?;
        } else {
            decode(s, plan, read_local(&path).await?, false).await?;
        }
    } else {
        let location = plan.location.as_ref().unwrap();
        let storage = Storage::for_location(s, location)?;
        if storage.bucket != text(location, "bucket") {
            return Err(Error::conflict(
                "Restore the original bucket configuration before migrating storage.",
            ));
        }
        if !plan.discard {
            if state == 0 {
                let input = storage
                    .download_bytes(&plan.source, MAX_LEGACY_BYTES)
                    .await?;
                let old_bytes = input.len();
                let plaintext = decode(s, plan, input, true).await?;
                let output = match storage
                    .download_bytes(&plan.target, MAX_ENCODED_BYTES)
                    .await
                {
                    Ok(output) => {
                        decode(s, plan, output.clone(), false).await?;
                        output
                    }
                    Err(error)
                        if error.status == 409
                            && error.message == "Remote recovery block is missing." =>
                    {
                        let output = s
                            .hot_s3
                            .block_codec
                            .encode(s.vault.clone(), plan.target.clone(), plaintext)
                            .await?;
                        storage.upload_bytes(output.clone(), &plan.target).await?;
                        output
                    }
                    Err(error) => return Err(error),
                };
                // A successful PUT is not the migration commit: verify the fetched
                // ciphertext and original plaintext identity before source deletion.
                decode(
                    s,
                    plan,
                    storage
                        .download_bytes(&plan.target, MAX_ENCODED_BYTES)
                        .await?,
                    false,
                )
                .await?;
                copied(s, id, old_bytes, output.len()).await?;
            } else {
                decode(
                    s,
                    plan,
                    storage
                        .download_bytes(&plan.target, MAX_ENCODED_BYTES)
                        .await?,
                    false,
                )
                .await?;
            }
        }
        // Purge includes historical versions on providers that support versioning.
        // Repeating this after a crash is safe: the new key is disjoint.
        storage.purge_key(&plan.source).await?;
    }
    let id = id.to_owned();
    s.store
        .write(move |db| {
            db.0.execute("UPDATE disk_block_migration SET state=2 WHERE id=?1", [id])?;
            Ok(())
        })
        .await
}

async fn read_local(path: &std::path::Path) -> Result<Vec<u8>> {
    let file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::new();
    file.take(MAX_LEGACY_BYTES + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > MAX_LEGACY_BYTES {
        return Err(Error::bad("Oversized storage migration object."));
    }
    Ok(bytes)
}
