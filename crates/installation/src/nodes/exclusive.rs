//! Durable, fair reservations. The controller alone confirms physical suspension.
use super::exclusive_state::Reservation;
use crate::{
    config::{id, now},
    error::{Error, Result},
    project_workspaces,
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use cairn_contracts::{AcquireExclusiveInput, EventKind, ExclusiveReservation, ExclusiveState};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};

fn pending_requests(db: &Db<'_>) -> Result<Vec<Reservation>> {
    db.pending_exclusive_reservations()
}

pub fn acquire_tool() -> Value {
    json!({
        "name": "acquire_exclusive",
        "description": "Reserve this execution node exclusively for this run and explicitly included active dependencies. Cairn suspends other Cairn workloads without restarting their commands, blocks new work and confirms acquisition only after the controller acknowledges suspension and the requested RAM headroom. Requires the installation owner's explicit node permission. Native reservations leave external host applications and GPU allocations unchanged; administrator enforcement is cooperative. Use for CPU-intensive local work; release before remote waiting. Requests queue fairly and expire automatically; existing commands, security leases and external services retain their own semantics. Repeating the same request does not extend its duration. Memory reclamation is capability-qualified and fails explicitly when unavailable.",
        "inputSchema": cairn_contracts::schema_for::<AcquireExclusiveInput>(cairn_contracts::Contract::Deserialize),
        "outputSchema": cairn_contracts::schema_for::<ExclusiveReservation>(cairn_contracts::Contract::Serialize),
    })
}

pub fn release_tool() -> Value {
    json!({
        "name": "release_exclusive",
        "description": "Release an exclusive reservation owned by this run. Repeated release is safe. Cairn resumes other tasks only after the controller confirms release; other suspension reasons and cancellations remain effective.",
        "inputSchema": cairn_contracts::schema_for::<cairn_contracts::ReleaseExclusiveInput>(cairn_contracts::Contract::Deserialize),
        "outputSchema": cairn_contracts::schema_for::<ExclusiveReservation>(cairn_contracts::Contract::Serialize),
    })
}

pub fn status_tool() -> Value {
    json!({
        "name": "exclusive_status",
        "description": "Read this run's current and recent exclusive reservations (up to 20), including queued acquisition, confirmed suspension and release. Never treats a queued or expired request as acquired.",
        "inputSchema": cairn_contracts::schema_for::<cairn_contracts::ListNodesInput>(cairn_contracts::Contract::Deserialize),
        "outputSchema": cairn_contracts::schema_for::<cairn_contracts::ExclusiveStatusOutput>(cairn_contracts::Contract::Serialize),
    })
}

pub fn renew_tool() -> Value {
    json!({
        "name": "renew_exclusive",
        "description": "Request an extension of this run's acquired reservation by 1 to 900 seconds from now, within a one-hour lifetime. The response keeps the previously confirmed expiry until the controller acknowledges renewal. Check exclusive_status before relying on an extension. Current permissions and execution identity are revalidated.",
        "inputSchema": cairn_contracts::schema_for::<cairn_contracts::RenewExclusiveInput>(cairn_contracts::Contract::Deserialize),
        "outputSchema": cairn_contracts::schema_for::<ExclusiveReservation>(cairn_contracts::Contract::Serialize),
    })
}

pub async fn renew(s: &Service, bearer: &str, input: &Value) -> Result<Value> {
    let input: cairn_contracts::RenewExclusiveInput = serde_json::from_value(input.clone())
        .map_err(|_| Error::bad("Provide a reservation ID and 1 to 900 seconds."))?;
    uuid(&input.reservation_id)?;
    if !(1..=900).contains(&input.duration_seconds) {
        return Err(Error::bad("Provide a reservation ID and 1 to 900 seconds."));
    }
    let bearer = bearer.to_owned();
    let record = s
        .store
        .transaction(move |db| {
            let run = project_workspaces::authorize_in(db, &bearer)?;
            let mut record = db
                .exclusive_reservation(&input.reservation_id)?
                .ok_or_else(|| Error::not_found("Reservation missing."))?;
            if record.owner_run_id != run["id"] || !valid_record(db, &record)? {
                return Err(Error::forbidden("Reservation access was revoked."));
            }
            if record.state != ExclusiveState::Acquired
                || record.expires_at.is_none_or(|until| until <= now())
            {
                return Err(Error::conflict(
                    "Only a live acquired reservation can be renewed.",
                ));
            }
            let until = now() + input.duration_seconds as i64 * 1000;
            if until > record.acquisition_started_at.unwrap_or(0) + 3_600_000 {
                return Err(Error::bad(
                    "Exclusive reservations have a maximum lifetime of one hour.",
                ));
            }
            record.requested_expires_at = Some(until.max(record.expires_at.unwrap()));
            db.save_exclusive_reservation(&record)?;
            Ok(public(&record))
        })
        .await?;
    s.worker.notify();
    Ok(record)
}

