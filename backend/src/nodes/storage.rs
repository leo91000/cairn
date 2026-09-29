//! Storage status reconciles mounted journals with their published S3 state.
use crate::{
    error::{Error, Result},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

async fn controller(s: &Service, run: &str, operation: &str, value: &Value) -> Result<Value> {
    let base = super::transport::url(s, run).await?;
    let response = s
        .http
        .post(format!("{base}/disks/{run}/{operation}"))
        .bearer_auth(crate::execution::secret(&s.config.data_dir, "runner-secret").await?)
        .json(value)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| Error::new(503, "Node storage request interrupted."))?;
    if !response.status().is_success() {
        return Err(Error::new(
            response.status().as_u16(),
            "Node could not complete the storage operation; local work is retained.",
        ));
    }
    response.json().await.map_err(Error::internal)
}

fn record_volume(db: &crate::store::Db<'_>, run: &str, node: &str, status: &Value) -> Result<()> {
    if let Some(mut volume) = db.get("node-volumes", &format!("{run}:{node}"))? {
        volume["storageMode"] = "on-demand".into();
        volume["activeDiskMiB"] = status["activeLocalBytes"]
            .as_u64()
            .unwrap_or(0)
            .div_ceil(1048576)
            .into();
        volume["diskMiB"] = status["localBytes"]
            .as_u64()
            .unwrap_or(0)
            .div_ceil(1048576)
            .into();
        db.put("node-volumes", &volume)?;
    }
    Ok(())
}

pub async fn monitor(s: Arc<Service>) {
    loop {
        tokio::select! {
            _ = s.shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        let _ = super::publication::maintain_local_cache(&s).await;
        let Ok(runs) = s.store.read(|db| db.active()).await else {
            continue;
        };
        for run in runs {
            if run["storage"]["mode"] != "on-demand" {
                continue;
            }
            let _ = refresh(&s, &run).await;
        }
    }
}

pub async fn refresh(s: &Service, run: &Value) -> Result<()> {
    let id = text(run, "id");
    let status = controller(s, id, "storage-status", &json!({})).await?;
    if status["mode"] != "on-demand" {
        return Ok(());
    }
    let point = if let Some(backup) = status["published"]["backupId"].as_str() {
        s.store.get("node-backups", backup).await.ok().flatten()
    } else {
        None
    };
    let point = point.filter(|p| {
        p["runId"] == run["id"]
            && p["nodeId"].as_str().unwrap_or(super::LOCAL_NODE_ID)
                == run["nodeId"].as_str().unwrap_or(super::LOCAL_NODE_ID)
    });
    if let Some(point) = &point {
        super::disk_grants::acknowledged(s, text(&status, "grantId"), point).await?;
    }
    let (id, node) = (
        id.to_owned(),
        run["nodeId"]
            .as_str()
            .unwrap_or(super::LOCAL_NODE_ID)
            .to_owned(),
    );
    s.store
        .transaction(move |db| {
            let Some(current) = db.run(&id)? else {
                return Ok(());
            };
            if current["nodeId"].as_str().unwrap_or(super::LOCAL_NODE_ID) != node {
                return Ok(());
            }
            let mut patch = json!({"storage": status});
            if status["dirtyBytes"] == 0
                && let Some(point) = point
                && point["capturedAt"].as_i64().unwrap_or(0)
                    >= current["backup"]["capturedAt"].as_i64().unwrap_or(0)
            {
                patch["backup"] = json!({
                    "id": point["id"],
                    "snapshotId": point["snapshotId"],
                    "capturedAt": point["capturedAt"],
                    "status": "ready",
                    "error": null
                });
            }
            db.patch_run(&id, &patch)?;
            record_volume(db, &id, &node, &status)?;
            Ok(())
        })
        .await
}
