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
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const IDLE_MS: i64 = 60 * 60 * 1000;
const BATCH: usize = 32;

pub async fn maintain(s: Arc<Service>) {
    let mut cursor = String::new();
    let mut last_idle = Instant::now();
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => return,
            () = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        if let Err(error) = pressure(&s).await {
            tracing::warn!(%error, "Native pressure cache eviction deferred");
        }
        if last_idle.elapsed() < Duration::from_secs(60) {
            continue;
        }
        last_idle = Instant::now();
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

/// An allocation failure requests asynchronous reclamation. Never perform network
/// eviction inside a heartbeat transaction: lease renewal must remain prompt.
pub(crate) fn request(db: &Db<'_>, node: &Value) -> Result<()> {
    if !pressure_allowed(db, node)? {
        return Ok(());
    }
    db.set(
        &format!("native-capacity-pressure:{}", text(node, "id")),
        &json!(true),
        Some(now() + 30_000),
    )
}

fn pressure_allowed(db: &Db<'_>, node: &Value) -> Result<bool> {
    let Some(observed) = node["diskCapacity"]["capturedAt"].as_i64() else {
        return Ok(false);
    };
    let last = db
        .kv(&format!("native-capacity-reclaimed:{}", text(node, "id")))?
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    Ok(observed > last && (0..=30_000).contains(&now().saturating_sub(observed)))
}

fn pressure_page(db: &Db<'_>, node: &str) -> Result<Vec<Value>> {
    let cursor = db
        .kv(&format!("native-capacity-cursor:{node}"))?
        .unwrap_or_default();
    // A normal chat continuation clears finishedAt before admission. Its last
    // released execution still establishes when that inactive cache was used.
    let idle_at = "COALESCE(json_extract(r.data,'$.finishedAt'),
        (SELECT MAX(json_extract(a.data,'$.releasedAt')) FROM records a
         WHERE a.kind='node-attempts' AND json_extract(a.data,'$.runId')=r.id),0)";
    let query = format!(
        "SELECT json_set(r.data,'$._evictionIdleAt',{idle_at}) FROM runs r
         WHERE status IN ('succeeded','failed','cancelled','interrupted','queued')
         AND ({idle_at}>?2 OR ({idle_at}=?2 AND r.id>?3))
         AND EXISTS(SELECT 1 FROM records v WHERE v.kind='node-volumes'
           AND json_extract(v.data,'$.runId')=r.id
           AND json_extract(v.data,'$.nodeId')=?1
           AND json_extract(v.data,'$.materialized')=1
           AND (json_extract(v.data,'$.lastEvictedBackupId') IS NOT json_extract(r.data,'$.backup.id')
                OR json_extract(v.data,'$.lastEvictedDiskMiB') IS NULL
                OR json_extract(v.data,'$.diskMiB')>json_extract(v.data,'$.lastEvictedDiskMiB')))
         ORDER BY {idle_at}, r.id LIMIT 32"
    );
    db.json_rows(
        &query,
        rusqlite::params![
            node,
            cursor["finishedAt"].as_i64().unwrap_or(0),
            text(&cursor, "id")
        ],
    )
}

async fn pressure(s: &Service) -> Result<()> {
    let nodes = s
        .store
        .transaction(|db| {
            let mut nodes = Vec::new();
            for node in db.list("nodes")? {
                let id = text(&node, "id");
                let key = format!("native-capacity-pressure:{id}");
                if db.kv(&key)? == Some(json!(true)) {
                    db.delete(&key)?;
                    if pressure_allowed(db, &node)? {
                        nodes.push(id.to_owned());
                    }
                }
            }
            Ok(nodes)
        })
        .await?;
    for node in nodes {
        let id = node.clone();
        let runs = s.store.read(move |db| pressure_page(db, &id)).await?;
        let mut reclaimed = false;
        for run in &runs {
            // Keep all publication/ownership guards; only the idle age changes.
            // A dirty or temporarily unavailable cache must not starve others.
            match evict_clean(s, text(run, "id"), now(), Some(&node)).await {
                Ok(true) => {
                    reclaimed = true;
                    break;
                }
                Ok(false) => {}
                Err(error) => tracing::warn!(run = text(run, "id"), %error,
                    "Native pressure cache candidate deferred"),
            }
        }
        s.store
            .transaction(move |db| {
                let key = format!("native-capacity-cursor:{node}");
                if reclaimed || runs.len() < BATCH {
                    db.delete(&key)?;
                } else if let Some(last) = runs.last() {
                    db.set(
                        &key,
                        &json!({ "id": last["id"], "finishedAt": last["_evictionIdleAt"] }),
                        None,
                    )?;
                }
                Ok(())
            })
            .await?;
    }
    Ok(())
}

