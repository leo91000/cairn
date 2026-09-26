//! Durable pause/copy/resume coordination. Never changes a destination before fencing.
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::time::Duration;
pub fn tool() -> Value {
    json!({"name":"request_capacity","description":"Request CPU/RAM/disk for this conversation on an authorized node. Failure leaves the current conversation running. A successful request schedules pause, full environment transfer and resume. Optional waitSeconds is bounded to one hour; GPU is not supported.","inputSchema":{"type":"object","properties":{"cpu":{"type":"integer","minimum":1},"memoryMiB":{"type":"integer","minimum":128},"diskMiB":{"type":"integer","minimum":128},"requiredTags":{"type":"array","maxItems":32,"items":{"type":"string","maxLength":40}},"nodeId":{"type":"string","format":"uuid"},"waitSeconds":{"type":"integer","minimum":0,"maximum":3600}},"required":["cpu","memoryMiB","diskMiB"],"additionalProperties":false}})
}
pub async fn request(s: &Service, run: &Value, args: &Value) -> Result<Value> {
    let resources: super::Resources = serde_json::from_value(
        json!({"cpu":args["cpu"],"memoryMiB":args["memoryMiB"],"diskMiB":args["diskMiB"]}),
    )
    .map_err(|_| Error::bad("Invalid capacity request."))?;
    resources.validate()?;
    let existing = run["resources"]["diskMiB"]
        .as_u64()
        .or(run["requestedResources"]["diskMiB"].as_u64())
        .unwrap_or(32768);
    if resources.disk_mi_b < existing {
        return Err(Error::bad(
            "An existing VM disk cannot shrink; request at least its current disk size.",
        ));
    }
    let wait = args["waitSeconds"].as_u64().unwrap_or(0);
    if wait
        > super::backups::settings(s).await?["maxCapacityWaitSeconds"]
            .as_u64()
            .unwrap_or(3600)
    {
        return Err(Error::bad("Capacity wait exceeds the configured maximum."));
    }
    if let Some(node) = args["nodeId"].as_str() {
        crate::validation::uuid(node)?;
    }
    if let Some(tags) = args.get("requiredTags") {
        let tags = tags
            .as_array()
            .filter(|tags| tags.len() <= 32)
            .ok_or_else(|| Error::bad("Invalid required tags."))?;
        if tags.iter().any(|tag| {
            tag.as_str().is_none_or(|tag| {
                tag.is_empty()
                    || tag.len() > 40
                    || !tag
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_:./".contains(&b))
            })
        }) {
            return Err(Error::bad("Invalid required tags."));
        }
    }
    let run_id = text(run, "id");
    if run["moveRequest"].is_object() {
        return Err(Error::new(409, "A movement is already pending."));
    }
    let checkpoint = s
        .store
        .kv(&format!("run-checkpoint:{run_id}"))
        .await?
        .unwrap_or_default();
    let deadline =
        (now() + wait as i64 * 1000).min(checkpoint["deadline"].as_i64().unwrap_or(i64::MAX));
    let reservation = id();
    let mut selecting = run.clone();
    selecting["placementTransition"] = true.into();
    selecting["requestedResources"] = json!(resources);
    selecting["targetNodeId"] = args["nodeId"].clone();
    selecting["requiredTags"] = args["requiredTags"].clone();
    let selected = loop {
        let current = s.store.run(run_id).await?;
        if !current["cancelRequestedAt"].is_null() || current["status"] != "running" {
            s.store
                .patch_run(run_id, json!({"capacityWaitUntil":null}))
                .await?;
            return Err(Error::new(409, "Conversation is no longer active."));
        }
        match super::placement::reserve(s, &selecting, &reservation).await {
            Ok(value) => break value,
            Err(error) if error.status == 503 && now() < deadline => {
                s.store
                    .patch_run(run_id, json!({"capacityWaitUntil":deadline}))
                    .await?;
                tokio::select! {_=s.shutdown.cancelled()=>return Err(Error::new(503,"Master is stopping.")),_=tokio::time::sleep(Duration::from_secs(1))=>{}}
            }
            Err(error) => {
                s.store
                    .patch_run(run_id, json!({"capacityWaitUntil":null}))
                    .await?;
                return Err(error);
            }
        }
    };
    s.store.patch_run(run_id,json!({"capacityWaitUntil":null,"moveRequest":{"reservation":reservation,"nodeId":selected["nodeId"],"resources":selected["resources"],"requestedAt":now(),"automatic":false},"nodeState":"pausing"})).await?;
    s.store
        .event(
            run_id,
            "status",
            "Pausing to move the complete conversation environment",
            None,
        )
        .await?;
    let (service, run_id) = (s.clone(), run_id.to_owned());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if let Ok(run) = service.store.run(&run_id).await {
            let _ = stop(&service, &run).await;
        }
    });
    Ok(json!({"status":"moving","nodeId":selected["nodeId"],"resources":selected["resources"]}))
}
async fn stop(s: &Service, run: &Value) -> Result<()> {
    let checkpoint = s
        .store
        .kv(&format!("run-checkpoint:{}", text(run, "id")))
        .await?
        .unwrap_or_default();
    let attempt = text(&checkpoint, "runnerId");
    crate::validation::uuid(attempt)?;
    let response = s
        .http
        .delete(format!(
            "{}/runs/{attempt}",
            super::transport::url(s, text(run, "id")).await?
        ))
        .bearer_auth(crate::execution::secret(&s.config.data_dir, "runner-secret").await?)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| Error::new(503, "Waiting for source node to pause."))?;
    if !response.status().is_success() {
        return Err(Error::new(503, "Waiting for source VM to stop."));
    }
    Ok(())
}
pub async fn advance(s: &Service, run: &Value) -> Result<bool> {
    let run_id = text(run, "id");
    if !run["cancelRequestedAt"].is_null() {
        release_destination(s, run).await?;
        return Ok(true);
    }
    let mut current = run.clone();
    if !current["moveRequest"].is_object() {
        let checkpoint = s
            .store
            .kv(&format!("run-checkpoint:{run_id}"))
            .await?
            .unwrap_or_default();
        let source = text(&checkpoint, "nodeId");
        if source.is_empty() || source == super::LOCAL_NODE_ID {
            return Ok(true);
        }
        let node = s.get("nodes", source).await?;
        if node["lastSeen"]
            .as_i64()
            .is_some_and(|seen| now() - seen < 60000)
            && node["revoked"] != true
        {
            return Ok(true);
        }
        if current["pinnedNodeId"].is_string() {
            return Ok(false);
        }
        let Some(backup) = latest(s, run_id).await? else {
            s.store.patch_run(run_id,json!({"nodeState":"waiting-for-node","accountWaitReason":"Original node unavailable; no usable recovery point exists."})).await?;
            return Ok(false);
        };
        let mut selecting = current.clone();
        selecting["placementTransition"] = true.into();
        selecting["requiredRuntime"] =
            super::backups::manifest(s, &backup).await?["runtime"]["runtimeId"].clone();
        let reservation = id();
        let selected = match super::placement::reserve(s, &selecting, &reservation).await {
            Ok(value) => value,
            Err(_) => return Ok(false),
        };
        current["moveRequest"] = json!({"reservation":reservation,"nodeId":selected["nodeId"],"resources":selected["resources"],"backupId":backup["id"],"automatic":true});
        s.store
            .patch_run(run_id, json!({"moveRequest":current["moveRequest"]}))
            .await?;
    }
    crate::recovery::fence(s, &current).await?;
    let movement = &current["moveRequest"];
    let transfer = async {
        let backup = if let Some(id) = movement["backupId"].as_str() {
            s.get("node-backups", id).await?
        } else {
            s.store
                .patch_run(run_id, json!({"nodeState":"saving"}))
                .await?;
            let point = super::backups::capture(s, &current).await?;
            s.get("node-backups", text(&point, "id")).await?
        };
        s.store
            .patch_run(run_id, json!({"nodeState":"restoring"}))
            .await?;
        super::placement::materialize(s, text(movement, "reservation")).await?;
        let source = s
            .store
            .kv(&format!("run-checkpoint:{run_id}"))
            .await?
            .unwrap_or_default();
        if source["nodeId"] != movement["nodeId"] || movement["automatic"] == true {
            super::restore::start(s, &current, text(movement, "nodeId"), &backup).await?;
        }
        Ok::<_, Error>(backup)
    }
    .await;
    let backup = match transfer {
        Ok(backup) => backup,
        Err(error) => {
            release_destination(s, &current).await?;
            s.store.patch_run(run_id,json!({"moveRequest":null,"moveReservation":null,"nodeState":"waiting-for-node","movementError":error.message,"recoveryPending":true})).await?;
            s.store.event(run_id,"status","Environment transfer failed. The original disk is retained; recovery will retry from its last owner.",None).await?;
            return Ok(false);
        }
    };
    let (run_id, node, reservation, resources, session, captured_at) = (
        run_id.to_owned(),
        movement["nodeId"].clone(),
        movement["reservation"].clone(),
        movement["resources"].clone(),
        backup["sessionId"].clone(),
        backup["capturedAt"].clone(),
    );
    s.store.transaction(move |db| {
        let key=format!("run-checkpoint:{run_id}");let mut checkpoint=db.kv(&key)?.ok_or_else(||Error::new(409,"Missing resume checkpoint."))?;
        let current=db.run(&run_id)?.ok_or_else(||Error::new(404,"Conversation removed."))?;
        if !current["cancelRequestedAt"].is_null() {return Err(Error::new(409,"Conversation cancelled during restore."));}
        checkpoint["nodeId"]=node.clone();checkpoint["process"]=Value::Null;checkpoint["settled"]=Value::Null;checkpoint["controllerRecoveries"]=0.into();
        db.set(&key,&checkpoint,None)?;
        db.patch_run(&run_id,&json!({"nodeId":node,"nodeState":"resuming","requestedResources":resources,"moveRequest":null,"moveReservation":reservation,"sessionId":session,"restoredAt":captured_at,"recoveryPending":true,"status":"queued"}))?;
        db.event(&run_id,"status","Restoring conversation from a dated recovery point; newer chat remains visible. Verify external effects before repeating actions.",Some(&json!({"capturedAt":captured_at})))?;
        Ok(())
    }).await?;
    Ok(false)
}
pub async fn latest(s: &Service, run: &str) -> Result<Option<Value>> {
    Ok(s.store
        .list("node-backups")
        .await?
        .into_iter()
        .filter(|b| b["runId"] == run)
        .max_by_key(|b| b["capturedAt"].as_i64().unwrap_or(0)))
}

async fn release_destination(s: &Service, run: &Value) -> Result<()> {
    for reservation in [
        run["moveRequest"]["reservation"].as_str(),
        run["moveReservation"].as_str(),
    ]
    .into_iter()
    .flatten()
    {
        super::placement::release(s, reservation).await?;
    }
    Ok(())
}