fn terminal(record: &Reservation) -> bool {
    record.state.terminal()
}

fn authorize_run(db: &Db<'_>, run: &Value, node_id: &str) -> Result<String> {
    let denied = || Error::forbidden("Exclusive node access is unavailable or was revoked.");
    let node = db.node_boundary(node_id)?.ok_or_else(denied)?;
    let agent_id = super::run_agent(run);
    let profile = db.agent(agent_id)?.ok_or_else(denied)?;
    let agent = serde_json::to_value(&profile)?;
    if run["status"] != RunStatus::Running
        || !run["cancelRequestedAt"].is_null()
        || run["moveRequest"].is_object()
        || node["revoked"] == true
        || !profile
            .settings
            .exclusive_nodes
            .iter()
            .any(|node| node == node_id)
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
        || checkpoint["prepared"]["backend"]
            != if crate::native::policy::node_native(&node) {
                "native"
            } else {
                "firecracker"
            }
    {
        return Err(Error::conflict(
            "Exclusivity requires an active execution of this node's backend.",
        ));
    }
    let attempt = text(&checkpoint, "runnerId");
    uuid(attempt)?;
    Ok(attempt.into())
}

fn valid_record(db: &Db<'_>, record: &Reservation) -> Result<bool> {
    let Some(run) = db.run(&record.owner_run_id)? else {
        return Ok(false);
    };
    if crate::waits::execution_id(&run).as_str() != record.message_id.as_deref() {
        return Ok(false);
    }
    let Ok(attempt) = authorize_run(db, &run, &record.node_id) else {
        return Ok(false);
    };
    if attempt != record.owner_attempt_id {
        return Ok(false);
    }
    for (run_id, expected_attempt) in record
        .input
        .member_run_ids
        .iter()
        .zip(&record.member_attempt_ids)
    {
        let Some(member) = db.run(&run_id.to_string())? else {
            return Ok(false);
        };
        let Ok(attempt) = authorize_run(db, &member, &record.node_id) else {
            return Ok(false);
        };
        if attempt != *expected_attempt || super::run_agent(&member) != super::run_agent(&run) {
            return Ok(false);
        }
    }
    if let Some(chat) = db.conversation_for_run(text(&run, "id"))?
        && (chat.paused
            || chat.wakeup.is_some()
            || chat.lifecycle != cairn_contracts::ConversationLifecycle::Active
            || crate::chats::continuation::has_unhandled_input(db, &chat)?)
    {
        return Ok(false);
    }
    Ok(true)
}

