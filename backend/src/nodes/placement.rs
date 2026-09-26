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
        let resources=run.get("requestedResources").cloned().unwrap_or_else(||json!(defaults()));
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

        for node in nodes {
            let id=text(&node,"id");
            if node["maintenance"].is_string() {continue;}
            if node["local"] == true && !configured && run["isolated"] == true {continue;}
            if run["requiredTags"].as_array().into_iter().flatten().any(|tag|!node["tags"].as_array().into_iter().flatten().chain(node["systemTags"].as_array().into_iter().flatten()).any(|present|present==tag)) {continue;}

            if required.is_some_and(|runtime|!node["runtimes"].as_array().into_iter().flatten().any(|r|r==runtime)) {continue;}
            if target.is_some_and(|target|target!=id) || !allowed(&access["nodes"],id) || pinned.is_some_and(|p|p!=id) || existing.is_some_and(|p|p!=id) || node["revoked"]==true || node["accepting"]!=true {continue;}
            if (node["local"]!=true || configured) && (node["capabilities"]["kvm"]!=true || node["executionReady"]!=true || node["lastSeen"].as_i64().is_none_or(|v|now()-v>=30000)) {continue;}
            let fits=["cpu","memoryMiB","diskMiB"].iter().all(|key| {
                let used=if *key=="diskMiB" {volumes.iter().filter(|v|v["nodeId"]==id && v["runId"]!=run["id"]).map(|v|v["diskMiB"].as_u64().unwrap_or(0)).sum::<u64>()} else {attempts.iter().filter(|a|a["nodeId"]==id && a["released"]!=true && !(moving && a["runId"]==run["id"])).map(|a|a["resources"][key].as_u64().unwrap_or(0)).sum::<u64>()};
                let current=if *key=="diskMiB" {volumes.iter().filter(|v|v["nodeId"]==id && v["runId"]==run["id"]).map(|v|v["diskMiB"].as_u64().unwrap_or(0)).max().unwrap_or(0)} else if moving {attempts.iter().filter(|a|a["nodeId"]==id && a["runId"]==run["id"] && a["released"]!=true).map(|a|a["resources"][key].as_u64().unwrap_or(0)).max().unwrap_or(0)} else {0};
                used.checked_add(if *key=="diskMiB" && moving && checkpoint["nodeId"]!=id {current.saturating_add(resources[key].as_u64().unwrap_or(u64::MAX))} else {current.max(resources[key].as_u64().unwrap_or(u64::MAX))}).is_some_and(|total|total<=node["limits"][key].as_u64().unwrap_or(0))
            });
            if !fits {continue;}
            if attempts.iter().any(|a|a["runId"]==run["id"] && a["released"]!=true && (!moving || a["role"]=="destination")) {return Err(Error::new(409,"Previous execution still owns this conversation."));}
            let record=json!({"id":attempt,"role":if moving {"destination"} else {"execution"},"runId":run["id"],"nodeId":id,"resources":resources,"runtimeId":required.unwrap_or_else(||text(&node,"runtimeId")),"createdAt":now(),"leaseExpiresAt":now()+60000,"released":false});
            db.put("node-attempts",&record)?;
            let volume_id=format!("{}:{id}",text(&run,"id"));
            let mut volume=db.get("node-volumes",&volume_id)?.unwrap_or_else(||json!({"id":volume_id,"nodeId":id,"runId":run["id"],"materialized":false}));
            let previous=volume["diskMiB"].as_u64().unwrap_or(0);let requested=resources["diskMiB"].as_u64().unwrap_or(0);
            volume["diskMiB"]=(if moving && checkpoint["nodeId"]!=id {previous.saturating_add(requested)} else {previous.max(requested)}).into();
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
            if let Some(mut record) = db.get("node-attempts", &attempt)? {
                record["released"] = true.into();
                record["releasedAt"] = now().into();
                db.put("node-attempts", &record)?;
                let volume_id = format!("{}:{}", text(&record, "runId"), text(&record, "nodeId"));
                if let Some(volume) = db.get("node-volumes", &volume_id)?
                    && volume["materialized"] != true
                    && !db.list("node-attempts")?.iter().any(|a| {
                        a["runId"] == record["runId"]
                            && a["nodeId"] == record["nodeId"]
                            && a["released"] != true
                    })
                {
                    db.remove("node-volumes", &volume_id)?;
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
            let record = db
                .get("node-attempts", &attempt)?
                .ok_or_else(|| Error::new(409, "Missing disk allocation."))?;
            let id = format!("{}:{}", text(&record, "runId"), text(&record, "nodeId"));
            let mut volume = db
                .get("node-volumes", &id)?
                .ok_or_else(|| Error::new(409, "Missing disk allocation."))?;
            volume["materialized"] = true.into();
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
