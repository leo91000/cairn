//! Durable pause/copy/resume coordination. Never changes a destination before fencing.
use super::NodeState;
use crate::{
    config::{id, now},
    error::{Error, Result},
    run_status::RunStatus,
    service::Service,
    validation::text,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

const MOVE_DESCRIPTION: &str = "Move this conversation to an authorized node. Call list_nodes first to see ids, tags, shared budgets and free slots. Failure leaves the current conversation running. Success saves, pauses and resumes on the destination, interrupting running commands. Optional waitSeconds respects the configured wait limit.";
const LIST_NODES_DESCRIPTION: &str = "List authorized execution nodes with their tags, shared CPU/RAM/disk budgets, slot availability and current resource pressure. Use a returned node id with move_to_node. Resources are shared automatically; agents do not request allocations.";
/// A requested move waits this long for the conversation to settle before pausing it.
const PAUSE_DELAY_MS: i64 = 2000;
const MAX_WAIT_SECONDS: u64 = 3600;

pub fn tool() -> Value {
    json!({
        "name": "move_to_node",
        "description": MOVE_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "requiredTags": {
                    "type": "array",
                    "maxItems": 32,
                    "items": { "type": "string", "maxLength": 40 },
                },
                "nodeId": { "type": "string", "format": "uuid" },
                "waitSeconds": { "type": "integer", "minimum": 0 },
            },
            "required": ["nodeId"],
            "additionalProperties": false,
        },
    })
}

pub fn list_tool() -> Value {
    json!({
        "name": "list_nodes",
        "description": LIST_NODES_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        },
    })
}

/// A node as the agent sees it in `list_nodes`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeSummary<'a> {
    id: &'a Value,
    name: &'a Value,
    tags: Vec<&'a Value>,
    status: &'a Value,
    accepting_work: &'a Value,
    slots: u64,
    available_slots: &'a Value,
    usage: &'a Value,
    pressure: &'a Value,
    limits: &'a Value,
    current: bool,
}

/// A pending move, persisted in `run.moveRequest` until the destination resumes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MoveRequest<'a> {
    reservation: &'a str,
    node_id: &'a Value,
    resources: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_at: Option<i64>,
    /// Recovery from an unavailable node, rather than a requested move.
    automatic: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    idle: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_tags: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_id: Option<&'a Value>,
}

async fn run_agent(s: &Service, run: &Value) -> Result<Value> {
    s.store
        .get("agents", super::run_agent(run))
        .await?
        .ok_or_else(|| Error::forbidden("Agent removed."))
}