pub async fn acquire(s: &Service, bearer: &str, input: &Value) -> Result<Value> {
    let mut input: AcquireExclusiveInput = serde_json::from_value(input.clone())
        .map_err(|_| Error::bad("Invalid exclusive reservation request."))?;
    input.validate().map_err(Error::bad)?;
    input.reason = crate::run_output::redact(&input.reason, &[]);
    input.member_run_ids.sort_by_key(ToString::to_string);
    let bearer = bearer.to_owned();
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
                .node_boundary(node_id)?
                .ok_or_else(|| Error::not_found("Node missing."))?;
            if node["exclusiveReservations"] != true {
                return Err(Error::conflict(
                    "Update this node to a runtime supporting exclusive reservations.",
                ));
            }
            if input.minimum_available_memory_mi_b
                > node["limits"]["memoryMiB"].as_u64().unwrap_or(0)
            {
                return Err(Error::bad(
                    "Requested headroom exceeds this node's shared RAM budget.",
                ));
            }
            let mut members = Vec::new();
            for member_id in &input.member_run_ids {
                let member = db
                    .run(&member_id.to_string())?
                    .ok_or_else(|| Error::not_found("Dependency run missing."))?;
                if super::run_agent(&member) != super::run_agent(&run) || member["id"] == run["id"]
                {
                    return Err(Error::forbidden(
                        "Dependencies must be distinct active runs of the same authorized agent.",
                    ));
                }
                members.push(authorize_run(db, &member, node_id)?);
            }
            let requests = pending_requests(db)?;
            if let Some(existing) = requests
                .iter()
                .find(|record| record.owner_run_id == run["id"] && !terminal(record))
            {
                if existing.input != input || existing.owner_attempt_id != owner_attempt {
                    return Err(Error::conflict(
                        "Release the current reservation before changing its scope.",
                    ));
                }
                return Ok(existing.clone());
            }
            let created = now();
            if requests
                .iter()
                .filter(|record| record.node_id == node_id && !terminal(record))
                .count()
                >= 64
            {
                return Err(Error::too_many_requests(
                    "The exclusive reservation queue is full.",
                ));
            }
            let order = db.next_exclusive_order()?;
            let request = Reservation {
                id: id(),
                node_id: node_id.into(),
                owner_run_id: text(&run, "id").into(),
                owner_attempt_id: owner_attempt,
                member_attempt_ids: members,
                message_id: crate::waits::execution_id(&run).as_str().map(str::to_owned),
                reason: input.reason.clone(),
                created_at: created,
                order,
                wait_until: created + input.acquisition_timeout_seconds as i64 * 1000,
                input,
                state: ExclusiveState::Queued,
                epoch: None,
                expires_at: None,
                requested_expires_at: None,
                acquisition_started_at: None,
                acquired_at: None,
                release_requested_at: None,
                paused_run_ids: vec![],
                error: None,
            };
            db.save_exclusive_reservation(&request)?;
            db.event(
                text(&run, "id"),
                &EventKind::from_adapter("node.exclusive"),
                "Exclusive reservation queued.",
                Some(&public(&request)),
            )?;
            Ok(request)
        })
        .await?;
    s.worker.notify();
    let request_id = &request.id;
    let until = now() + request.input.wait_seconds as i64 * 1000;
    loop {
        let current = s
            .store
            .exclusive_reservation(request_id)
            .await?
            .ok_or_else(|| Error::not_found("Reservation missing."))?;
        if current.state == ExclusiveState::Acquired
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
    s.store
        .read(move |db| {
            let requests = db.owned_exclusive_reservations(&run_id)?;
            Ok(serde_json::to_value(
                cairn_contracts::ExclusiveStatusOutput {
                    reservations: requests.iter().map(public_view).collect(),
                },
            )?)
        })
        .await
}

pub async fn release(s: &Service, bearer: &str, input: &Value) -> Result<Value> {
    let input: cairn_contracts::ReleaseExclusiveInput = serde_json::from_value(input.clone())
        .map_err(|_| Error::bad("Provide a reservation ID."))?;
    let bearer = bearer.to_owned();
    let requested = input.reservation_id.clone();
    let read_bearer = bearer.clone();
    let run = s
        .store
        .read(move |db| project_workspaces::authorize_in(db, &read_bearer))
        .await?;
    release_authorized(s, &requested, Some(text(&run, "id")), move |db| {
        project_workspaces::authorize_in(db, &bearer)?;
        Ok(())
    })
    .await
}

pub async fn release_owned(s: &Service, request_id: &str, owner: Option<&str>) -> Result<Value> {
    release_authorized(s, request_id, owner, |_| Ok(())).await
}

