//! Observe local environment usage without scheduling disk publications.
use crate::{
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{future::Future, sync::Arc, time::Duration};

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

/// Observe running and unconsolidated disks, including stopped native environments.
fn monitored_runs(db: &mut Db<'_>) -> Result<Vec<Value>> {
    let mut runs = db.active()?;
    let mut seen: std::collections::HashSet<String> =
        runs.iter().map(|run| text(run, "id").to_owned()).collect();
    for run in db.json_rows(
        "SELECT data FROM run_views WHERE json_extract(boundary,'$.storage.mode') IN ('on-demand','native')
         AND json_extract(boundary,'$.storage.dirtyBytes')>0", [],
    )? {
        if seen.insert(text(&run, "id").to_owned()) {
            runs.push(run);
        }
    }
    for node in db.nodes_boundary()? {
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

/// Records local journal and base-block usage.
pub async fn refresh(s: &Service, run: &Value) -> Result<()> {
    let id = text(run, "id");
    let request = json!({});
    let status = controller(s, id, "storage-status", &request);
    refresh_with_status(s, run, status).await
}

pub(super) async fn refresh_with_status(
    s: &Service,
    run: &Value,
    status: impl Future<Output = Result<Value>>,
) -> Result<()> {
    let id = text(run, "id");
    let _refresh = s.node_storage_operation.storage_refresh(id).await;
    let status = status.await?;
    let observed_at = crate::config::now();
    if status["mode"] == "native" {
        if status["availability"] == "unavailable" {
            if status["reason"] != crate::native::MISSING_NATIVE_JOURNAL_CODE {
                return Err(Error::unavailable(
                    "Node reported an unsupported native storage availability reason.",
                ));
            }
            let (id, node, status) = (
                id.to_owned(),
                super::owner_node(run).to_owned(),
                status.clone(),
            );
            return s
                .store
                .transaction(move |db| {
                    record_missing_native_journal(db, &id, &node, &status, observed_at)
                })
                .await;
        }
        let (id, node, status) = (
            id.to_owned(),
            super::owner_node(run).to_owned(),
            status.clone(),
        );
        return s
            .store
            .transaction(move |db| record_status(db, &id, &node, &status))
            .await;
    }
    if status["mode"] != super::ON_DEMAND {
        return Ok(());
    }

    let id = id.to_owned();
    let node = super::owner_node(run).to_owned();
    s.store
        .transaction(move |db| record_status(db, &id, &node, &status))
        .await
}

/// Record that the node could not inspect the journal without replacing its last known metrics.
fn record_missing_native_journal(
    db: &Db<'_>,
    id: &str,
    node: &str,
    status: &Value,
    observed_at: i64,
) -> Result<()> {
    let Some(current) = db.run(id)? else {
        return Ok(());
    };
    if super::owner_node(&current) != node {
        return Ok(());
    }

    let storage = storage_with_missing_native_journal(&current["storage"], status, observed_at);
    db.update_run_boundary(id, &json!({ "storage": storage }))?;
    Ok(())
}

fn storage_with_missing_native_journal(current: &Value, status: &Value, observed_at: i64) -> Value {
    let mut storage = current.as_object().cloned().unwrap_or_default();
    storage.insert("mode".into(), "native".into());
    storage.insert("availability".into(), "unavailable".into());
    storage.insert("availabilityReason".into(), status["reason"].clone());
    storage.insert("metricsAvailability".into(), "stale".into());
    storage.insert("availabilityObservedAt".into(), observed_at.into());
    Value::Object(storage)
}

fn record_status(db: &Db<'_>, id: &str, node: &str, status: &Value) -> Result<()> {
    let Some(current) = db.run(id)? else {
        return Ok(());
    };
    if super::owner_node(&current) != node {
        return Ok(());
    }
    db.update_run_boundary(id, &json!({ "storage": status }))?;
    record_volume(db, id, node, status)?;
    Ok(())
}

#[cfg(test)]
#[path = "storage/tests.rs"]
mod tests;
