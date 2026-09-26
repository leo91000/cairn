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
    json!({"name":"request_capacity","description":"Request CPU/RAM/disk for this conversation on an authorized node; call list_nodes first to see node ids, tags and available capacity. Failure leaves the current conversation running. A successful request schedules pause, full environment transfer and resume. Optional waitSeconds respects the configured limit (one hour by default); GPU is not supported.","inputSchema":{"type":"object","properties":{"cpu":{"type":"integer","minimum":1},"memoryMiB":{"type":"integer","minimum":128},"diskMiB":{"type":"integer","minimum":128},"requiredTags":{"type":"array","maxItems":32,"items":{"type":"string","maxLength":40}},"nodeId":{"type":"string","format":"uuid"},"waitSeconds":{"type":"integer","minimum":0}},"required":["cpu","memoryMiB","diskMiB"],"additionalProperties":false}})
}
pub fn list_tool() -> Value {
    json!({"name":"list_nodes","description":"List the execution nodes this conversation's agent may use, with their tags and currently available CPU/RAM/disk. Use the returned ids and tags with request_capacity.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}})
}
/// Only authorized, non-revoked nodes; never exposes other agents or node credentials.
pub async fn list(s: &Service, run: &Value) -> Result<Value> {
    let agent = s
        .store
        .get("agents", text(&run["snapshot"]["agent"], "id"))
        .await?
        .ok_or_else(|| Error::new(403, "Agent removed."))?;
    let scope = crate::service::policy(&agent)["nodes"].clone();
    let current = run["nodeId"].clone();
    let nodes = super::inventory(s)
        .await?
        .into_iter()
        .filter(|node| node["revoked"] != true && crate::service::allowed(&scope, text(node, "id")))
        .map(|node| {
            let tags = node["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(node["systemTags"].as_array().into_iter().flatten())
                .cloned()
                .collect::<Vec<_>>();
            json!({"id":node["id"],"name":node["name"],"tags":tags,"status":node["status"],"acceptingWork":node["accepting"],"available":node["available"],"limits":node["limits"],"current":node["id"]==current})
        })
        .collect::<Vec<_>>();
    Ok(json!({"nodes":nodes,"currentNodeId":current,"currentResources":run["resources"]}))
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
    let idle = run["status"] == "succeeded"
        && run["sessionId"].is_string()
        && checkpoint["prepared"]["backend"] == "firecracker";
    let reservation = id();
    let mut selecting = run.clone();
    selecting["placementTransition"] = true.into();
    selecting["requestedResources"] = json!(resources);
    selecting["targetNodeId"] = args["nodeId"].clone();
    selecting["requiredTags"] = args
        .get("requiredTags")
        .unwrap_or(&run["requiredTags"])
        .clone();
    let selected = loop {
        let current = s.store.run(run_id).await?;
        if !current["cancelRequestedAt"].is_null()
            || (current["status"] != "running" && !(idle && current["status"] == "succeeded"))
        {
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
                tokio::select! {_=s.shutdown.cancelled()=>{s.store.patch_run(run_id,json!({"capacityWaitUntil":null})).await?;return Err(Error::new(503,"Master is stopping."));},_=tokio::time::sleep(Duration::from_secs(1))=>{}}
            }
            Err(error) => {
                s.store
                    .patch_run(run_id, json!({"capacityWaitUntil":null}))
                    .await?;
                return Err(error);
            }
        }
    };
    let required_tags = selecting["requiredTags"].clone();
    let owner = run_id.to_owned();
    let held = reservation.clone();
    let selected_node = selected.clone();
    let recorded=s.store.transaction(move |db| {
        let current=db.run(&owner)?.ok_or_else(||Error::new(404,"Conversation removed."))?;
        if current["moveRequest"].is_object() || !current["cancelRequestedAt"].is_null() || current["status"]!=if idle {"succeeded"}else{"running"} {
            return Err(Error::new(409,"Conversation changed while reserving capacity."));
        }
        let mut patch=json!({"movementError":null,"capacityWaitUntil":null,"moveRequest":{"reservation":held,"nodeId":selected_node["nodeId"],"resources":selected_node["resources"],"requestedAt":now(),"automatic":false,"idle":idle,"requiredTags":required_tags},"nodeState":"pausing"});
        if idle {patch["status"]="queued".into();patch["recoveryPending"]=true.into();}
        db.patch_run(&owner,&patch)?;
        Ok(())
    }).await;
    if let Err(error) = recorded {
        super::placement::release(s, &reservation).await?;
        return Err(error);
    }
    s.store
        .event(
            run_id,
            "status",
            "Pausing to move the complete conversation environment",
            None,
        )
        .await?;
    Ok(json!({"status":"moving","nodeId":selected["nodeId"],"resources":selected["resources"]}))
}
/// Retried by the worker heartbeat until the active execution exits.
pub async fn pause_pending(s: &Service, run_id: &str) -> Result<()> {
    let run = s.store.run(run_id).await?;
    if run["moveRequest"].is_object()
        && run["moveRequest"]["idle"] != true
        && run["moveRequest"]["requestedAt"]
            .as_i64()
            .is_some_and(|at| now() - at >= 2000)
    {
        stop(s, &run).await?;
    }
    Ok(())
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
        if source.is_empty() {
            return Ok(true);
        }
        if source == super::LOCAL_NODE_ID {
            let _ = super::refresh_local(s).await;
        }
        let node = s.get("nodes", source).await?;
        let agent = s
            .get("agents", text(&current["snapshot"]["agent"], "id"))
            .await?;
        if node["lastSeen"]
            .as_i64()
            .is_some_and(|seen| now() - seen < 60000)
            && node["revoked"] != true
            && node["executionReady"] == true
            && crate::service::allowed(&crate::service::policy(&agent)["nodes"], source)
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
            let idle = movement["idle"] == true;
            let mut patch = json!({"moveRequest":null,"moveReservation":null,"nodeState":if idle {Value::Null}else{json!("waiting-for-node")},"movementError":error.message,"recoveryPending":!idle});
            if idle {
                patch["status"] = "succeeded".into();
            }
            let owner = run_id.to_owned();
            s.store
                .transaction(move |db| {
                    let current = db
                        .run(&owner)?
                        .ok_or_else(|| Error::new(404, "Conversation removed."))?;
                    if !current["cancelRequestedAt"].is_null() {
                        patch["status"] = "cancelled".into();
                        patch["recoveryPending"] = false.into();
                        patch["nodeState"] = Value::Null;
                    }
                    db.patch_run(&owner, &patch)?;
                    Ok(())
                })
                .await?;
            s.store
                .event(
                    run_id,
                    "status",
                    "Environment transfer failed. The original disk is retained.",
                    None,
                )
                .await?;
            return Ok(false);
        }
    };
    let idle = movement["idle"] == true;
    let required_tags = movement
        .get("requiredTags")
        .unwrap_or(&current["requiredTags"])
        .clone();
    if idle {
        super::placement::release(s, text(movement, "reservation")).await?;
    }
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
        db.patch_run(&run_id,&json!({"movementError":null,"accountWaitReason":null,"nodeId":node,"nodeState":if idle {Value::Null}else{json!("resuming")},"requestedResources":resources,"resources":resources,"requiredTags":required_tags,"moveRequest":null,"moveReservation":if idle {Value::Null}else{reservation},"sessionId":session,"restoredAt":captured_at,"recoveryPending":!idle,"status":if idle {"succeeded"}else{"queued"}}))?;
        db.event(&run_id,"status","Restoring conversation from a dated recovery point; newer chat remains visible. Verify external effects before repeating actions.",Some(&json!({"capturedAt":captured_at})))?;
        Ok(())
    }).await?;
    Ok(false)
}
pub async fn latest(s: &Service, run: &str) -> Result<Option<Value>> {
    let _operation = s.node_backup_operation.lock().await;
    let mut points = s
        .store
        .list("node-backups")
        .await?
        .into_iter()
        .filter(|b| b["runId"] == run)
        .collect::<Vec<_>>();
    points.sort_by_key(|b| std::cmp::Reverse(b["capturedAt"].as_i64().unwrap_or(0)));
    for point in points {
        let usable = async {
            let manifest = super::backups::manifest(s, &point).await?;
            let mut checked = std::collections::HashSet::new();
            for block in manifest["blocks"].as_array().unwrap() {
                if let Some(hash) = block["hash"].as_str()
                    && checked.insert(hash.to_owned())
                {
                    let bytes = super::backups::read_block(s, &point, hash).await?;
                    if bytes.len() as u64 != block["size"].as_u64().unwrap_or(0) {
                        return Err(Error::bad("Backup block size mismatch."));
                    }
                }
            }
            Ok::<_, Error>(())
        }
        .await;
        if usable.is_ok() {
            return Ok(Some(point));
        }
    }
    Ok(None)
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