pub(crate) async fn release_authorized(
    s: &Service,
    request_id: &str,
    owner: Option<&str>,
    authorize: impl FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
) -> Result<Value> {
    uuid(request_id)?;
    let request_id = request_id.to_owned();
    let owner = owner.map(str::to_owned);
    let record = s
        .store
        .transaction(move |db| {
            authorize(db)?;
            let mut record = db
                .exclusive_reservation(&request_id)?
                .ok_or_else(|| Error::not_found("Reservation missing."))?;
            if owner
                .as_ref()
                .is_some_and(|owner| record.owner_run_id != *owner)
            {
                return Err(Error::forbidden(
                    "Only the reservation's owner can release it.",
                ));
            }
            if !terminal(&record) {
                record.state = if record.epoch.is_none() {
                    ExclusiveState::Released
                } else {
                    ExclusiveState::Releasing
                };
                record.release_requested_at = Some(now());
                db.save_exclusive_reservation(&record)?;
            }
            Ok(public(&record))
        })
        .await?;
    s.worker.notify();
    Ok(record)
}

pub fn public_view(record: &Reservation) -> ExclusiveReservation {
    ExclusiveReservation {
        id: record.id.clone(),
        node_id: record.node_id.clone(),
        owner_run_id: record.owner_run_id.clone(),
        reason: record.reason.clone(),
        state: if record.state == ExclusiveState::Acquired
            && record.expires_at.is_some_and(|until| until <= now())
        {
            ExclusiveState::Releasing
        } else {
            record.state
        },
        created_at: record.created_at,
        expires_at: record.expires_at,
        renewal_pending: record
            .requested_expires_at
            .is_some_and(|until| Some(until) > record.expires_at),
        wait_until: record.wait_until,
        paused_run_ids: record.paused_run_ids.clone(),
        minimum_available_memory_mi_b: record.input.minimum_available_memory_mi_b,
        error: record.error.clone(),
    }
}

