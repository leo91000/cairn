//! One-hour idle policy for clean native block caches. Journals remain mounted bases.
use crate::{
    config::now,
    error::{Error, Result},
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::text,
    worker::checkpoint,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const IDLE_MS: i64 = 60 * 60 * 1000;
const BATCH: usize = 32;

pub async fn maintain(s: Arc<Service>) {
    let mut cursor = String::new();
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => return,
            () = tokio::time::sleep(Duration::from_secs(60)) => {}
        }
        let (after, cutoff) = (cursor.clone(), now().saturating_sub(IDLE_MS));
        let runs = s.store.read(move |db| db.json_rows(
            "SELECT r.data FROM runs r WHERE r.id>?1
             AND status IN ('succeeded','failed','cancelled','interrupted')
             AND json_extract(r.data,'$.finishedAt')<=?2
             AND json_extract(r.data,'$.storage.mode')='on-demand'
             AND (json_extract(r.data,'$.executionBackend')='native'
                  OR json_extract(r.data,'$.snapshot.agent.access.backend')='native')
             AND EXISTS(SELECT 1 FROM records v WHERE v.kind='node-volumes'
                AND json_extract(v.data,'$.runId')=r.id
                AND json_extract(v.data,'$.materialized')=1
                AND json_extract(v.data,'$.lastEvictedBackupId') IS NOT json_extract(r.data,'$.backup.id'))
             ORDER BY r.id LIMIT 32", rusqlite::params![after, cutoff],
        )).await;
        let Ok(runs) = runs else { continue };
        cursor = if runs.len() == BATCH {
            text(runs.last().unwrap(), "id").to_owned()
        } else {
            String::new()
        };
        for run in runs {
            if s.shutdown.is_cancelled() {
                return;
            }
            if let Err(error) = evict_idle(&s, text(&run, "id"), now()).await {
                tracing::warn!(run = text(&run, "id"), %error, "Native clean cache eviction deferred");
            }
        }
    }
}

fn candidate(db: &mut Db<'_>, run: &str, time: i64) -> Result<Option<(Value, Value, Value)>> {
    let Some(current) = db.run(run)? else {
        return Ok(None);
    };
    let cutoff = time.saturating_sub(IDLE_MS);
    if !RunStatus::of(&current).is_some_and(|status| !status.is_active())
        || current["finishedAt"]
            .as_i64()
            .is_none_or(|finished| finished > cutoff)
        || current["storage"]["mode"] != "on-demand"
        || current["backup"]["status"] != "ready"
        || super::publication::requested(&current)
        || !current["moveRequest"].is_null()
        || !current["nodeState"].is_null()
        || current["recoveryPending"] == true
    {
        return Ok(None);
    }
    let checkpoint = db.kv(&checkpoint::key(run))?.unwrap_or_default();
    let node_id = text(&checkpoint, "nodeId");
    let Some(node) = db.get("nodes", node_id)? else {
        return Ok(None);
    };
    if !crate::native::policy::node_native(&node)
        || node["revoked"] == true
        || !node["maintenance"].is_null()
        || !super::seen_within(&node, 60_000)
    {
        return Ok(None);
    }
    let Some(volume) = db.get("node-volumes", &format!("{run}:{node_id}"))? else {
        return Ok(None);
    };
    if volume["materialized"] != true || volume["lastEvictedBackupId"] == current["backup"]["id"] {
        return Ok(None);
    }
    let attempts = db.json_rows(
        "SELECT data FROM records WHERE kind='node-attempts' AND json_extract(data,'$.runId')=?",
        [run],
    )?;
    let execution = attempts
        .iter()
        .find(|attempt| attempt["id"] == checkpoint["runnerId"]);
    // Consumed historical destination reservations may lack releasedAt. The
    // actual latest execution must positively prove that its writer stopped.
    if execution.is_none_or(|attempt| {
        attempt["releasedAt"]
            .as_i64()
            .is_none_or(|released| released > cutoff)
    }) || attempts.iter().any(|attempt| {
        attempt["released"] != true
            || attempt["releasedAt"]
                .as_i64()
                .is_some_and(|released| released > cutoff)
    }) {
        return Ok(None);
    }
    let Some(point) = db.get("node-backups", text(&current["backup"], "id"))? else {
        return Ok(None);
    };
    if point["runId"] != current["id"]
        || point["nodeId"] != node["id"]
        || point["snapshotId"] != current["backup"]["snapshotId"]
        || !super::publication::in_s3(&point)
    {
        return Ok(None);
    }
    let expected = json!({
        "attempt": checkpoint["runnerId"], "backupId": point["id"],
        "generation": point["diskGeneration"], "grantId": current["storage"]["grantId"],
    });
    Ok(Some((node, point, expected)))
}

