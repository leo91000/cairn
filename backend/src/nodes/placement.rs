//! Atomic admission and durable attempt ownership. Failed admission changes nothing.
use super::{LOCAL_NODE_ID, Resources};
use crate::{
    config::now,
    error::{Error, Result},
    service::{Service, allowed, policy},
    validation::text,
};
use serde_json::{Value, json};
pub fn defaults() -> Resources {
    Resources {
        cpu: 2,
        memory_mi_b: 4096,
        disk_mi_b: 32768,
    }
}
fn disk_total(volume: &Value, requested: u64, replacing: bool) -> u64 {
    let total = volume["diskMiB"].as_u64().unwrap_or(0);
    let current = volume["activeDiskMiB"].as_u64().unwrap_or(total);
    total.saturating_add(if replacing {
        requested
    } else {
        requested.saturating_sub(current)
    })
}
/// Select within current grants and preserve the location of an existing environment.
pub async fn reserve(s: &Service, run: &Value, attempt: &str) -> Result<Value> {
    let _ = super::refresh_local(s).await;
    let run = run.clone();
    let attempt = attempt.to_owned();
    let configured = !s.config.runner_url.is_empty();
    s.store.transaction(move |db| {
        let agent=db.get("agents",text(&run["snapshot"]["agent"],"id"))?.ok_or_else(||Error::new(403,"Agent removed."))?;
        let access=policy(&agent);
        let checkpoint=db.kv(&format!("run-checkpoint:{}",text(&run,"id")))?.unwrap_or_default();
        let requested=run.get("requestedResources").filter(|v|v.is_object()).or_else(||run.get("resources").filter(|v|v.is_object()));
        let resources=requested.cloned().unwrap_or_else(||json!(defaults()));
        let resources:Resources=serde_json::from_value(resources).map_err(|_|Error::bad("Invalid execution resources."))?;
        resources.validate()?;
        let resources=json!(resources);
        let pinned=run["pinnedNodeId"].as_str();
        let moving=run["placementTransition"]==true;
        let existing=if moving {None} else {checkpoint["nodeId"].as_str()};
        let target=run["targetNodeId"].as_str();
        let required=run["requiredRuntime"].as_str().or(checkpoint["runtimeId"].as_str());
        let mut nodes=db.list("nodes")?;
        // Existing development execution stays available without a controller.
        if !configured && nodes.iter().all(|n|n["id"]!=LOCAL_NODE_ID) { nodes.push(json!({"id":LOCAL_NODE_ID,"local":true,"accepting":true,"limits":{"cpu":4096,"memoryMiB":1073741824u64,"diskMiB":1099511627776u64},"capabilities":{"kvm":configured},"runtimeId":"local"})); }
        // Ties keep the preferred node, then the current runner.
        nodes.sort_by_key(|node| (node["id"]!=run["preferredNodeId"],node["id"]!=LOCAL_NODE_ID));
        let attempts=db.list("node-attempts")?;
        let volumes=db.list("node-volumes")?;
        if !moving && let Some(reservation)=run["moveReservation"].as_str() {
            let mut held=db.get("node-attempts",reservation)?.ok_or_else(||Error::new(409,"Movement reservation is missing."))?;
            let node=text(&held,"nodeId");
            let record=nodes.iter().find(|n|n["id"]==node).ok_or_else(||Error::new(409,"Destination removed."))?;
            if record["maintenance"].is_string() {return Err(Error::new(503,"Destination is preparing for maintenance."));}
            if held["released"]==true || held["runId"]!=run["id"] || !allowed(&access["nodes"],node) || record["revoked"]==true {return Err(Error::new(409,"Movement reservation was revoked."));}
            let mut consumed=held.clone();consumed["released"]=true.into();db.put("node-attempts",&consumed)?;
            held["id"]=attempt.clone().into();held["role"]="execution".into();held["leaseExpiresAt"]=(now()+60000).into();db.put("node-attempts",&held)?;
            db.patch_run(text(&run,"id"),&json!({"moveReservation":null}))?;
            return Ok(json!({"nodeId":held["nodeId"],"resources":held["resources"],"runtimeId":held["runtimeId"]}));
        }

        let mut best:Option<(bool,f64,Value,Value)>=None;
        for node in nodes {
            let id=text(&node,"id");
            let mut resources=resources.clone();
            if requested.is_none() {
                for key in ["cpu","memoryMiB","diskMiB"] {resources[key]=resources[key].as_u64().unwrap().min(node["limits"][key].as_u64().unwrap_or(0)).into();}
                if resources["cpu"].as_u64().unwrap()<1 || resources["memoryMiB"].as_u64().unwrap()<128 || resources["diskMiB"].as_u64().unwrap()<128 {continue;}
            }
            if node["maintenance"].is_string() {continue;}
            if node["local"] == true && !configured && run["isolated"] == true {continue;}
            if run["requiredTags"].as_array().into_iter().flatten().any(|tag|!node["tags"].as_array().into_iter().flatten().chain(node["systemTags"].as_array().into_iter().flatten()).any(|present|present==tag)) {continue;}

            if required.is_some_and(|runtime|!node["runtimes"].as_array().into_iter().flatten().any(|r|r==runtime)) {continue;}
            if target.is_some_and(|target|target!=id) || !allowed(&access["nodes"],id) || pinned.is_some_and(|p|p!=id) || existing.is_some_and(|p|p!=id) || node["revoked"]==true || node["accepting"]!=true {continue;}
            if (node["local"]!=true || configured) && (node["capabilities"]["kvm"]!=true || node["executionReady"]!=true || node["lastSeen"].as_i64().is_none_or(|v|now()-v>=30000)) {continue;}
            let mut headroom=f64::MAX;
            let fits=["cpu","memoryMiB","diskMiB"].iter().all(|key| {
                let used=if *key=="diskMiB" {volumes.iter().filter(|v|v["nodeId"]==id && v["runId"]!=run["id"]).map(|v|v["diskMiB"].as_u64().unwrap_or(0)).sum::<u64>()} else {attempts.iter().filter(|a|a["nodeId"]==id && a["released"]!=true && !(moving && a["runId"]==run["id"])).map(|a|a["resources"][key].as_u64().unwrap_or(0)).sum::<u64>()};
                let current=if *key=="diskMiB" {volumes.iter().filter(|v|v["nodeId"]==id && v["runId"]==run["id"]).map(|v|v["diskMiB"].as_u64().unwrap_or(0)).max().unwrap_or(0)} else if moving {attempts.iter().filter(|a|a["nodeId"]==id && a["runId"]==run["id"] && a["released"]!=true).map(|a|a["resources"][key].as_u64().unwrap_or(0)).max().unwrap_or(0)} else {0};
                used.checked_add(if *key=="diskMiB" {disk_total(volumes.iter().find(|v|v["nodeId"]==id && v["runId"]==run["id"]).unwrap_or(&Value::Null),resources[key].as_u64().unwrap_or(u64::MAX),moving && checkpoint["nodeId"]!=id)} else {current.max(resources[key].as_u64().unwrap_or(u64::MAX))}).is_some_and(|total| {
                    let limit=node["limits"][key].as_u64().unwrap_or(0);
                    if *key!="diskMiB" && limit>0 {headroom=headroom.min((limit.saturating_sub(total)) as f64/limit as f64);}
                    total<=limit
                })
            });
            if !fits {continue;}
            // Automatic placement spreads work: a preferred node wins, otherwise the most CPU/RAM headroom.
            let preferred=node["id"]==run["preferredNodeId"];
            if best.as_ref().is_none_or(|(p,h,_,_):&(bool,f64,Value,Value)|preferred && !*p || preferred==*p && headroom>*h) {best=Some((preferred,headroom,node,resources));}
        }
        if let Some((_,_,node,resources))=best {
            let id=text(&node,"id");
            if attempts.iter().any(|a|a["runId"]==run["id"] && a["released"]!=true && (!moving || a["role"]=="destination")) {return Err(Error::new(409,"Previous execution still owns this conversation."));}
            let mut record=json!({"id":attempt,"role":if moving {"destination"} else {"execution"},"runId":run["id"],"nodeId":id,"resources":resources,"runtimeId":required.unwrap_or_else(||text(&node,"runtimeId")),"createdAt":now(),"leaseExpiresAt":now()+60000,"released":false});
            let volume_id=format!("{}:{id}",text(&run,"id"));
            let mut volume=db.get("node-volumes",&volume_id)?.unwrap_or_else(||json!({"id":volume_id,"nodeId":id,"runId":run["id"],"materialized":false}));
            let previous=volume["diskMiB"].as_u64().unwrap_or(0);let requested=resources["diskMiB"].as_u64().unwrap_or(0);
            volume["diskMiB"]=disk_total(&volume,requested,moving && checkpoint["nodeId"]!=id).into();
            record["additionalDiskMiB"]=volume["diskMiB"].as_u64().unwrap().saturating_sub(previous).into();
            record["diskMaterialized"]=false.into();
            db.put("node-attempts",&record)?;
            db.put("node-volumes",&volume)?;
            return Ok(json!({"nodeId":id,"resources":resources,"runtimeId":required.unwrap_or_else(||text(&node,"runtimeId")),"leaseExpiresAt":now()+60000}));
        }
        Err(Error::new(503,"No authorized node has the required capacity. The existing environment is preserved."))
    }).await
}
pub async fn release(s: &Service, attempt: &str) -> Result<()> {
    let attempt = attempt.to_owned();
    s.store
        .transaction(move |db| {
            if let Some(mut record) = db.get("node-attempts", &attempt)?
                && record["released"] != true
            {
                record["released"] = true.into();
                record["releasedAt"] = now().into();
                db.put("node-attempts", &record)?;
                let volume_id = format!("{}:{}", text(&record, "runId"), text(&record, "nodeId"));
                if let Some(mut volume) = db.get("node-volumes", &volume_id)? {
                    if record["diskMaterialized"] != true {
                        volume["diskMiB"] = volume["diskMiB"]
                            .as_u64()
                            .unwrap_or(0)
                            .saturating_sub(record["additionalDiskMiB"].as_u64().unwrap_or(0))
                            .into();
                    }
                    if volume["materialized"] != true
                        && !db.list("node-attempts")?.iter().any(|a| {
                            a["runId"] == record["runId"]
                                && a["nodeId"] == record["nodeId"]
                                && a["released"] != true
                        })
                    {
                        db.remove("node-volumes", &volume_id)?;
                    } else {
                        db.put("node-volumes", &volume)?;
                    }
                }
            }
            Ok(())
        })
        .await
}