/// Only authorized, non-revoked nodes; never exposes other agents or node credentials.
pub async fn list(s: &Service, run: &Value) -> Result<Value> {
    let access = crate::service::policy(&run_agent(s, run).await?);
    let scope = &access["nodes"];
    let current = &run["nodeId"];
    let inventory = super::inventory(s).await?;
    let nodes = inventory
        .iter()
        .filter(|node| node["revoked"] != true && crate::service::allowed(scope, text(node, "id")))
        .map(|node| {
            serde_json::to_value(NodeSummary {
                id: &node["id"],
                name: &node["name"],
                tags: super::node_tags(node).collect(),
                status: &node["status"],
                accepting_work: &node["accepting"],
                slots: super::slots(node),
                available_slots: &node["availableSlots"],
                usage: &node["usage"],
                pressure: &node["pressure"],
                limits: &node["limits"],
                current: node["id"] == *current,
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(json!({
        "nodes": nodes,
        "currentNodeId": current,
    }))
}

/// Owner-initiated move from the web or Android conversation controls.
pub async fn request(s: &Service, run: &Value, args: &Value) -> Result<Value> {
    requested(s, run, args, false).await
}

/// Agents choose a node; the runtime owns all resource sharing.
pub async fn request_by_agent(s: &Service, run: &Value, args: &Value) -> Result<Value> {
    crate::validation::uuid(text(args, "nodeId"))?;
    requested(s, run, args, true).await.map_err(|mut error| {
        if super::placement::is_no_capacity(&error) {
            error.message.push_str(" The current conversation keeps running. Retry move_to_node with waitSeconds or choose another node.");
        }
        error
    })
}

fn validate_destination(args: &Value) -> Result<()> {
    if let Some(node) = args["nodeId"].as_str() {
        crate::validation::uuid(node)?;
    }
    let Some(tags) = args.get("requiredTags") else {
        return Ok(());
    };
    let valid = tags
        .as_array()
        .filter(|tags| tags.len() <= 32)
        .is_some_and(|tags| {
            tags.iter()
                .all(|tag| tag.as_str().is_some_and(super::valid_tag))
        });
    if !valid {
        return Err(Error::bad("Invalid required tags."));
    }
    Ok(())
}

async fn requested(s: &Service, run: &Value, args: &Value, by_agent: bool) -> Result<Value> {
    if ["cpu", "memoryMiB", "diskMiB"]
        .iter()
        .any(|key| args.get(key).is_some())
    {
        return Err(Error::bad(
            "Choose a node; resource requests are no longer supported.",
        ));
    }
    let wait = args["waitSeconds"].as_u64().unwrap_or(0);
    let max_wait = super::publication::settings(s).await?["maxCapacityWaitSeconds"]
        .as_u64()
        .unwrap_or(MAX_WAIT_SECONDS);
    if wait > max_wait {
        return Err(Error::bad("Capacity wait exceeds the configured maximum."));
    }
    validate_destination(args)?;
    let run_id = text(run, "id");
    if run["moveRequest"].is_object() {
        return Err(Error::conflict("A movement is already pending."));
    }
    let checkpoint = super::checkpoint(s, run_id).await?;
    let deadline =
        (now() + wait as i64 * 1000).min(checkpoint["deadline"].as_i64().unwrap_or(i64::MAX));
    // A finished conversation with a VM session moves without being resumed.
    let idle = run["status"] == RunStatus::Succeeded
        && run["sessionId"].is_string()
        && checkpoint["prepared"]["backend"] == "firecracker";
    let reservation = id();
    let mut selecting = run.clone();
    selecting["placementTransition"] = true.into();
    selecting["targetNodeId"] = args["nodeId"].clone();
    selecting["requiredTags"] = args
        .get("requiredTags")
        .unwrap_or(&run["requiredTags"])
        .clone();
    let selected = wait_for_capacity(s, run_id, &selecting, &reservation, idle, deadline).await?;
    let move_request = serde_json::to_value(MoveRequest {
        reservation: &reservation,
        node_id: &selected["nodeId"],
        resources: &selected["resources"],
        requested_at: Some(now()),
        automatic: false,
        idle: Some(idle),
        required_tags: Some(&selecting["requiredTags"]),
        backup_id: None,
    })?;
    let expected = if idle {
        RunStatus::Succeeded
    } else {
        RunStatus::Running
    };
    if let Err(error) = record_request(s, run_id, move_request, idle, expected).await {
        super::placement::release(s, &reservation).await?;
        return Err(error);
    }
    let destination = destination_name(s, selected["nodeId"].as_str()).await?;
    // The owner sees in the conversation that the agent moved itself, and what it asked for.
    let message = if by_agent {
        format!("The agent is moving to {destination}. Running commands are interrupted.")
    } else {
        format!("Moving the conversation to {destination}. Running commands are interrupted.")
    };
    s.store.event(run_id, "status", &message, None).await?;
    Ok(json!({
        "status": "moving",
        "nodeId": selected["nodeId"],
    }))
}

async fn set_capacity_wait(s: &Service, run_id: &str, until: Option<i64>) -> Result<()> {
    s.store
        .patch_run(run_id, json!({ "capacityWaitUntil": until }))
        .await?;
    Ok(())
}

/// Reserves destination capacity, retrying until `deadline` while no node has room.
async fn wait_for_capacity(
    s: &Service,
    run_id: &str,
    selecting: &Value,
    reservation: &str,
    idle: bool,
    deadline: i64,
) -> Result<Value> {
    loop {
        let current = s.store.run(run_id).await?;
        let active = current["cancelRequestedAt"].is_null()
            && (current["status"] == RunStatus::Running
                || (idle && current["status"] == RunStatus::Succeeded));
        if !active {
            set_capacity_wait(s, run_id, None).await?;
            return Err(Error::conflict("Conversation is no longer active."));
        }
        match super::placement::reserve(s, selecting, reservation).await {
            Ok(value) => return Ok(value),
            Err(error) if error.is_unavailable() && now() < deadline => {
                set_capacity_wait(s, run_id, Some(deadline)).await?;
                let stopping = tokio::select! {
                    () = s.shutdown.cancelled() => true,
                    () = tokio::time::sleep(Duration::from_secs(1)) => false,
                };
                if stopping {
                    set_capacity_wait(s, run_id, None).await?;
                    return Err(Error::unavailable("Master is stopping."));
                }
            }
            Err(error) => {
                set_capacity_wait(s, run_id, None).await?;
                return Err(error);
            }
        }
    }
}

/// Records the move unless the conversation changed while capacity was reserved.
async fn record_request(
    s: &Service,
    run_id: &str,
    move_request: Value,
    idle: bool,
    expected: RunStatus,
) -> Result<()> {
    let owner = run_id.to_owned();
    s.store
        .transaction(move |db| {
            let current = db
                .run(&owner)?
                .ok_or_else(|| Error::not_found("Conversation removed."))?;
            if current["moveRequest"].is_object()
                || !current["cancelRequestedAt"].is_null()
                || current["status"] != expected
                || (expected == RunStatus::Queued && current["pinnedNodeId"].is_string())
            {
                return Err(Error::conflict(
                    "Conversation changed while reserving capacity.",
                ));
            }
            let mut patch = json!({
                "movementError": null,
                "capacityWaitUntil": null,
                "moveRequest": move_request,
                "nodeState": NodeState::Pausing,
            });
            if idle || expected == RunStatus::Queued {
                patch["status"] = RunStatus::Queued.into();
                patch["recoveryPending"] = true.into();
            }
            db.patch_run(&owner, &patch)?;
            Ok(())
        })
        .await
}

async fn node_name(s: &Service, node: &str) -> Result<String> {
    Ok(s.store
        .get("nodes", node)
        .await?
        .and_then(|n| n["name"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "another node".into()))
}

async fn destination_name(s: &Service, node: Option<&str>) -> Result<String> {
    match node {
        Some(super::LOCAL_NODE_ID) | None => Ok("the master runner".to_owned()),
        Some(node) => node_name(s, node).await,
    }
}

/// Retried by the worker heartbeat until the active execution exits.
pub async fn pause_pending(s: &Service, run_id: &str) -> Result<()> {
    let run = s.store.run(run_id).await?;
    let movement = &run["moveRequest"];
    let due = movement.is_object()
        && movement["idle"] != true
        && movement["requestedAt"]
            .as_i64()
            .is_some_and(|at| now() - at >= PAUSE_DELAY_MS);
    if due {
        stop(s, &run).await?;
    }
    Ok(())
}

async fn stop(s: &Service, run: &Value) -> Result<()> {
    let checkpoint = super::checkpoint(s, text(run, "id")).await?;
    let attempt = text(&checkpoint, "runnerId");
    crate::validation::uuid(attempt)?;
    let response = s
        .http
        .delete(format!(
            "{}/runs/{attempt}",
            super::transport::url(s, text(run, "id")).await?
        ))
        .bearer_auth(super::runner_secret(s).await?)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| Error::unavailable("Waiting for source node to pause."))?;
    if !response.status().is_success() {
        return Err(Error::unavailable("Waiting for source VM to stop."));
    }
    Ok(())
}

/// Whether a conversation without a pending move needs to leave its node.
enum Recovery {
    /// Its node is available, or it has none.
    NotNeeded,
    /// It cannot move yet; the owner was alerted.
    Waiting,
    /// A move to the reserved node, to record in `run.moveRequest`.
    Planned(Value),
}

const WAITING_TITLE: &str = "Conversation waiting for its node";

/// Plans a lossless move of queued work when its retained node has no room.
pub async fn queue_capacity_move(s: &Service, run: &Value) -> Result<bool> {
    if super::publication::requested(run) {
        s.node_publication_notify.notify_one();
        return Ok(false);
    }
    if run["status"] != RunStatus::Queued
        || run["pinnedNodeId"].is_string()
        || run["moveRequest"].is_object()
        || !run["cancelRequestedAt"].is_null()
    {
        return Ok(false);
    }
    let run_id = text(run, "id");
    let checkpoint = super::checkpoint(s, run_id).await?;
    let Some(source) = checkpoint["nodeId"].as_str() else {
        return Ok(false);
    };
    // A normal resume still prefers the intact local disk. Only a capacity
    // shortage starts a full, fenced transfer; unavailable nodes use recovery.
    match super::placement::check(s, run).await {
        Err(error) if super::placement::is_no_capacity(&error) => {}
        _ => return Ok(false),
    }
    let mut selecting = run.clone();
    selecting["placementTransition"] = true.into();
    let reservation = id();
    let selected = match super::placement::reserve(s, &selecting, &reservation).await {
        Ok(selected) => selected,
        Err(error) if super::placement::is_no_capacity(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    if selected["nodeId"] == source {
        super::placement::release(s, &reservation).await?;
        return Ok(false);
    }
    let movement = serde_json::to_value(MoveRequest {
        reservation: &reservation,
        node_id: &selected["nodeId"],
        resources: &selected["resources"],
        requested_at: None,
        // Healthy source: capture all current writes after fencing, rather than
        // restoring an older published recovery point.
        automatic: false,
        idle: None,
        required_tags: None,
        backup_id: None,
    })?;
    if let Err(error) = record_request(s, run_id, movement, false, RunStatus::Queued).await {
        super::placement::release(s, &reservation).await?;
        return Err(error);
    }
    s.store.event(
        run_id,
        "status",
        "The current node has no capacity. Saving the full environment before resuming on another authorized node.",
        None,
    ).await?;
    Ok(true)
}

async fn plan_recovery(s: &Service, current: &Value) -> Result<Recovery> {
    let run_id = text(current, "id");
    let checkpoint = super::checkpoint(s, run_id).await?;
    let source = text(&checkpoint, "nodeId");
    if source.is_empty() {
        return Ok(Recovery::NotNeeded);
    }
    if source == super::LOCAL_NODE_ID {
        // A stale local record only delays recovery until the next attempt.
        let _ = super::refresh_local(s).await;
    }
    let node = s.get("nodes", source).await?;
    let agent = s.get("agents", super::run_agent(current)).await?;
    let available = super::seen_within(&node, super::HEARTBEAT_TIMEOUT_MS)
        && node["revoked"] != true
        && node["executionReady"] == true
        && super::agent_allows(&agent, source);
    if available {
        return Ok(Recovery::NotNeeded);
    }
    if current["pinnedNodeId"].is_string() {
        let body = "This conversation is fixed to a node that is unavailable. It resumes when that node returns.";
        super::alerts::raise(s, run_id, "waiting", WAITING_TITLE, body).await?;
        return Ok(Recovery::Waiting);
    }
    let Some(backup) = latest(s, run_id).await? else {
        let patch = json!({
            "nodeState": NodeState::WaitingForNode,
            "accountWaitReason": "Original node unavailable; no usable recovery point exists.",
        });
        s.store.patch_run(run_id, patch).await?;
        let body = "The node running this conversation is unavailable and no recovery point can resume it elsewhere. It resumes when the node returns.";
        super::alerts::raise(s, run_id, "waiting", WAITING_TITLE, body).await?;
        return Ok(Recovery::Waiting);
    };
    let mut selecting = current.clone();
    selecting["placementTransition"] = true.into();
    selecting["requiredRuntime"] =
        super::publication::manifest(s, &backup).await?["runtime"]["runtimeId"].clone();
    let reservation = id();
    let Ok(selected) = super::placement::reserve(s, &selecting, &reservation).await else {
        let body = "The node running this conversation is unavailable and no other authorized node can resume it yet.";
        super::alerts::raise(
            s,
            run_id,
            "waiting",
            "Conversation waiting for a node",
            body,
        )
        .await?;
        return Ok(Recovery::Waiting);
    };
    let movement = serde_json::to_value(MoveRequest {
        reservation: &reservation,
        node_id: &selected["nodeId"],
        resources: &selected["resources"],
        requested_at: None,
        automatic: true,
        idle: None,
        required_tags: None,
        backup_id: Some(&backup["id"]),
    })?;
    Ok(Recovery::Planned(movement))
}

/// Advances a conversation's pending move, or starts recovery from an unavailable node.
/// Returns whether the conversation may keep executing where it is.
pub async fn advance(s: &Service, run: &Value) -> Result<bool> {
    let run_id = text(run, "id");
    if !run["cancelRequestedAt"].is_null() {
        release_destination(s, run).await?;
        return Ok(true);
    }
    // Keep the authoritative journal on its source until every coalesced final
    // publication is acknowledged. Same-node work may continue in the meantime.
    if super::publication::requested(run) {
        s.node_publication_notify.notify_one();
        return Ok(!run["moveRequest"].is_object());
    }
    let mut current = run.clone();
    if !current["moveRequest"].is_object() {
        match plan_recovery(s, &current).await? {
            Recovery::NotNeeded => return Ok(true),
            Recovery::Waiting => return Ok(false),
            Recovery::Planned(movement) => {
                current["moveRequest"] = movement;
                s.store
                    .patch_run(run_id, json!({ "moveRequest": current["moveRequest"] }))
                    .await?;
            }
        }
    }
    crate::recovery::fence(s, &current).await?;
    match transfer(s, &current).await {
        Ok(backup) => resume_at_destination(s, &current, &backup).await?,
        Err(error) => fail_transfer(s, &current, &error).await?,
    }
    Ok(false)
}

/// Publishes the source disk if needed and mounts it on the destination.
async fn transfer(s: &Service, current: &Value) -> Result<Value> {
    let run_id = text(current, "id");
    let movement = &current["moveRequest"];
    let backup = if let Some(id) = movement["backupId"].as_str() {
        s.get("node-backups", id).await?
    } else {
        s.store
            .patch_run(run_id, json!({ "nodeState": NodeState::Saving }))
            .await?;
        let point = super::publication::capture(s, current).await?;
        s.get("node-backups", text(&point, "id")).await?
    };
    s.store
        .patch_run(run_id, json!({ "nodeState": NodeState::Restoring }))
        .await?;
    super::placement::materialize(s, text(movement, "reservation")).await?;
    let source = super::checkpoint(s, run_id).await?;
    if source["nodeId"] != movement["nodeId"] || movement["automatic"] == true {
        super::restore::start(s, current, text(movement, "nodeId"), &backup).await?;
    }
    Ok(backup)
}

async fn fail_transfer(s: &Service, current: &Value, error: &Error) -> Result<()> {
    let run_id = text(current, "id");
    release_destination(s, current).await?;
    let idle = current["moveRequest"]["idle"] == true;
    let node_state = (!idle).then_some(NodeState::WaitingForNode);
    let mut patch = json!({
        "moveRequest": null,
        "moveReservation": null,
        "nodeState": node_state,
        "movementError": error.message,
        "recoveryPending": !idle,
    });
    if idle {
        patch["status"] = RunStatus::Succeeded.into();
    }
    let owner = run_id.to_owned();
    s.store
        .transaction(move |db| {
            let current = db
                .run(&owner)?
                .ok_or_else(|| Error::not_found("Conversation removed."))?;
            if !current["cancelRequestedAt"].is_null() {
                patch["status"] = RunStatus::Cancelled.into();
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
    if !idle {
        super::alerts::raise(
            s,
            run_id,
            "move-failed",
            "Conversation move failed",
            &format!("{} The original disk is retained.", error.message),
        )
        .await?;
    }
    Ok(())
}

/// Points the checkpoint at the destination and queues the conversation to resume there.
async fn resume_at_destination(s: &Service, current: &Value, backup: &Value) -> Result<()> {
    let run_id = text(current, "id").to_owned();
    let movement = &current["moveRequest"];
    let idle = movement["idle"] == true;
    let automatic = movement["automatic"] == true;
    let destination = text(movement, "nodeId").to_owned();
    let required_tags = movement
        .get("requiredTags")
        .unwrap_or(&current["requiredTags"]);
    if idle {
        super::placement::release(s, text(movement, "reservation")).await?;
    }
    let node = movement["nodeId"].clone();
    let captured_at = backup["capturedAt"].clone();
    let node_state = (!idle).then_some(NodeState::Resuming);
    let reservation = if idle {
        Value::Null
    } else {
        movement["reservation"].clone()
    };
    let status = if idle {
        RunStatus::Succeeded
    } else {
        RunStatus::Queued
    };
    let patch = json!({
        "storage": { "mode": super::ON_DEMAND },
        "movementError": null,
        "accountWaitReason": null,
        "nodeId": node,
        "nodeState": node_state,
        "resources": movement["resources"],
        "requiredTags": required_tags,
        "moveRequest": null,
        "moveReservation": reservation,
        "sessionId": backup["sessionId"],
        "restoredAt": captured_at,
        "recoveryPending": !idle,
        "status": status,
    });
    let owner = run_id.clone();
    s.store
        .transaction(move |db| {
            let key = super::checkpoint_key(&owner);
            let mut checkpoint = db
                .kv(&key)?
                .ok_or_else(|| Error::conflict("Missing resume checkpoint."))?;
            let current = db
                .run(&owner)?
                .ok_or_else(|| Error::not_found("Conversation removed."))?;
            if !current["cancelRequestedAt"].is_null() {
                return Err(Error::conflict("Conversation cancelled during restore."));
            }
            checkpoint["nodeId"] = node;
            checkpoint["process"] = Value::Null;
            checkpoint["settled"] = Value::Null;
            checkpoint["controllerRecoveries"] = 0.into();
            db.set(&key, &checkpoint, None)?;
            db.patch_run(&owner, &patch)?;
            db.event(
                &owner,
                "status",
                "Restoring conversation from a dated recovery point; newer chat remains visible. Verify external effects before repeating actions.",
                Some(&json!({ "capturedAt": captured_at })),
            )?;
            Ok(())
        })
        .await?;
    if automatic {
        let name = node_name(s, &destination).await?;
        super::alerts::raise(
            s,
            &run_id,
            "resumed",
            "Conversation resumed on another node",
            &format!("Its node became unavailable, so it resumed on {name} from its latest recovery point. Recent file changes may be missing."),
        )
        .await?;
    }
    Ok(())
}

pub async fn latest(s: &Service, run: &str) -> Result<Option<Value>> {
    let _operation = s.node_backup_operation.lock(run).await;
    let current = s.store.run(run).await?;
    let Some(head) = current["backup"]["id"].as_str() else {
        return Ok(None);
    };
    let point = s.store.get("node-backups", head).await?.filter(|point| {
        point["runId"] == run && point["sessionId"].is_string() && super::publication::in_s3(point)
    });
    // Publication already verified every immutable dependency. Demand reads
    // verify blocks again, so movement need only validate the current manifest.
    match point {
        Some(point) if super::publication::manifest(s, &point).await.is_ok() => Ok(Some(point)),
        _ => Ok(None),
    }
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
