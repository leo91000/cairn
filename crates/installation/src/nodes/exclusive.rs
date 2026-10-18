//! Durable, fair reservations. The controller alone confirms physical suspension.
use crate::{
    config::{id, now},
    error::{Error, Result},
    project_workspaces,
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};

const KIND: &str = "node-exclusive-requests";
const MAX_DURATION: u64 = 900;
const MAX_WAIT: u64 = 3600;

fn pending_requests(db: &Db<'_>) -> Result<Vec<Value>> {
    db.json_rows(
        "SELECT data FROM records WHERE kind=? AND json_extract(data,'$.state') IN ('queued','acquiring','acquired','releasing')",
        [KIND],
    )
}

pub fn acquire_tool() -> Value {
    json!({
        "name": "acquire_exclusive",
        "description": "Reserve this execution node exclusively for this run and explicitly included active dependencies. Cairn suspends other VMs without restarting their commands, blocks new work and confirms acquisition only after the controller acknowledges suspension and the requested RAM headroom. Requires the installation owner's explicit node permission. Use for CPU-intensive local work; release before remote waiting. Requests queue fairly and expire automatically; existing commands, security leases and external services retain their own semantics. Repeating the same request does not extend its duration. Memory reclamation is capability-qualified and fails explicitly when unavailable.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "durationSeconds": { "type": "integer", "minimum": 1, "maximum": MAX_DURATION },
                "waitSeconds": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT, "description": "How long this call waits for a result (default 300). Zero returns immediately; the request remains queued." },
                "acquisitionTimeoutSeconds": { "type": "integer", "minimum": 1, "maximum": MAX_WAIT, "description": "How long the queued request remains eligible for acquisition (default 300), independently of the call's response wait." },
                "reason": { "type": "string", "minLength": 1, "maxLength": 500 },
                "memberRunIds": { "type": "array", "maxItems": 16, "uniqueItems": true, "items": { "type": "string", "format": "uuid" } },
                "minimumAvailableMemoryMiB": { "type": "integer", "minimum": 0, "maximum": 1073741824 },
            },
            "required": ["durationSeconds", "reason"],
            "additionalProperties": false,
        },
    })
}

pub fn release_tool() -> Value {
    json!({
        "name": "release_exclusive",
        "description": "Release an exclusive reservation owned by this run. Repeated release is safe. Cairn resumes other tasks only after the controller confirms release; other suspension reasons and cancellations remain effective.",
        "inputSchema": {
            "type": "object",
            "properties": { "reservationId": { "type": "string", "format": "uuid" } },
            "required": ["reservationId"],
            "additionalProperties": false,
        },
    })
}

pub fn status_tool() -> Value {
    json!({
        "name": "exclusive_status",
        "description": "Read this run's current and recent exclusive reservations (up to 20), including queued acquisition, confirmed suspension and release. Never treats a queued or expired request as acquired.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
    })
}

pub fn renew_tool() -> Value {
    json!({
        "name": "renew_exclusive",
        "description": "Request an extension of this run's acquired reservation by 1 to 900 seconds from now, within a one-hour lifetime. The response keeps the previously confirmed expiry until the controller acknowledges renewal. Check exclusive_status before relying on an extension. Current permissions and execution identity are revalidated.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "reservationId": { "type": "string", "format": "uuid" },
                "durationSeconds": { "type": "integer", "minimum": 1, "maximum": MAX_DURATION },
            },
            "required": ["reservationId", "durationSeconds"],
            "additionalProperties": false,
        },
    })
}

pub async fn renew(s: &Service, bearer: &str, input: &Value) -> Result<Value> {
    if !jsonschema::is_valid(&renew_tool()["inputSchema"], input) {
        return Err(Error::bad("Provide a reservation ID and 1 to 900 seconds."));
    }
    let bearer = bearer.to_owned();
    let input = input.clone();
    let record = s
        .store
        .transaction(move |db| {
            let run = project_workspaces::authorize_in(db, &bearer)?;
            let mut record = db
                .get(KIND, text(&input, "reservationId"))?
                .ok_or_else(|| Error::not_found("Reservation missing."))?;
            if record["ownerRunId"] != run["id"] || !valid_record(db, &record)? {
                return Err(Error::forbidden("Reservation access was revoked."));
            }
            if record["state"] != "acquired"
                || record["expiresAt"]
                    .as_i64()
                    .is_none_or(|until| until <= now())
            {
                return Err(Error::conflict(
                    "Only a live acquired reservation can be renewed.",
                ));
            }
            let until = now() + input["durationSeconds"].as_f64().unwrap() as i64 * 1000;
            if until > record["acquisitionStartedAt"].as_i64().unwrap_or(0) + 3_600_000 {
                return Err(Error::bad(
                    "Exclusive reservations have a maximum lifetime of one hour.",
                ));
            }
            record["requestedExpiresAt"] = until.max(record["expiresAt"].as_i64().unwrap()).into();
            db.put(KIND, &record)?;
            Ok(public(&record))
        })
        .await?;
    s.worker.notify();
    Ok(record)
}

