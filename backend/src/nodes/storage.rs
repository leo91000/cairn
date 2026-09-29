//! Storage status reconciles mounted journals with their published S3 state.
use super::publication::{BackupStatus, PublishedPointer};
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

async fn controller(s: &Service, run: &str, operation: &str, value: &Value) -> Result<Value> {
    let base = super::transport::url(s, run).await?;
    let response = s
        .http
        .post(format!("{base}/disks/{run}/{operation}"))
        .bearer_auth(super::runner_secret(s).await?)
        .json(value)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| Error::unavailable("Node storage request interrupted."))?;
    if !response.status().is_success() {
        return Err(Error::new(
            response.status().as_u16(),
            "Node could not complete the storage operation; local work is retained.",
        ));
    }
    response.json().await.map_err(Error::internal)
}

fn record_volume(db: &Db<'_>, run: &str, node: &str, status: &Value) -> Result<()> {
    let Some(mut volume) = db.get("node-volumes", &format!("{run}:{node}"))? else {
        return Ok(());
    };
    let mi_b = |key: &str| status[key].as_u64().unwrap_or(0).div_ceil(1_048_576);
    volume["storageMode"] = super::ON_DEMAND.into();
    volume["activeDiskMiB"] = mi_b("activeLocalBytes").into();
    volume["diskMiB"] = mi_b("localBytes").into();
    db.put("node-volumes", &volume)?;
    Ok(())
}

pub async fn monitor(s: Arc<Service>) {
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => return,
            () = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        // Cache pressure is retried on the next tick.
        let _ = super::publication::maintain_local_cache(&s).await;
        let Ok(runs) = s.store.read(|db| db.active()).await else {
            continue;
        };
        for run in runs
            .iter()
            .filter(|run| run["storage"]["mode"] == super::ON_DEMAND)
        {
            // Unreachable controllers and unacknowledged grants are retried on the next tick.
            let _ = refresh(&s, run).await;
        }
    }
}

/// Records the node's journal status, and the publication it has acknowledged.
pub async fn refresh(s: &Service, run: &Value) -> Result<()> {
    let id = text(run, "id");
    let status = controller(s, id, "storage-status", &json!({})).await?;
    if status["mode"] != super::ON_DEMAND {
        return Ok(());
    }

    let point = acknowledged_point(s, run, &status).await;
    if let Some(point) = &point {
        super::disk_grants::acknowledged(s, text(&status, "grantId"), point).await?;
    }

    let id = id.to_owned();
    let node = super::owner_node(run).to_owned();
    s.store
        .transaction(move |db| record_status(db, &id, &node, &status, point.as_ref()))
        .await
}

/// The publication the node reports as mounted, if it belongs to this run and node.
async fn acknowledged_point(s: &Service, run: &Value, status: &Value) -> Option<Value> {
    let backup = status["published"]["backupId"].as_str()?;
    let point = s.store.get("node-backups", backup).await.ok().flatten()?;
    let same_owner =
        point["runId"] == run["id"] && super::owner_node(&point) == super::owner_node(run);
    same_owner.then_some(point)
}

fn record_status(
    db: &Db<'_>,
    id: &str,
    node: &str,
    status: &Value,
    point: Option<&Value>,
) -> Result<()> {
    let Some(current) = db.run(id)? else {
        return Ok(());
    };
    if super::owner_node(&current) != node {
        return Ok(());
    }
    let mut patch = json!({ "storage": status });
    let captured_at = |value: &Value| value["capturedAt"].as_i64().unwrap_or(0);
    if status["dirtyBytes"] == 0
        && let Some(point) = point
        && captured_at(point) >= captured_at(&current["backup"])
    {
        patch["backup"] = serde_json::to_value(PublishedPointer {
            id: &point["id"],
            snapshot_id: &point["snapshotId"],
            captured_at: &point["capturedAt"],
            uploaded_bytes: None,
            status: BackupStatus::Ready,
            error: None,
        })?;
    }
    db.patch_run(id, &patch)?;
    record_volume(db, id, node, status)?;
    Ok(())
}