/// Once a controller may have created a disk, keep its allocation until explicit disk deletion.
pub async fn materialize(s: &Service, attempt: &str) -> Result<()> {
    let attempt = attempt.to_owned();
    s.store
        .transaction(move |db| {
            let mut record = db
                .get("node-attempts", &attempt)?
                .ok_or_else(|| Error::new(409, "Missing disk allocation."))?;
            if record["released"] == true {
                return Err(Error::new(409, "Disk reservation was released."));
            }
            record["diskMaterialized"] = true.into();
            db.put("node-attempts", &record)?;
            let id = format!("{}:{}", text(&record, "runId"), text(&record, "nodeId"));
            let mut volume = db
                .get("node-volumes", &id)?
                .ok_or_else(|| Error::new(409, "Missing disk allocation."))?;
            volume["materialized"] = true.into();
            volume["activeDiskMiB"] = record["resources"]["diskMiB"].clone();
            db.put("node-volumes", &volume)?;
            Ok(())
        })
        .await
}

/// Owner controls still obey the selected agent's current node grants.
pub async fn configure(s: &Service, run: &str, input: Option<Value>) -> Result<Value> {
    crate::validation::uuid(run)?;
    let run = run.to_owned();
    s.store.transaction(move |db| {
        let mut record = db.run(&run)?.ok_or_else(|| Error::new(404,"Conversation not found."))?;
        let agent = db.get("agents", text(&record["snapshot"]["agent"], "id"))?.ok_or_else(|| Error::new(403,"Agent removed."))?;
        let access = policy(&agent);
        if let Some(input) = input {
            #[derive(serde::Deserialize)]
            #[serde(rename_all="camelCase",deny_unknown_fields)]
            struct Selection { pinned_node_id: Option<String>, preferred_node_id: Option<String> }
            let selection: Selection = serde_json::from_value(input).map_err(|_| Error::bad("Invalid placement selection."))?;
            for node in [&selection.pinned_node_id, &selection.preferred_node_id].into_iter().flatten() {
                crate::validation::uuid(node)?;
                if !allowed(&access["nodes"],node) || (node!=LOCAL_NODE_ID && db.get("nodes",node)?.is_none_or(|n|n["revoked"]==true)) {return Err(Error::new(403,"This node is not authorized for the conversation's agent."));}
            }
            if record["moveRequest"].is_object() || record["moveReservation"].is_string() {return Err(Error::new(409,"Wait for the current movement to finish."));}
            record=db.patch_run(&run,&json!({"pinnedNodeId":selection.pinned_node_id,"preferredNodeId":selection.preferred_node_id}))?;
            db.audit("node.placement.configured",&json!({"runId":run,"pinnedNodeId":record["pinnedNodeId"],"preferredNodeId":record["preferredNodeId"]}))?;
        }
        let nodes=db.list("nodes")?.into_iter().filter(|n|n["revoked"]!=true && allowed(&access["nodes"],text(n,"id"))).collect::<Vec<_>>();
        Ok(json!({"nodes":nodes,"pinnedNodeId":record["pinnedNodeId"],"preferredNodeId":record["preferredNodeId"]}))
    }).await
}