fn candidate(
    db: &mut Db<'_>,
    run: &str,
    cutoff: i64,
    expected_node: Option<&str>,
) -> Result<Option<(Value, Value, Value)>> {
    let Some(current) = db.run(run)? else {
        return Ok(None);
    };
    let queued_resume = expected_node.is_some() && current["status"] == "queued";
    if (!queued_resume
        && (!RunStatus::of(&current).is_some_and(|status| !status.is_active())
            || current["finishedAt"]
                .as_i64()
                .is_none_or(|finished| finished > cutoff)))
        || current["storage"]["mode"] != "on-demand"
        || current["backup"]["status"] != "ready"
        || super::publication::requested(&current)
        || !current["moveRequest"].is_null()
        || !current["nodeState"].is_null()
        || !queued_resume && current["recoveryPending"] == true
    {
        return Ok(None);
    }
    let checkpoint = db.kv(&checkpoint::key(run))?.unwrap_or_default();
    let node_id = text(&checkpoint, "nodeId");
    if expected_node.is_some_and(|expected| expected != node_id) {
        return Ok(None);
    }
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
    let already_evicted = volume["lastEvictedBackupId"] == current["backup"]["id"]
        && volume["lastEvictedDiskMiB"].as_u64().is_some_and(|after| {
            volume["diskMiB"]
                .as_u64()
                .is_some_and(|current| current <= after)
        });
    if volume["materialized"] != true || already_evicted {
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
    evict_clean(s, run, time.saturating_sub(IDLE_MS), None).await
}

async fn evict_clean(
    s: &Service,
    run: &str,
    cutoff: i64,
    expected_node: Option<&str>,
) -> Result<bool> {
    let preparation = s.worker.run_preparation_lock(run).await;
    let Ok(_preparation) = preparation.try_lock() else {
        return Ok(false);
    };
    if s.worker.active.lock().await.contains_key(run) {
        return Ok(false);
    }
    let _publication = s.node_backup_operation.lock(run).await;
    let id = run.to_owned();
    let expected_node = expected_node.map(str::to_owned);
    let Some((node, point, expected)) = s
        .store
        .read(move |db| candidate(db, &id, cutoff, expected_node.as_deref()))
        .await?
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
            // Admission retries may still see the pre-eviction sample. Do not
            // reclaim another cache until the node measures after completion.
            db.set(
                &format!("native-capacity-reclaimed:{node_id}"),
                &json!(now()),
                None,
            )?;
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
                volume["lastEvictedDiskMiB"] = status["localBytes"]
                    .as_u64()
                    .unwrap_or(0)
                    .div_ceil(1_048_576)
                    .into();
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
    async fn pressure_can_reclaim_refetched_clean_payload_without_repeating_empty_evictions() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let (run, time) = (id(), now());
                records(db, &run, time)?;
                let node = super::super::LOCAL_NODE_ID;
                let mut volume = db.get("node-volumes", &format!("{run}:{node}"))?.unwrap();
                volume["lastEvictedBackupId"] = db.run(&run)?.unwrap()["backup"]["id"].clone();
                volume["lastEvictedDiskMiB"] = volume["diskMiB"].clone();
                db.put("node-volumes", &volume)?;
                assert!(candidate(db, &run, time, Some(node))?.is_none());
                assert!(pressure_page(db, node)?.is_empty());
                volume["diskMiB"] = 124.into();
                db.put("node-volumes", &volume)?;
                assert!(candidate(db, &run, time, Some(node))?.is_some());
                assert_eq!(pressure_page(db, node)?[0]["id"], run);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pressure_can_reclaim_a_queued_resumes_old_clean_cache_without_deleting_work() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let (run, time) = (id(), now());
                records(db, &run, time)?;
                db.patch_run(
                    &run,
                    &json!({ "status": "queued", "finishedAt": null, "recoveryPending": true }),
                )?;
                let node = super::super::LOCAL_NODE_ID;
                assert_eq!(pressure_page(db, node)?[0]["id"], run);
                assert!(candidate(db, &run, time, None)?.is_none());
                assert!(candidate(db, &run, time, Some(node))?.is_some());
                db.patch_run(&run, &json!({ "backup": { "status": "saving" } }))?;
                assert!(candidate(db, &run, time, Some(node))?.is_none());
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pressure_never_evicts_the_current_cache_on_another_node_after_a_move() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let (run, time, previous) = (id(), now(), id());
                records(db, &run, time)?;
                db.put(
                    "node-volumes",
                    &json!({ "id": format!("{run}:{previous}"),
                "runId": run, "nodeId": previous, "materialized": true }),
                )?;
                assert_eq!(pressure_page(db, &previous)?[0]["id"], run);
                assert!(candidate(db, &run, time, Some(&previous))?.is_none());
                assert!(candidate(db, &run, time, Some(super::super::LOCAL_NODE_ID))?.is_some());
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pressure_paginates_past_ineligible_old_caches_and_waits_for_post_eviction_observation()
    {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let node_id = super::super::LOCAL_NODE_ID;
                let time = now();
                let mut final_run = String::new();
                for index in 0..33 {
                    let run = id();
                    records(db, &run, time)?;
                    db.patch_run(&run, &json!({ "finishedAt": time - IDLE_MS + index }))?;
                    if index < 32 {
                        let mut backup = db.run(&run)?.unwrap()["backup"].clone();
                        backup["status"] = "error".into();
                        db.patch_run(&run, &json!({ "backup": backup }))?;
                    } else {
                        final_run = run;
                    }
                }
                let page = pressure_page(db, node_id)?;
                assert_eq!(page.len(), BATCH);
                for run in &page {
                    assert!(candidate(db, text(run, "id"), time, None)?.is_none());
                }
                let last = page.last().unwrap();
                db.set(
                    &format!("native-capacity-cursor:{node_id}"),
                    &json!({ "id": last["id"], "finishedAt": last["_evictionIdleAt"] }),
                    None,
                )?;
                let page = pressure_page(db, node_id)?;
                assert_eq!(page.len(), 1);
                assert_eq!(page[0]["id"], final_run);
                assert!(candidate(db, &final_run, time, None)?.is_some());
                let mut node = db.get("nodes", node_id)?.unwrap();
                node["diskCapacity"] = json!({ "capturedAt": time - 2_000 });
                db.set(
                    &format!("native-capacity-reclaimed:{node_id}"),
                    &json!(time - 1_000),
                    None,
                )?;
                request(db, &node)?;
                let key = format!("native-capacity-pressure:{node_id}");
                assert!(db.kv(&key)?.is_none());
                node["diskCapacity"]["capturedAt"] = time.into();
                request(db, &node)?;
                assert_eq!(db.kv(&key)?, Some(json!(true)));
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pressure_can_reclaim_recent_clean_cache_but_never_unpublished_or_owned_work() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let (run, time) = (id(), now());
                records(db, &run, time)?;
                db.patch_run(&run, &json!({ "finishedAt": time }))?;
                let saved = db.kv(&checkpoint::key(&run))?.unwrap();
                let mut attempt = db.get("node-attempts", text(&saved, "runnerId"))?.unwrap();
                attempt["releasedAt"] = time.into();
                db.put("node-attempts", &attempt)?;
                assert!(candidate(db, &run, time - IDLE_MS, None)?.is_none());
                assert!(candidate(db, &run, time, None)?.is_some());
                attempt["released"] = false.into();
                db.put("node-attempts", &attempt)?;
                assert!(candidate(db, &run, time, None)?.is_none());
                attempt["released"] = true.into();
                db.put("node-attempts", &attempt)?;
                db.patch_run(&run, &json!({ "backup": { "requestedRevision": 3 } }))?;
                assert!(candidate(db, &run, time, None)?.is_none());
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn native_idle_cache_requires_one_hour_and_latest_released_acknowledged_execution() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.transaction(|db| {
            let (run, time) = (id(), now());
            records(db, &run, time)?;
            assert!(candidate(db, &run, time - IDLE_MS - 1, None)?.is_none());
            assert!(candidate(db, &run, time - IDLE_MS, None)?.is_some());
            let original = db.run(&run)?.unwrap();
            for patch in [
                json!({ "status": "queued" }), json!({ "status": "running" }),
                json!({ "recoveryPending": true }), json!({ "moveRequest": {} }), json!({ "nodeState": "restoring" }),
                json!({ "backup": { "status": "error" } }), json!({ "backup": { "status": "saving" } }),
                json!({ "backup": { "status": "ready", "requestedRevision": 3, "acknowledgedRevision": 2 } }),
            ] {
                db.patch_run(&run, &patch)?;
                assert!(candidate(db, &run, time - IDLE_MS, None)?.is_none(), "{patch}");
                db.patch_run(&run, &original)?;
            }
            let saved = db.kv(&checkpoint::key(&run))?.unwrap();
            let mut execution = db.get("node-attempts", text(&saved, "runnerId"))?.unwrap();
            execution["releasedAt"] = (time - IDLE_MS + 1).into();
            db.put("node-attempts", &execution)?;
            assert!(candidate(db, &run, time - IDLE_MS, None)?.is_none());
            execution["releasedAt"] = (time - IDLE_MS).into();
            db.put("node-attempts", &execution)?;
            // A real consumed destination reservation has no release timestamp.
            db.put("node-attempts", &json!({
                "id": id(), "role": "destination", "runId": run, "nodeId": super::super::LOCAL_NODE_ID,
                "released": true,
            }))?;
            assert!(candidate(db, &run, time - IDLE_MS, None)?.is_some());
            execution["released"] = false.into();
            db.put("node-attempts", &execution)?;
            assert!(candidate(db, &run, time - IDLE_MS, None)?.is_none());
            execution["released"] = true.into();
            execution["releasedAt"] = Value::Null;
            db.put("node-attempts", &execution)?;
            assert!(candidate(db, &run, time - IDLE_MS, None)?.is_none());
            Ok(())
        }).await.unwrap();
    }
}
