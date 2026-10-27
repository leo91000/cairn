//! Storage status reconciles mounted journals with their published S3 state.
use super::publication::{BackupStatus, PublishedPointer};
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use futures_util::StreamExt;
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
    volume["storageMode"] = status["mode"].clone();
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
        let Ok(runs) = s.store.read(monitored_runs).await else {
            continue;
        };
        futures_util::stream::iter(runs.iter().filter(|run| {
            matches!(
                run["storage"]["mode"].as_str(),
                Some("on-demand" | "native")
            )
        }))
        .for_each_concurrent(4, |run| async {
            // One slow/offline conversation must not serialize every receipt.
            let _ = refresh(&s, run).await;
        })
        .await;
    }
}

/// Retained VMs can write filesystem metadata after the completed-turn boundary.
/// Their journals still pin physical slots even when the last published status was clean.
fn monitored_runs(db: &mut Db<'_>) -> Result<Vec<Value>> {
    let mut runs = db.active()?;
    let mut seen: std::collections::HashSet<String> =
        runs.iter().map(|run| text(run, "id").to_owned()).collect();
    // Completed native disks need reconciliation even after their VM is gone.
    // A durable node receipt can outlive the HTTP response that acknowledged it.
    for run in db.json_rows(super::publication::SYNC_CANDIDATES, [])? {
        if seen.insert(text(&run, "id").to_owned()) {
            runs.push(run);
        }
    }
    for node in db.list("nodes")? {
        if node["revoked"] == true {
            continue;
        }
        for id in node["retainedRuns"]
            .as_array()
            .into_iter()
            .flatten()
            .take(2)
            .filter_map(Value::as_str)
        {
            if seen.contains(id) {
                continue;
            }
            if let Some(run) = db.run(id)?
                && super::owner_node(&run) == text(&node, "id")
            {
                seen.insert(id.to_owned());
                runs.push(run);
            }
        }
    }
    Ok(runs)
}

/// Records the node's journal status, and the publication it has acknowledged.
pub async fn refresh(s: &Service, run: &Value) -> Result<()> {
    let id = text(run, "id");
    let status = controller(s, id, "storage-status", &json!({})).await?;
    if status["mode"] == "native" {
        let (id, node, status) = (
            id.to_owned(),
            super::owner_node(run).to_owned(),
            status.clone(),
        );
        return s
            .store
            .transaction(move |db| record_status(db, &id, &node, &status, None, 0))
            .await;
    }
    if status["mode"] != super::ON_DEMAND {
        return Ok(());
    }

    let point = acknowledged_point(s, run, &status).await;
    if let Some(point) = &point {
        super::disk_grants::acknowledged(s, text(&status, "grantId"), point).await?;
    }

    let id = id.to_owned();
    let node = super::owner_node(run).to_owned();
    let revision = run["backup"]["requestedRevision"].as_u64().unwrap_or(0);
    s.store
        .transaction(move |db| record_status(db, &id, &node, &status, point.as_ref(), revision))
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
    observed_revision: u64,
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
        let completed = status["completedGeneration"].as_i64();
        let published = status["published"]["generation"].as_i64();
        let covers_completed = completed.is_some_and(|generation| {
            generation > 0
                && published.is_some_and(|published| published >= generation)
                && published == point["diskGeneration"].as_i64()
        });
        let requested = current["backup"]["requestedRevision"].as_u64().unwrap_or(0);
        let acknowledged = current["backup"]["acknowledgedRevision"]
            .as_u64()
            .unwrap_or(0);
        let acknowledged = if covers_completed {
            acknowledged.max(observed_revision.min(requested))
        } else {
            acknowledged
        };
        let pointer = serde_json::to_value(PublishedPointer {
            id: &point["id"],
            snapshot_id: &point["snapshotId"],
            captured_at: &point["capturedAt"],
            uploaded_bytes: None,
            status: if requested > acknowledged {
                BackupStatus::Pending
            } else {
                BackupStatus::Ready
            },
            error: None,
        })?;
        let mut backup = current["backup"].as_object().cloned().unwrap_or_default();
        backup.extend(pointer.as_object().unwrap().clone());
        if covers_completed {
            backup.insert("acknowledgedRevision".into(), acknowledged.into());
        }
        patch["backup"] = backup.into();
    }
    db.update_run_boundary(id, &patch)?;
    record_volume(db, id, node, status)?;
    Ok(())
}