fn terminal(record: &Value) -> bool {
    matches!(text(record, "state"), "released" | "failed" | "expired")
}

fn enabled(node: &Value, agent: &str) -> bool {
    node["exclusiveAgents"]
        .as_array()
        .is_some_and(|agents| agents.iter().any(|id| id == agent))
}

fn authorize_run(db: &Db<'_>, run: &Value, node_id: &str) -> Result<String> {
    let denied = || Error::forbidden("Exclusive node access is unavailable or was revoked.");
    let node = db.get("nodes", node_id)?.ok_or_else(denied)?;
    let agent_id = super::run_agent(run);
    let agent = db.get("agents", agent_id)?.ok_or_else(denied)?;
    if run["status"] != RunStatus::Running
        || !run["cancelRequestedAt"].is_null()
        || run["moveRequest"].is_object()
        || node["revoked"] == true
        || !enabled(&node, agent_id)
        || !super::agent_allows(&agent, node_id)
        || !crate::service::covers(&agent, &run["snapshot"]["agent"])
    {
        return Err(denied());
    }
    let checkpoint = super::db_checkpoint(db, text(run, "id"))?;
    if checkpoint["nodeId"]
        .as_str()
        .unwrap_or(super::LOCAL_NODE_ID)
        != node_id
        || checkpoint["prepared"]["backend"] != "firecracker"
    {
        return Err(Error::conflict(
            "Exclusivity requires an active VM on this node.",
        ));
    }
    let attempt = text(&checkpoint, "runnerId");
    uuid(attempt)?;
    Ok(attempt.into())
}

fn valid_record(db: &Db<'_>, record: &Value) -> Result<bool> {
    let Some(run) = db.run(text(record, "ownerRunId"))? else {
        return Ok(false);
    };
    if run["chatExecution"]["messageId"] != record["messageId"] {
        return Ok(false);
    }
    let Ok(attempt) = authorize_run(db, &run, text(record, "nodeId")) else {
        return Ok(false);
    };
    if attempt != record["ownerAttemptId"] {
        return Ok(false);
    }
    for (run_id, expected_attempt) in record["input"]["memberRunIds"]
        .as_array()
        .into_iter()
        .flatten()
        .zip(record["memberAttemptIds"].as_array().into_iter().flatten())
    {
        let Some(member) = db.run(run_id.as_str().unwrap_or(""))? else {
            return Ok(false);
        };
        let Ok(attempt) = authorize_run(db, &member, text(record, "nodeId")) else {
            return Ok(false);
        };
        if attempt != *expected_attempt || super::run_agent(&member) != super::run_agent(&run) {
            return Ok(false);
        }
    }
    if let Some(chat) = crate::chats::chat_for_run(db, text(&run, "id"))?
        && (chat["paused"] == true
            || chat["wakeup"].is_object()
            || !crate::conversation_lifecycle::is_active(&chat)
            || crate::chats::sleep::has_unhandled_input(db, &chat)?)
    {
        return Ok(false);
    }
    Ok(true)
}