pub fn public(record: &Reservation) -> Value {
    serde_json::to_value(public_view(record)).expect("serializable exclusive reservation")
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
    if remote.is_object()
        && s.store
            .exclusive_reservation(text(remote, "id"))
            .await?
            .is_none()
    {
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
            requests.sort_by_key(|record| record.order);
            let mut chosen: Option<Reservation> = None;
            for mut record in requests
                .into_iter()
                .filter(|record| record.node_id == node && !terminal(record))
            {
                let valid = valid_record(db, &record)?;
                let queued_expired =
                    record.state == ExclusiveState::Queued && now() > record.wait_until;
                if record.state == ExclusiveState::Queued && (!valid || queued_expired) {
                    record.state = ExclusiveState::Expired;
                    db.save_exclusive_reservation(&record)?;
                    continue;
                }
                if record.state != ExclusiveState::Queued {
                    if !valid || record.expires_at.is_some_and(|until| now() >= until) {
                        record.state = ExclusiveState::Releasing;
                        db.save_exclusive_reservation(&record)?;
                    }
                    if chosen
                        .as_ref()
                        .is_none_or(|chosen| chosen.state == ExclusiveState::Queued)
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
            if record.state == ExclusiveState::Queued {
                record.epoch = Some(db.next_exclusive_epoch(highest_epoch)?);
                record.expires_at = Some(now() + record.input.duration_seconds as i64 * 1000);
                record.acquisition_started_at = Some(now());
                record.state = ExclusiveState::Acquiring;
                db.save_exclusive_reservation(&record)?;
            }
            Ok(Some(record))
        })
        .await?;
    let Some(record) = pending else { return Ok(()) };
    let request_id = &record.id;
    let releasing = record.state == ExclusiveState::Releasing;
    let response = if releasing {
        controller(
            s,
            node_id,
            "/release",
            Some(&json!({ "id": record.id, "epoch": record.epoch })),
        )
        .await?
    } else {
        let acquisition = controller(
            s,
            node_id,
            "/acquire",
            Some(&json!({
                "id": record.id,
                "epoch": record.epoch,
                "ownerAttemptId": record.owner_attempt_id,
                "memberAttemptIds": record.member_attempt_ids,
                "expiresAt": record.requested_expires_at.or(record.expires_at),
                "remainingMs": (record.requested_expires_at.or(record.expires_at).unwrap_or(0) - now()).max(0),
                "reason": record.reason,
                "minimumAvailableMemoryMiB": record.input.minimum_available_memory_mi_b,
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
                    Some(&json!({ "id": record.id, "epoch": record.epoch })),
                )
                .await?;
                if !released["reservation"].is_null() {
                    return Err(error);
                }
                let request_id = request_id.to_owned();
                s.store
                    .transaction(move |db| {
                        let Some(mut current) = db.exclusive_reservation(&request_id)? else {
                            return Ok(());
                        };
                        current.state = ExclusiveState::Failed;
                        current.error = Some(error.message);
                        db.save_exclusive_reservation(&current)?;
                        clear_suspensions(db, &current)?;

                        Ok(())
                    })
                    .await?;
                return Ok(());
            }
        }
    };
    let request_id = request_id.to_owned();
    s.store
        .transaction(move |db| {
            let Some(mut current) = db.exclusive_reservation(&request_id)? else {
                return Ok(());
            };
            if current.epoch != record.epoch || terminal(&current) {
                return Ok(());
            }
            let previous = current.clone();
            let confirmed = &response["reservation"];
            if releasing && confirmed.is_null() {
                current.state = ExclusiveState::Released;
            } else if !releasing
                && confirmed["id"] == current.id
                && confirmed["epoch"].as_u64() == current.epoch
                && confirmed["state"] == "acquired"
            {
                current.state =
                    if valid_record(db, &current)? && current.state != ExclusiveState::Releasing {
                        ExclusiveState::Acquired
                    } else {
                        ExclusiveState::Releasing
                    };
                if current.acquired_at.is_none() {
                    current.acquired_at = Some(now());
                }
                current.expires_at = confirmed["expiresAt"].as_i64();
                let attempts: HashSet<&str> = confirmed["pausedAttemptIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                current.paused_run_ids = db
                    .attempts()?
                    .iter()
                    .filter(|attempt| attempts.contains(attempt.id.as_str()))
                    .map(|attempt| attempt.run_id.clone())
                    .collect::<Vec<_>>();
                for run in &current.paused_run_ids {
                    if let Some(run_record) = db.run(run.as_str())?
                        && run_record["suspendedByExclusive"] != public(&current)
                    {
                        db.update_run_placement(
                            run.as_str(),
                            crate::nodes::run_placement::PlacementChanges {
                                suspended_by_exclusive: Some(serde_json::from_value(
                                    serde_json::json!(public(&current)),
                                )?),
                                ..Default::default()
                            },
                        )?;
                    }
                }
            }
            if terminal(&current) {
                clear_suspensions(db, &current)?;
            }
            if current != previous {
                db.save_exclusive_reservation(&current)?;
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
                    .map(|record| record.node_id.clone())
                    .collect();
                // A restored backup may predate every manager request while the
                // controller still holds a reservation. Inspect reachable controllers
                // even when the restored request queue is completely empty.
                nodes.extend(
                    db.nodes_boundary()?
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

fn clear_suspensions(db: &Db<'_>, record: &Reservation) -> Result<()> {
    for run_id in &record.paused_run_ids {
        if let Some(run) = db.run(run_id)?
            && run["suspendedByExclusive"]["id"] == record.id
        {
            db.update_run_placement(
                run_id,
                crate::nodes::run_placement::PlacementChanges {
                    suspended_by_exclusive: Some(None),
                    ..Default::default()
                },
            )?;
        }
    }
    Ok(())
}

/// Persist release intent with the wait itself; no cancellation can restore exclusivity.
pub(crate) fn release_before_wait(db: &Db<'_>, run_id: &str) -> Result<()> {
    for mut record in pending_requests(db)?
        .into_iter()
        .filter(|record| record.owner_run_id == run_id && !terminal(record))
    {
        record.state = if record.epoch.is_none() {
            ExclusiveState::Released
        } else {
            ExclusiveState::Releasing
        };
        db.save_exclusive_reservation(&record)?;
    }
    Ok(())
}

pub async fn administrative_pause_ms(s: &Service, run: &str, attempt: &str) -> Result<u64> {
    let node = super::transport::url(s, run).await?;
    administrative_pause_at(s, &node, attempt).await
}

pub(crate) async fn administrative_pause_for_node(
    s: &Service,
    node: Option<&str>,
    attempt: &str,
) -> Result<u64> {
    let node = super::transport::url_for_node(s, node);
    administrative_pause_at(s, &node, attempt).await
}

async fn administrative_pause_at(s: &Service, node: &str, attempt: &str) -> Result<u64> {
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
