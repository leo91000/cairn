//! Progressive migration and storage status are independent of recovery uploads.
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
        .timeout(Duration::from_secs(if operation == "migrate" {
            3600
        } else {
            10
        }))
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
        tokio::select! {_=s.shutdown.cancelled()=>return,_=tokio::time::sleep(Duration::from_secs(5))=>{}}
        let _ = super::backups::maintain_local_cache(&s).await;
        let Ok(runs) = s.store.read(|db| db.active()).await else {
            continue;
        };
        for run in runs {
            if run["storage"]["mode"] != "on-demand" {
                continue;
            }
            let id = text(&run, "id");
            if let Ok(status) = controller(&s, id, "storage-status", &json!({})).await {
                // A raw environment on an enabled node is awaiting stopped migration.
                // Do not forget the requested mode before that migration can run.
                if status["mode"] == "on-demand" {
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
                    if let Some(point) = &point
                        && super::disk_grants::acknowledged(&s, text(&status, "grantId"), point)
                            .await
                            .is_err()
                    {
                        continue;
                    }
                    let (id, node) = (
                        id.to_owned(),
                        run["nodeId"]
                            .as_str()
                            .unwrap_or(super::LOCAL_NODE_ID)
                            .to_owned(),
                    );
                    let _ = s
                        .store
                        .transaction(move |db| {
                            let Some(current)=db.run(&id)? else{return Ok(());};
                            if current["nodeId"].as_str().unwrap_or(super::LOCAL_NODE_ID)!=node{return Ok(());}
                            let mut patch=json!({"storage":status});
                            if status["dirtyBytes"]==0 && let Some(point)=point && point["capturedAt"].as_i64().unwrap_or(0)>=current["backup"]["capturedAt"].as_i64().unwrap_or(0) {patch["backup"]=json!({"id":point["id"],"snapshotId":point["snapshotId"],"capturedAt":point["capturedAt"],"status":"ready","error":null});}
                            db.patch_run(&id, &patch)?;
                            record_volume(db, &id, &node, &status)?;
                            Ok(())
                        })
                        .await;
                }
            }
        }
    }
}
pub async fn migrate_one(s: &Service) -> Result<bool> {
    // Archives and migrations cannot erase or replace each other's disk state.
    let _storage = match crate::conversation_lifecycle::storage_lock(&s.config.data_dir) {
        Ok(lock) => lock,
        Err(e) if e.status == 409 => return Ok(false),
        Err(e) => return Err(e),
    };
    let nodes = s.store.list("nodes").await?;
    for chat in s.store.list("chats").await? {
        if crate::conversation_lifecycle::state(&chat) != "active" {
            continue;
        }
        let run_id = text(&chat, "runId");
        let Ok(mut run) = s.store.run(run_id).await else {
            continue;
        };
        if ["running", "queued"].contains(&text(&run, "status"))
            || run["isolated"] != true
            || run["moveRequest"].is_object()
        {
            continue;
        }
        let checkpoint = s
            .store
            .kv(&format!("run-checkpoint:{run_id}"))
            .await?
            .unwrap_or_default();
        let node = checkpoint["nodeId"]
            .as_str()
            .unwrap_or(super::LOCAL_NODE_ID);
        let Some(record) = nodes
            .iter()
            .find(|n| n["id"] == node && n["storage"]["enabled"] == true && n["revoked"] != true)
        else {
            continue;
        };
        if run["storageMigration"]["retryAt"]
            .as_i64()
            .is_some_and(|at| at > crate::config::now())
        {
            continue;
        }
        if run["storage"]["mode"] == "on-demand"
            && run["storage"]["checkedAt"]
                .as_i64()
                .is_some_and(|at| crate::config::now() - at < 60000)
        {
            continue;
        }
        let result=async {
            let status=controller(s,run_id,"storage-status",&json!({})).await?;
            if status["mode"]=="on-demand" {
                if status["dirtyBytes"].as_u64().unwrap_or(0)>0 {
                    run["storage"]=status;super::backups::capture(s,&run).await?;
                    return controller(s,run_id,"storage-status",&json!({})).await;
                }
                return Ok(status);
            }
            run["storageRequested"]=true.into();run["storageMigrationCapture"]=true.into();
            let point=super::backups::capture(s,&run).await?;
            let point=s.get("node-backups",text(&point,"id")).await?;
            let manifest=super::backups::manifest(s,&point).await?;
            let grant=super::disk_grants::issue(s,&run,node,&point).await?;
            controller(s,run_id,"migrate",&json!({"manifest":manifest,"master":s.config.public_url,"grant":grant,"backupId":point["id"],"policy":record["storage"]})).await
        }.await;
        let (node, run_id) = (node.to_owned(), run_id.to_owned());
        let error = result.as_ref().err().map(|e| e.message.clone());
        s.store.transaction(move |db| {
            let mut record=db.get("nodes",&node)?.ok_or_else(||Error::new(404,"Node removed."))?;
            record["storageMigration"]=json!({"runId":run_id,"error":error,"retryAt":if error.is_some(){crate::config::now()+60000}else{0}});
            db.put("nodes",&record)?;
            db.patch_run(&run_id,&json!({"storageMigration":record["storageMigration"]}))?;
            if let Ok(mut status)=result {
                let current=db.kv(&format!("run-checkpoint:{run_id}"))?.unwrap_or_default();
                if current["nodeId"]==checkpoint["nodeId"] && current["runnerId"]==checkpoint["runnerId"] {
                    status["mode"]="on-demand".into();status["migrated"]=true.into();status["checkedAt"]=crate::config::now().into();
                    db.patch_run(&run_id,&json!({"storage":status}))?;
                    record_volume(db, &run_id, &node, &status)?;
                }
            }
            Ok(())
        }).await?;
        return Ok(true);
    }
    Ok(false)
}
pub async fn migrate(s: Arc<Service>) {
    let mut delay = Duration::from_secs(10);
    loop {
        tokio::select! {_=s.shutdown.cancelled()=>return,_=tokio::time::sleep(delay)=>{}}
        let result =
            tokio::select! {_=s.shutdown.cancelled()=>return,result=migrate_one(&s)=>result};
        delay = match result {
            Ok(true) => Duration::from_millis(100),
            Ok(false) => Duration::from_secs(10),
            Err(error) => {
                tracing::warn!(message=%error.message,"Storage migration postponed");
                Duration::from_secs(10)
            }
        };
    }
}