pub async fn acquire(s: &Service, bearer: &str, input: &Value) -> Result<Value> {
    if !jsonschema::is_valid(&acquire_tool()["inputSchema"], input)
        || text(input, "reason").trim().is_empty()
    {
        return Err(Error::bad(
            "Provide a concrete reason and a duration of 1 to 900 seconds.",
        ));
    }
    let bearer = bearer.to_owned();
    let mut input = crate::run_output::payload(input, &[]);
    for (field, default) in [
        ("durationSeconds", 1),
        ("waitSeconds", 300),
        ("acquisitionTimeoutSeconds", 300),
        ("minimumAvailableMemoryMiB", 0),
    ] {
        input[field] = (input[field].as_f64().map_or(default, |value| value as u64)).into();
    }
    let mut members = input["memberRunIds"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    members.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    input["memberRunIds"] = members.into();
    let request = s
        .store
        .transaction(move |db| {
            let run = project_workspaces::authorize_in(db, &bearer)?;
            let checkpoint = super::db_checkpoint(db, text(&run, "id"))?;
            let node_id = checkpoint["nodeId"]
                .as_str()
                .unwrap_or(super::LOCAL_NODE_ID);
            let owner_attempt = authorize_run(db, &run, node_id)?;
            let node = db
                .get("nodes", node_id)?
                .ok_or_else(|| Error::not_found("Node missing."))?;
            if node["exclusiveReservations"] != true {
                return Err(Error::conflict(
                    "Update this node to a runtime supporting exclusive reservations.",
                ));
            }
            if input["minimumAvailableMemoryMiB"].as_u64().unwrap()
                > node["limits"]["memoryMiB"].as_u64().unwrap_or(0)
            {
                return Err(Error::bad(
                    "Requested headroom exceeds this node's shared RAM budget.",
                ));
            }
            let mut members = Vec::new();
            for member_id in input["memberRunIds"].as_array().unwrap() {
                let member = db
                    .run(member_id.as_str().unwrap())?
                    .ok_or_else(|| Error::not_found("Dependency run missing."))?;
                if super::run_agent(&member) != super::run_agent(&run) || member["id"] == run["id"]
                {
                    return Err(Error::forbidden(
                        "Dependencies must be distinct active runs of the same authorized agent.",
                    ));
                }
                members.push(Value::from(authorize_run(db, &member, node_id)?));
            }
            let requests = pending_requests(db)?;
            if let Some(existing) = requests
                .iter()
                .find(|record| record["ownerRunId"] == run["id"] && !terminal(record))
            {
                if existing["input"] != input || existing["ownerAttemptId"] != owner_attempt {
                    return Err(Error::conflict(
                        "Release the current reservation before changing its scope.",
                    ));
                }
                return Ok(existing.clone());
            }
            let created = now();
            if requests
                .iter()
                .filter(|record| record["nodeId"] == node_id && !terminal(record))
                .count()
                >= 64
            {
                return Err(Error::too_many_requests(
                    "The exclusive reservation queue is full.",
                ));
            }
            let order = db
                .kv("exclusive-request-order")?
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| Error::conflict("Exclusive queue exhausted."))?;
            db.set("exclusive-request-order", &Value::from(order), None)?;
            let request = json!({
                "id": id(),
                "nodeId": node_id,
                "ownerRunId": run["id"],
                "ownerAttemptId": owner_attempt,
                "memberAttemptIds": members,
                "messageId": run["chatExecution"]["messageId"],
                "reason": input["reason"],
                "input": input,
                "createdAt": created,
                "order": order,
                "waitUntil": created + input["acquisitionTimeoutSeconds"].as_u64().unwrap() as i64 * 1000,
                "state": "queued",
            });
            db.put(KIND, &request)?;
            db.event(
                text(&run, "id"),
                "node.exclusive",
                "Exclusive reservation queued.",
                Some(&public(&request)),
            )?;
            Ok(request)
        })
        .await?;
    s.worker.notify();
    let request_id = text(&request, "id");
    let until = now() + request["input"]["waitSeconds"].as_u64().unwrap_or(300) as i64 * 1000;
    loop {
        let current = s
            .store
            .get(KIND, request_id)
            .await?
            .ok_or_else(|| Error::not_found("Reservation missing."))?;
        if current["state"] == "acquired"
            || terminal(&current)
            || now() >= until
            || s.shutdown.is_cancelled()
        {
            return Ok(public(&current));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub async fn status(s: &Service, bearer: &str) -> Result<Value> {
    let run = project_workspaces::authorize(s, bearer).await?;
    let run_id = text(&run, "id").to_owned();
    s.store.read(move |db| {
        let requests = db.json_rows(
            "SELECT data FROM records WHERE kind=? AND json_extract(data,'$.ownerRunId')=? ORDER BY CASE WHEN json_extract(data,'$.state') IN ('queued','acquiring','acquired','releasing') THEN 0 ELSE 1 END, json_extract(data,'$.createdAt') DESC LIMIT 20",
            [KIND, &run_id],
        )?;
        Ok(requests.iter().map(public).collect::<Vec<_>>().into())
    }).await
}

pub async fn release(s: &Service, bearer: &str, input: &Value) -> Result<Value> {
    if !jsonschema::is_valid(&release_tool()["inputSchema"], input) {
        return Err(Error::bad("Provide a reservation ID."));
    }
    let run = project_workspaces::authorize(s, bearer).await?;
    release_owned(s, text(input, "reservationId"), Some(text(&run, "id"))).await
}

pub async fn release_owned(s: &Service, request_id: &str, owner: Option<&str>) -> Result<Value> {
    uuid(request_id)?;
    let request_id = request_id.to_owned();
    let owner = owner.map(str::to_owned);
    let record = s
        .store
        .transaction(move |db| {
            let mut record = db
                .get(KIND, &request_id)?
                .ok_or_else(|| Error::not_found("Reservation missing."))?;
            if owner
                .as_ref()
                .is_some_and(|owner| record["ownerRunId"] != *owner)
            {
                return Err(Error::forbidden(
                    "Only the reservation's owner can release it.",
                ));
            }
            if !terminal(&record) {
                record["state"] = (if record["epoch"].is_null() {
                    "released"
                } else {
                    "releasing"
                })
                .into();
                record["releaseRequestedAt"] = now().into();
                db.put(KIND, &record)?;
                if let Some(mut node) = db.get("nodes", text(&record, "nodeId"))?
                    && node["exclusiveReservation"]["id"] == record["id"]
                {
                    node["exclusiveReservation"] = public(&record);
                    db.put("nodes", &node)?;
                }
            }
            Ok(public(&record))
        })
        .await?;
    s.worker.notify();
    Ok(record)
}

pub fn public(record: &Value) -> Value {
    let state = if record["state"] == "acquired"
        && record["expiresAt"]
            .as_i64()
            .is_some_and(|until| until <= now())
    {
        Value::from("releasing")
    } else {
        record["state"].clone()
    };
    json!({
        "id": record["id"],
        "nodeId": record["nodeId"],
        "ownerRunId": record["ownerRunId"],
        "reason": record["reason"],
        "state": state,
        "createdAt": record["createdAt"],
        "expiresAt": record["expiresAt"],
        "renewalPending": record["requestedExpiresAt"].as_i64().is_some_and(|until| Some(until) > record["expiresAt"].as_i64()),
        "waitUntil": record["waitUntil"],
        "pausedRunIds": record["pausedRunIds"].as_array().cloned().unwrap_or_default(),
        "minimumAvailableMemoryMiB": record["input"]["minimumAvailableMemoryMiB"],
        "error": record["error"],
    })
}

async fn controller(s: &Service, node: &str, action: &str, body: Option<&Value>) -> Result<Value> {
    let url = format!("{}/exclusive{action}", super::controller_url(s, node));
    let request = body.map_or_else(|| s.http.get(&url), |body| s.http.post(&url).json(body));
    let timeout = if body.is_some() { 150 } else { 5 };
    let response = request
        .bearer_auth(super::runner_secret(s).await?)
        .timeout(Duration::from_secs(timeout))
        .send()
        .await
        .map_err(|_| {
            Error::unavailable(
                "Exclusive reservation acknowledgement is unavailable; Cairn will reconcile it.",
            )
        })?;
    if !response.status().is_success() {
        return Err(Error::new(
            response.status().as_u16(),
            "The controller did not confirm the exclusive reservation.",
        ));
    }
    response
        .json()
        .await
        .map_err(|_| Error::unavailable("Invalid exclusive reservation acknowledgement."))
}

async fn reconcile_node(s: &Service, node_id: &str) -> Result<()> {
    let observed = controller(s, node_id, "", None).await?;
    let highest_epoch = observed["highestEpoch"].as_u64().unwrap_or(0);
    let remote = &observed["reservation"];
    if remote.is_object() && s.store.get(KIND, text(remote, "id")).await?.is_none() {
        // Restoring a manager backup must not orphan a newer controller reservation.
        controller(
            s,
            node_id,
            "/release",
            Some(&json!({ "id": remote["id"], "epoch": remote["epoch"] })),
        )
        .await?;
    }
    let node = node_id.to_owned();
    let pending = s
        .store
        .transaction(move |db| {
            let mut requests = pending_requests(db)?;
            requests.sort_by_key(|record| record["order"].as_u64().unwrap_or(0));
            let mut chosen: Option<Value> = None;
            for mut record in requests
                .into_iter()
                .filter(|record| record["nodeId"] == node && !terminal(record))
            {
                let valid = valid_record(db, &record)?;
                let queued_expired = record["state"] == "queued"
                    && record["waitUntil"]
                        .as_i64()
                        .is_some_and(|until| now() > until);
                if record["state"] == "queued" && (!valid || queued_expired) {
                    record["state"] = "expired".into();
                    db.put(KIND, &record)?;
                    continue;
                }
                if record["state"] != "queued" {
                    if !valid
                        || record["expiresAt"]
                            .as_i64()
                            .is_some_and(|until| now() >= until)
                    {
                        record["state"] = "releasing".into();
                        db.put(KIND, &record)?;
                    }
                    if chosen
                        .as_ref()
                        .is_none_or(|chosen| chosen["state"] == "queued")
                    {
                        chosen = Some(record);
                    }
                    // Expire every queued waiter even while a holder still owns
                    // this node; a timed-out caller can submit a fresh request.
                    continue;
                }
                if chosen.is_none() {
                    chosen = Some(record);
                }
            }
            let Some(mut record) = chosen else {
                return Ok(None);
            };
            if record["state"] == "queued" {
                let epoch = db
                    .kv("exclusive-epoch")?
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0)
                    .max(highest_epoch)
                    .checked_add(1)
                    .ok_or_else(|| Error::conflict("Exclusive generation exhausted."))?;
                db.set("exclusive-epoch", &Value::from(epoch), None)?;
                record["epoch"] = epoch.into();
                record["expiresAt"] =
                    (now() + record["input"]["durationSeconds"].as_i64().unwrap() * 1000).into();
                record["acquisitionStartedAt"] = now().into();
                record["state"] = "acquiring".into();
                db.put(KIND, &record)?;
            }
            let mut node_record = db
                .get("nodes", &node)?
                .ok_or_else(|| Error::not_found("Node missing."))?;
            let summary = public(&record);
            if node_record["exclusiveReservation"] != summary {
                node_record["exclusiveReservation"] = summary;
                db.put("nodes", &node_record)?;
            }
            Ok(Some(record))
        })
        .await?;
    let Some(record) = pending else { return Ok(()) };
    let request_id = text(&record, "id");
    let releasing = record["state"] == "releasing";
    let response = if releasing {
        controller(
            s,
            node_id,
            "/release",
            Some(&json!({ "id": record["id"], "epoch": record["epoch"] })),
        )
        .await?
    } else {
        let acquisition = controller(
            s,
            node_id,
            "/acquire",
            Some(&json!({
                "id": record["id"],
                "epoch": record["epoch"],
                "ownerAttemptId": record["ownerAttemptId"],
                "memberAttemptIds": record["memberAttemptIds"],
                "expiresAt": record.get("requestedExpiresAt").unwrap_or(&record["expiresAt"]),
                "remainingMs": (record.get("requestedExpiresAt").unwrap_or(&record["expiresAt"]).as_i64().unwrap_or(0) - now()).max(0),
                "reason": record["reason"],
                "minimumAvailableMemoryMiB": record["input"]["minimumAvailableMemoryMiB"],
            })),
        )
        .await;
        match acquisition {
            Ok(response) => response,
            Err(error) => {
                // Fence delayed acquisition before clearing the manager's admission barrier.
                let released = controller(
                    s,
                    node_id,
                    "/release",
                    Some(&json!({ "id": record["id"], "epoch": record["epoch"] })),
                )
                .await?;
                if !released["reservation"].is_null() {
                    return Err(error);
                }
                let request_id = request_id.to_owned();
                let node_id = node_id.to_owned();
                s.store
                    .transaction(move |db| {
                        let Some(mut current) = db.get(KIND, &request_id)? else {
                            return Ok(());
                        };
                        current["state"] = "failed".into();
                        current["error"] = error.message.into();
                        db.put(KIND, &current)?;
                        clear_suspensions(db, &current)?;
                        if let Some(mut node) = db.get("nodes", &node_id)? {
                            node["exclusiveReservation"] = Value::Null;
                            db.put("nodes", &node)?;
                        }
                        Ok(())
                    })
                    .await?;
                return Ok(());
            }
        }
    };
    let request_id = request_id.to_owned();
    let node_id = node_id.to_owned();
    s.store
        .transaction(move |db| {
            let Some(mut current) = db.get(KIND, &request_id)? else {
                return Ok(());
            };
            if current["epoch"] != record["epoch"] || terminal(&current) {
                return Ok(());
            }
            let previous = current.clone();
            let confirmed = &response["reservation"];
            if releasing && confirmed.is_null() {
                current["state"] = "released".into();
            } else if !releasing
                && confirmed["id"] == current["id"]
                && confirmed["epoch"] == current["epoch"]
                && confirmed["state"] == "acquired"
            {
                current["state"] =
                    (if valid_record(db, &current)? && current["state"] != "releasing" {
                        "acquired"
                    } else {
                        "releasing"
                    })
                    .into();
                if current["acquiredAt"].is_null() {
                    current["acquiredAt"] = now().into();
                }
                current["expiresAt"] = confirmed["expiresAt"].clone();
                let attempts: HashSet<&str> = confirmed["pausedAttemptIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                current["pausedRunIds"] = db
                    .list("node-attempts")?
                    .iter()
                    .filter(|attempt| attempts.contains(text(attempt, "id")))
                    .map(|attempt| attempt["runId"].clone())
                    .collect::<Vec<_>>()
                    .into();
                for run in current["pausedRunIds"].as_array().into_iter().flatten() {
                    if let Some(run_record) = db.run(run.as_str().unwrap_or(""))?
                        && run_record["suspendedByExclusive"] != public(&current)
                    {
                        db.patch_run(
                            run.as_str().unwrap(),
                            &json!({ "suspendedByExclusive": public(&current) }),
                        )?;
                    }
                }
            }
            if terminal(&current) {
                clear_suspensions(db, &current)?;
            }
            if current != previous {
                db.put(KIND, &current)?;
            }
            if let Some(mut node) = db.get("nodes", &node_id)? {
                let summary = if terminal(&current) {
                    Value::Null
                } else {
                    public(&current)
                };
                if node["exclusiveReservation"] != summary {
                    node["exclusiveReservation"] = summary;
                    db.put("nodes", &node)?;
                }
            }
            Ok(())
        })
        .await
}

pub async fn maintain(s: Arc<Service>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            _ = interval.tick() => {},
        }
        let Ok(nodes) = s
            .store
            .read(|db| {
                let requests = pending_requests(db)?;
                let mut nodes: HashSet<String> = requests
                    .iter()
                    .filter(|record| !terminal(record))
                    .map(|record| text(record, "nodeId").to_owned())
                    .collect();
                // A restored backup may predate every manager request while the
                // controller still holds a reservation. Inspect reachable controllers
                // even when the restored request queue is completely empty.
                nodes.extend(
                    db.list("nodes")?
                        .iter()
                        .filter(|node| {
                            node["exclusiveReservations"] == true
                                && node["revoked"] != true
                                && (node["id"] == super::LOCAL_NODE_ID
                                    || super::seen_within(node, 30_000))
                        })
                        .map(|node| text(node, "id").to_owned()),
                );
                Ok(nodes)
            })
            .await
        else {
            continue;
        };
        stream::iter(nodes).map(|node| {
            let s = s.clone();
            async move {
            if let Err(error) = reconcile_node(&s, &node).await {
                tracing::warn!(node_id = %node, message = %error.message, "Exclusive reservation reconciliation will retry");
            }
            }
        }).buffer_unordered(4).collect::<Vec<_>>().await;
    }
}

fn clear_suspensions(db: &Db<'_>, record: &Value) -> Result<()> {
    for run_id in record["pausedRunIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some(run) = db.run(run_id)?
            && run["suspendedByExclusive"]["id"] == record["id"]
        {
            db.patch_run(run_id, &json!({ "suspendedByExclusive": null }))?;
        }
    }
    Ok(())
}

/// Persist release intent with the wait itself; no cancellation can restore exclusivity.
pub(crate) fn release_before_wait(db: &Db<'_>, run_id: &str) -> Result<()> {
    for mut record in pending_requests(db)?
        .into_iter()
        .filter(|record| record["ownerRunId"] == run_id && !terminal(record))
    {
        record["state"] = (if record["epoch"].is_null() {
            "released"
        } else {
            "releasing"
        })
        .into();
        db.put(KIND, &record)?;
    }
    Ok(())
}

pub async fn administrative_pause_ms(s: &Service, run: &str, attempt: &str) -> Result<u64> {
    let node = super::transport::url(s, run).await?;
    let response = s
        .http
        .get(format!("{node}/runs/{attempt}/administrative-pause"))
        .bearer_auth(super::runner_secret(s).await?)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map_err(|_| Error::unavailable("Execution suspension accounting unavailable."))?;
    if response.status().as_u16() == 404 {
        return Ok(0);
    }
    if !response.status().is_success() {
        return Err(Error::unavailable(
            "Execution suspension accounting unavailable.",
        ));
    }
    let value: Value = response.json().await.map_err(Error::internal)?;
    value["administrativePausedMs"]
        .as_u64()
        .ok_or_else(|| Error::unavailable("Invalid execution suspension accounting."))
}