pub(crate) async fn evict_idle(s: &Service, run: &str, time: i64) -> Result<bool> {
    let preparation = s.worker.run_preparation_lock(run).await;
    let Ok(_preparation) = preparation.try_lock() else {
        return Ok(false);
    };
    if s.worker.active.lock().await.contains_key(run) {
        return Ok(false);
    }
    let _publication = s.node_backup_operation.lock(run).await;
    let id = run.to_owned();
    let Some((node, point, expected)) = s.store.read(move |db| candidate(db, &id, time)).await?
    else {
        return Ok(false);
    };
    let manifest = super::publication::manifest(s, &point).await?;
    if manifest["kind"] != crate::native::block::KIND
        || manifest["onDemand"] != true
        || manifest["generation"] != expected["generation"]
    {
        return Err(Error::conflict(
            "Native eviction requires its current S3 block generation.",
        ));
    }
    super::publication::storage_for(s, &point)?;
    let response = s
        .http
        .post(format!(
            "{}/disks/{run}/evict",
            super::controller_url(s, text(&node, "id"))
        ))
        .bearer_auth(super::runner_secret(s).await?)
        .json(&expected)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|_| Error::unavailable("Native cache eviction interrupted; retry safely."))?;
    if !response.status().is_success() {
        return Err(Error::new(
            response.status().as_u16(),
            "Native cache eviction deferred; journal retained.",
        ));
    }
    let status: Value = response.json().await.map_err(Error::internal)?;
    if status["mode"] != "on-demand"
        || status["dirtyBytes"] != 0
        || status["published"]["backupId"] != point["id"]
    {
        return Err(Error::unavailable(
            "Native cache eviction completion was not confirmed.",
        ));
    }
    let (run, node_id) = (run.to_owned(), text(&node, "id").to_owned());
    s.store
        .transaction(move |db| {
            let Some(current) = db.run(&run)? else {
                return Ok(());
            };
            let checkpoint = db.kv(&checkpoint::key(&run))?.unwrap_or_default();
            if checkpoint["nodeId"] != node_id
                || checkpoint["runnerId"] != expected["attempt"]
                || current["backup"]["id"] != point["id"]
            {
                return Ok(());
            }
            if let Some(mut volume) = db.get("node-volumes", &format!("{run}:{node_id}"))? {
                volume["lastEvictedBackupId"] = point["id"].clone();
                volume["diskMiB"] = status["localBytes"]
                    .as_u64()
                    .unwrap_or(0)
                    .div_ceil(1_048_576)
                    .into();
                volume["activeDiskMiB"] = status["activeLocalBytes"]
                    .as_u64()
                    .unwrap_or(0)
                    .div_ceil(1_048_576)
                    .into();
                db.put("node-volumes", &volume)?;
            }
            db.patch_run(&run, &json!({ "storage": status }))?;
            Ok(())
        })
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::id, store::Store};

    fn records(db: &Db<'_>, run: &str, time: i64) -> Result<()> {
        let node = super::super::LOCAL_NODE_ID;
        let attempt = id();
        let point = json!({
            "id": id(), "runId": run, "nodeId": node, "snapshotId": id(),
            "diskGeneration": 7, "destination": "s3", "bucket": "synthetic-bucket",
        });
        db.put("nodes", &json!({
            "id": node, "lastSeen": now(), "capabilities": { "backend": "native", "ublk": true },
        }))?;
        db.add_run(&json!({
            "id": run, "taskId": id(), "createdAt": time - IDLE_MS, "finishedAt": time - IDLE_MS,
            "status": "succeeded", "executionBackend": "native", "recoveryPending": false,
            "nodeState": null, "moveRequest": null,
            "storage": { "mode": "on-demand", "grantId": "synthetic-grant-id" },
            "backup": { "id": point["id"], "snapshotId": point["snapshotId"], "status": "ready",
                "requestedRevision": 2, "acknowledgedRevision": 2 },
        }), None)?;
        db.set(
            &checkpoint::key(run),
            &json!({ "nodeId": node, "runnerId": attempt }),
            None,
        )?;
        db.put(
            "node-volumes",
            &json!({
                "id": format!("{run}:{node}"), "runId": run, "nodeId": node,
                "materialized": true, "diskMiB": 123, "activeDiskMiB": 100,
            }),
        )?;
        db.put(
            "node-attempts",
            &json!({
                "id": attempt, "role": "execution", "runId": run, "nodeId": node,
                "released": true, "releasedAt": time - IDLE_MS,
            }),
        )?;
        db.put("node-backups", &point)?;
        Ok(())
    }

    #[tokio::test]
    async fn native_idle_cache_requires_one_hour_and_latest_released_acknowledged_execution() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.transaction(|db| {
            let (run, time) = (id(), now());
            records(db, &run, time)?;
            assert!(candidate(db, &run, time - 1)?.is_none());
            assert!(candidate(db, &run, time)?.is_some());
            let original = db.run(&run)?.unwrap();
            for patch in [
                json!({ "status": "queued" }), json!({ "status": "running" }),
                json!({ "recoveryPending": true }), json!({ "moveRequest": {} }), json!({ "nodeState": "restoring" }),
                json!({ "backup": { "status": "error" } }), json!({ "backup": { "status": "saving" } }),
                json!({ "backup": { "status": "ready", "requestedRevision": 3, "acknowledgedRevision": 2 } }),
            ] {
                db.patch_run(&run, &patch)?;
                assert!(candidate(db, &run, time)?.is_none(), "{patch}");
                db.patch_run(&run, &original)?;
            }
            let saved = db.kv(&checkpoint::key(&run))?.unwrap();
            let mut execution = db.get("node-attempts", text(&saved, "runnerId"))?.unwrap();
            execution["releasedAt"] = (time - IDLE_MS + 1).into();
            db.put("node-attempts", &execution)?;
            assert!(candidate(db, &run, time)?.is_none());
            execution["releasedAt"] = (time - IDLE_MS).into();
            db.put("node-attempts", &execution)?;
            // A real consumed destination reservation has no release timestamp.
            db.put("node-attempts", &json!({
                "id": id(), "role": "destination", "runId": run, "nodeId": super::super::LOCAL_NODE_ID,
                "released": true,
            }))?;
            assert!(candidate(db, &run, time)?.is_some());
            execution["released"] = false.into();
            db.put("node-attempts", &execution)?;
            assert!(candidate(db, &run, time)?.is_none());
            execution["released"] = true.into();
            execution["releasedAt"] = Value::Null;
            db.put("node-attempts", &execution)?;
            assert!(candidate(db, &run, time)?.is_none());
            Ok(())
        }).await.unwrap();
    }
}