/// The local controller follows the same expiring ownership rule as remote nodes.
pub async fn renew_local(s: &Service, run_id: &str) -> Result<()> {
    let checkpoint = s
        .store
        .kv(&format!("run-checkpoint:{run_id}"))
        .await?
        .unwrap_or_default();
    if checkpoint["nodeId"] != LOCAL_NODE_ID {
        return Ok(());
    }
    let Some(attempt) = checkpoint["runnerId"].as_str() else {
        return Ok(());
    };
    let lease_ms = super::backups::settings(s).await?["disconnectTimeoutSeconds"]
        .as_u64()
        .unwrap_or(60)
        * 1000;
    let owned = attempt.to_owned();
    let run_id = run_id.to_owned();
    s.store
        .transaction(move |db| {
            let run = db
                .run(&run_id)?
                .ok_or_else(|| Error::new(404, "Conversation removed."))?;
            let agent = db
                .get("agents", text(&run["snapshot"]["agent"], "id"))?
                .unwrap_or_default();
            let mut record = db
                .get("node-attempts", &owned)?
                .ok_or_else(|| Error::new(409, "Attempt removed."))?;
            if run["status"] != "running"
                || !run["cancelRequestedAt"].is_null()
                || record["released"] == true
                || !allowed(&policy(&agent)["nodes"], LOCAL_NODE_ID)
            {
                return Err(Error::new(409, "Execution no longer authorized."));
            }
            record["leaseRequired"] = true.into();
            record["leaseDurationMs"] = record["leaseDurationMs"]
                .as_u64()
                .unwrap_or(0)
                .max(lease_ms)
                .into();
            db.put("node-attempts", &record)?;
            Ok(())
        })
        .await?;
    // Record an upper bound even if the acknowledgement is lost.
    super::record_lease(s, attempt, lease_ms + 3000).await;
    let response = s
        .http
        .post(format!("{}/runs/{attempt}/lease", s.config.runner_url))
        .bearer_auth(crate::execution::secret(&s.config.data_dir, "runner-secret").await?)
        .json(&json!({"remainingMs":lease_ms}))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .map_err(|_| Error::new(503, "Local execution lease unavailable."))?;
    if !response.status().is_success() {
        return Err(Error::new(
            503,
            "Local controller rejected execution lease.",
        ));
    }
    Ok(())
}
