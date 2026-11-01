//! Durable pause/copy/resume coordination. Never changes a destination before fencing.
use super::NodeState;
use crate::{
    config::{id, now},
    error::{Error, Result},
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::text,
};
use cairn_contracts::EventKind;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MOVE_DESCRIPTION: &str = "Move this conversation to an authorized node. Call list_nodes first to see ids, tags, shared budgets and free slots. Failure leaves the current conversation running. Success saves, pauses and resumes on the destination, interrupting running commands. Optional waitSeconds respects the configured wait limit.";
const LIST_NODES_DESCRIPTION: &str = "List authorized execution nodes with their tags, shared CPU/RAM/disk budgets, slot availability and current resource pressure. Use a returned node id with move_to_node. Resources are shared automatically; agents do not request allocations.";
/// A requested move waits this long for the conversation to settle before pausing it.
const PAUSE_DELAY_MS: i64 = 2000;
const MAX_WAIT_SECONDS: u64 = 3600;
pub(crate) const CANCELLED_CAPACITY_WAIT: &str = "Movement capacity wait was cancelled.";

pub fn tool() -> Value {
    let mut input = cairn_contracts::schema_for::<cairn_contracts::MoveToNodeInput>(
        cairn_contracts::Contract::Deserialize,
    );
    input["properties"].as_object_mut().unwrap().remove("runId");
    input["required"]
        .as_array_mut()
        .unwrap()
        .retain(|field| field != "runId");
    json!({
        "name": "move_to_node",
        "description": MOVE_DESCRIPTION,
        "inputSchema": input,
        "outputSchema": cairn_contracts::schema_for::<cairn_contracts::MoveToNodeOutput>(cairn_contracts::Contract::Serialize)
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
    exclusive_allowed: bool,
    exclusive_reservations: bool,
    exclusive_in_use: bool,
    exclusive_until: &'a Value,
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
        .agent_snapshot(super::run_agent(run))
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
                exclusive_allowed: node["exclusiveAgents"].as_array().is_some_and(|agents| {
                    agents.iter().any(|agent| agent == super::run_agent(run))
                }),
                exclusive_reservations: node["exclusiveReservations"] == true,
                exclusive_in_use: node["exclusiveReservation"].is_object(),
                exclusive_until: &node["exclusiveReservation"]["expiresAt"],
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
    requested(s, run, args, false, Authorization::default()).await
}

type LocalAuthorization = std::sync::Arc<dyn Fn(&Db<'_>) -> Result<()> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct Authorization {
    local: LocalAuthorization,
    public_credential: Option<String>,
    management_authorization: Option<crate::authorization::Authorization>,
    cancellation: CancellationToken,
    admitted: Arc<AtomicBool>,
}

impl Default for Authorization {
    fn default() -> Self {
        Self::new(|_| Ok(()), None)
    }
}

impl Authorization {
    pub(crate) fn new(
        local: impl Fn(&Db<'_>) -> Result<()> + Send + Sync + 'static,
        public_credential: Option<String>,
    ) -> Self {
        Self {
            local: std::sync::Arc::new(local),
            public_credential,
            management_authorization: None,
            cancellation: CancellationToken::new(),
            admitted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn with_management_authorization(
        mut self,
        authorization: crate::authorization::Authorization,
    ) -> Self {
        self.management_authorization = Some(authorization);
        self
    }

    pub(crate) fn workspace(bearer: &str, run: &Value) -> Self {
        let bearer = bearer.to_owned();
        let run_id = text(run, "id").to_owned();
        let turn = crate::waits::execution_id(run).clone();
        Self::new(
            move |db| {
                let current = crate::project_workspaces::authorize_in(db, &bearer)?;
                if text(&current, "id") != run_id || *crate::waits::execution_id(&current) != turn {
                    return Err(Error::unauthorized(
                        "Workspace access expired or was revoked.",
                    ));
                }
                Ok(())
            },
            None,
        )
    }

    pub(crate) fn check(&self, db: &Db<'_>) -> Result<()> {
        self.check_cancellation()?;
        self.check_access(db)
    }

    pub(crate) fn check_access(&self, db: &Db<'_>) -> Result<()> {
        if let Some(authorization) = &self.management_authorization {
            authorization.check_local(db)?;
        }
        (self.local)(db)
    }

    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        self.check_cancellation()?;
        if let Some(authorization) = &self.management_authorization {
            authorization.recheck().await?;
        }
        self.recheck_access(s).await
    }

    pub(crate) async fn recheck_access(&self, s: &Service) -> Result<()> {
        if let Some(digest) = &self.public_credential {
            crate::relay::authorize_plugin_mcp_scope(s, digest, "run").await?;
        }
        Ok(())
    }

    fn check_cancellation(&self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(Error::conflict(CANCELLED_CAPACITY_WAIT));
        }
        Ok(())
    }

    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub(crate) fn admitted(&self) -> bool {
        self.admitted.load(Ordering::Acquire)
    }
}

/// Keep one admission per conversation. Dropping an HTTP/workspace request must
/// clear its wait and release only an unreferenced destination reservation.
struct AdmissionGuard {
    service: Service,
    run: String,
    reservation: String,
    until: Option<i64>,
    authorization: Authorization,
    active: bool,
}

impl AdmissionGuard {
    fn new(service: &Service, run: &str, authorization: &Authorization) -> Result<Self> {
        service.node_move_operations.claim_wait(run)?;
        Ok(Self {
            service: service.clone(),
            run: run.to_owned(),
            reservation: id(),
            until: None,
            authorization: authorization.clone(),
            active: true,
        })
    }

    async fn finish(&mut self, succeeded: bool) -> Result<()> {
        if !succeeded {
            Self::cleanup(&self.service, &self.run, &self.reservation, self.until).await?;
        }
        self.service.node_move_operations.finish_wait(&self.run);
        self.active = false;
        Ok(())
    }

    async fn cleanup(
        service: &Service,
        run: &str,
        reservation: &str,
        until: Option<i64>,
    ) -> Result<()> {
        let (owner, allocation) = (run.to_owned(), reservation.to_owned());
        // This writer queues behind any in-flight admission transaction. A
        // committed move owns its reservation even if its reply was dropped.
        let unclaimed = service
            .store
            .transaction(move |db| {
                if let Some(state) = db.run_state(&owner)? {
                    let placement = db.run_placement(&owner)?;
                    if placement.move_reservation.as_deref() == Some(allocation.as_str())
                        || placement
                            .move_request
                            .as_ref()
                            .is_some_and(|request| request.reservation == allocation)
                    {
                        return Ok(false);
                    }
                    if until.is_some() && state.capacity_wait_until == until {
                        db.change_run(
                            &owner,
                            crate::runs::RunChanges {
                                capacity_wait_until: Some(None),
                                ..Default::default()
                            },
                        )?;
                    }
                }
                Ok(true)
            })
            .await?;
        if unclaimed {
            super::placement::release(service, reservation).await?;
        }
        Ok(())
    }
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.authorization.cancel();
        let (service, run, reservation, until) = (
            self.service.clone(),
            self.run.clone(),
            self.reservation.clone(),
            self.until,
        );
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if Self::cleanup(&service, &run, &reservation, until)
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        "Interrupted move cleanup failed; destination ownership retained."
                    );
                }
                service.node_move_operations.finish_wait(&run);
            });
        }
    }
}

pub(crate) async fn request_authorized(
    s: &Service,
    run: &Value,
    args: &Value,
    authorize: Authorization,
) -> Result<Value> {
    requested(s, run, args, false, authorize).await
}

/// Agents choose a node; the runtime owns all resource sharing.
pub async fn request_by_agent(s: &Service, run: &Value, args: &Value) -> Result<Value> {
    request_by_agent_authorized(s, run, args, Authorization::default()).await
}

pub(crate) async fn request_by_agent_authorized(
    s: &Service,
    run: &Value,
    args: &Value,
    authorize: Authorization,
) -> Result<Value> {
    crate::validation::uuid(text(args, "nodeId"))?;
    requested(s, run, args, true, authorize).await.map_err(|mut error| {
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
    let Some(tags) = args.get("requiredTags").filter(|tags| !tags.is_null()) else {
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

async fn requested(
    s: &Service,
    run: &Value,
    args: &Value,
    by_agent: bool,
    authorize: Authorization,
) -> Result<Value> {
    let mut guard = AdmissionGuard::new(s, text(run, "id"), &authorize)?;
    let result = requested_inner(s, run, args, by_agent, authorize, &mut guard).await;
    guard.finish(result.is_ok()).await?;
    result
}

async fn requested_inner(
    s: &Service,
    run: &Value,
    args: &Value,
    by_agent: bool,
    authorize: Authorization,
    guard: &mut AdmissionGuard,
) -> Result<Value> {
    if run["environmentState"].is_string() {
        return Err(Error::conflict(
            "The working environment expired. Send a new message before moving it.",
        ));
    }
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
    let source = (!by_agent).then(crate::native::policy::origin);
    let mut proposed = run.clone();
    if let Some(source) = &source {
        let (record, source) = (run.clone(), source.clone());
        let grant = s
            .store
            .read(move |db| crate::native::policy::run_grant(db, &record, &source))
            .await?;
        crate::native::policy::apply(&mut proposed, &grant);
    }
    let run = &proposed;
    let run_id = text(run, "id");
    if run["moveRequest"].is_object() {
        return Err(Error::conflict("A movement is already pending."));
    }
    let checkpoint = super::checkpoint(s, run_id).await?;
    let deadline = now() + wait as i64 * 1000;
    // A finished conversation with a VM session moves without being resumed.
    let idle = run["status"] == RunStatus::Succeeded
        && run["sessionId"].is_string()
        && matches!(
            checkpoint["prepared"]["backend"].as_str(),
            Some("firecracker" | "native")
        );
    let reservation = guard.reservation.clone();
    guard.until = Some(deadline);
    let mut selecting = run.clone();
    selecting["placementTransition"] = true.into();
    selecting["targetNodeId"] = args["nodeId"].clone();
    selecting["requiredTags"] = args
        .get("requiredTags")
        .filter(|tags| !tags.is_null())
        .unwrap_or(&run["requiredTags"])
        .clone();
    let selected = wait_for_capacity(
        s,
        run_id,
        &selecting,
        &reservation,
        idle,
        deadline,
        authorize.clone(),
    )
    .await?;
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
    record_request(
        s,
        run_id,
        move_request,
        idle,
        expected,
        source,
        authorize.clone(),
    )
    .await?;
    authorize.admitted.store(true, Ordering::Release);
    let destination = destination_name(s, selected["nodeId"].as_str()).await?;
    // The owner sees in the conversation that the agent moved itself, and what it asked for.
    let message = if by_agent {
        format!("The agent is moving to {destination}. Running commands are interrupted.")
    } else {
        format!("Moving the conversation to {destination}. Running commands are interrupted.")
    };
    s.store
        .event(run_id, &EventKind::Status, &message, None)
        .await?;
    Ok(json!({
        "status": "moving",
        "nodeId": selected["nodeId"],
    }))
}

async fn set_capacity_wait(s: &Service, run_id: &str, until: Option<i64>) -> Result<()> {
    s.store
        .change_run(
            run_id,
            crate::runs::RunChanges {
                capacity_wait_until: Some(until),
                ..Default::default()
            },
        )
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
    authorize: Authorization,
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
        match super::placement::reserve_authorized(s, selecting, reservation, authorize.clone())
            .await
        {
            Ok(value) => return Ok(value),
            Err(error) if error.is_unavailable() && now() < deadline => {
                set_capacity_wait(s, run_id, Some(deadline)).await?;
                let stopping = tokio::select! {
                    () = s.shutdown.cancelled() => true,
                    () = authorize.cancellation.cancelled() => {
                        return Err(Error::conflict(CANCELLED_CAPACITY_WAIT));
                    },
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
    source: Option<crate::native::policy::Origin>,
    authorize: Authorization,
) -> Result<()> {
    authorize.recheck(s).await?;
    let owner = run_id.to_owned();
    s.store
        .transaction(move |db| {
            authorize.check(db)?;
            let current = db
                .run(&owner)?
                .ok_or_else(|| Error::not_found("Conversation removed."))?;
            if source.is_none()
                && expected == RunStatus::Queued
                && source_mode_unconfirmed(db, &owner)?
            {
                return Err(Error::unavailable(
                    "Waiting for the node's privilege mode to be confirmed.",
                ));
            }
            // Explicit movement would bypass a mission wait. Internal relocation
            // of an unfinished queued turn must still be able to restore capacity.
            if expected != RunStatus::Queued
                && current["trigger"] != "chat"
                && current["wakeup"].is_object()
            {
                return Err(Error::conflict(
                    "Finish the mission wait before requesting a node move.",
                ));
            }
            if current["environmentState"].is_string()
                || current["moveRequest"].is_object()
                || !current["cancelRequestedAt"].is_null()
                || current["status"] != expected
                || (expected == RunStatus::Queued && current["pinnedNodeId"].is_string())
            {
                return Err(Error::conflict(
                    "Conversation changed while reserving capacity.",
                ));
            }
            if let Some(source) = source {
                crate::native::policy::authorize_run_in(db, &owner, &source)?;
            }
            let directed = db.run(&owner)?.unwrap_or_default();
            let destination = text(&move_request, "nodeId");
            let agent = db
                .agent_snapshot(super::run_agent(&directed))?
                .unwrap_or_default();
            let node = db.node_boundary(destination)?.unwrap_or_default();
            if !super::agent_allows(&agent, destination)
                || node["revoked"] == true
                || crate::native::policy::node_native(&node)
                    && !crate::native::policy::allows(db, &directed, &node)?
            {
                return Err(Error::forbidden("Destination authorization changed."));
            }
            let mut patch = crate::runs::RunChanges {
                capacity_wait_until: Some(None),
                placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
                    movement_error: Some(None),
                    move_request: Some(serde_json::from_value(serde_json::json!(move_request))?),
                    node_state: Some(serde_json::from_value(serde_json::json!(
                        NodeState::Pausing
                    ))?),
                    ..Default::default()
                })),
                ..Default::default()
            };
            if idle || expected == RunStatus::Queued {
                patch.status = Some(RunStatus::Queued);
                patch.recovery_pending = Some(true);
            }
            db.change_run(&owner, patch)?;
            Ok(())
        })
        .await
}

async fn node_name(s: &Service, node: &str) -> Result<String> {
    Ok(s.store
        .node_boundary(node)
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

fn source_mode_unconfirmed(db: &Db<'_>, run: &str) -> Result<bool> {
    let checkpoint = super::db_checkpoint(db, run)?;
    let Some(source) = checkpoint["nodeId"].as_str() else {
        return Ok(false);
    };
    let node = db.node_boundary(source)?.unwrap_or_default();
    Ok(super::native_mode::unconfirmed(&node))
}

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
    let owner = run_id.to_owned();
    if s.store
        .read(move |db| source_mode_unconfirmed(db, &owner))
        .await?
    {
        return Ok(false);
    }
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
    if let Err(error) = record_request(
        s,
        run_id,
        movement,
        false,
        RunStatus::Queued,
        None,
        Authorization::default(),
    )
    .await
    {
        super::placement::release(s, &reservation).await?;
        return Err(error);
    }
    s.store.event(
        run_id, &EventKind::Status,
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
    let node = s.node_boundary(source).await?;
    if super::native_mode::unconfirmed(&node) {
        return Ok(Recovery::Waiting);
    }
    let agent = s.agent_snapshot(super::run_agent(current)).await?;
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
        let patch = crate::runs::RunChanges {
            account_wait_reason: Some(Some(
                "Original node unavailable; no usable recovery point exists.".into(),
            )),
            placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
                node_state: Some(serde_json::from_value(serde_json::json!(
                    NodeState::WaitingForNode
                ))?),
                ..Default::default()
            })),
            ..Default::default()
        };
        s.store.change_run(run_id, patch).await?;
        let body = "The node running this conversation is unavailable and no recovery point can resume it elsewhere. It resumes when the node returns.";
        super::alerts::raise(s, run_id, "waiting", WAITING_TITLE, body).await?;
        return Ok(Recovery::Waiting);
    };
    let mut selecting = current.clone();
    selecting["placementTransition"] = true.into();
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
    let checkpoint =
        crate::worker::checkpoint::RunCheckpoint::load(&s.store, text(run, "id")).await?;
    advance_for_attempt(s, run, checkpoint.as_ref()).await
}

pub(crate) async fn advance_for_attempt(
    s: &Service,
    run: &Value,
    expected_attempt: Option<&crate::worker::checkpoint::RunCheckpoint>,
) -> Result<bool> {
    let run_id = text(run, "id");
    let current_checkpoint =
        crate::worker::checkpoint::RunCheckpoint::load(&s.store, run_id).await?;
    if !crate::recovery::same_execution(current_checkpoint.as_ref(), expected_attempt) {
        return Err(Error::conflict(
            "Execution attempt changed during recovery.",
        ));
    }
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
                let owner = run_id.to_owned();
                let expected = expected_attempt.cloned();
                let pending = movement.clone();
                let accepted = s
                    .store
                    .transaction(move |db| {
                        let checkpoint =
                            crate::worker::checkpoint::RunCheckpoint::load_in(db, &owner)?;
                        if !crate::recovery::same_execution(checkpoint.as_ref(), expected.as_ref())
                            || source_mode_unconfirmed(db, &owner)?
                            || db
                                .run(&owner)?
                                .is_none_or(|run| run["moveRequest"].is_object())
                        {
                            return Ok(false);
                        }
                        db.update_run_placement(
                            &owner,
                            crate::nodes::run_placement::PlacementChanges {
                                move_request: Some(serde_json::from_value(serde_json::json!(
                                    pending
                                ))?),
                                ..Default::default()
                            },
                        )?;
                        Ok(true)
                    })
                    .await;
                match accepted {
                    Ok(true) => current["moveRequest"] = movement,
                    rejected => {
                        super::placement::release(s, text(&movement, "reservation")).await?;
                        rejected?;
                        return Ok(false);
                    }
                }
            }
        }
    }
    crate::recovery::fence_attempt(s, &current, expected_attempt).await?;
    let current_checkpoint =
        crate::worker::checkpoint::RunCheckpoint::load(&s.store, run_id).await?;
    let mut expected_fenced = expected_attempt.cloned();
    if let Some(saved) = &mut expected_fenced {
        saved.process = None;
    }
    if !crate::recovery::same_execution(current_checkpoint.as_ref(), expected_fenced.as_ref()) {
        return Err(Error::conflict(
            "Execution attempt changed during recovery.",
        ));
    }
    match transfer(s, &current).await {
        Ok(backup) => resume_at_destination(s, &current, &backup, expected_fenced.as_ref()).await?,
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
            .update_run_placement(
                run_id,
                crate::nodes::run_placement::PlacementChanges {
                    node_state: Some(serde_json::from_value(serde_json::json!(
                        NodeState::Saving
                    ))?),
                    ..Default::default()
                },
            )
            .await?;
        let point = super::publication::capture(s, current).await?;
        s.get("node-backups", text(&point, "id")).await?
    };
    s.store
        .update_run_placement(
            run_id,
            crate::nodes::run_placement::PlacementChanges {
                node_state: Some(serde_json::from_value(serde_json::json!(
                    NodeState::Restoring
                ))?),
                ..Default::default()
            },
        )
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
    let mut patch = crate::runs::RunChanges {
        recovery_pending: Some(!idle),
        placement: Some(Box::new(super::run_placement::PlacementChanges {
            move_request: Some(None),
            move_reservation: Some(None),
            node_state: Some(serde_json::from_value(serde_json::to_value(node_state)?)?),
            movement_error: Some(Some(error.message.clone())),
            ..Default::default()
        })),
        ..Default::default()
    };
    if idle {
        patch.status = Some(RunStatus::Succeeded);
    }
    let owner = run_id.to_owned();
    s.store
        .transaction(move |db| {
            let current = db
                .run(&owner)?
                .ok_or_else(|| Error::not_found("Conversation removed."))?;
            let cancelled = !current["cancelRequestedAt"].is_null();
            if cancelled {
                patch.status = Some(RunStatus::Cancelled);
                patch.recovery_pending = Some(false);
                patch.placement.as_mut().unwrap().node_state = Some(None);
            }
            db.change_run(&owner, patch)?;
            // A run waiting for an automatic retry shows the stop in its conversation.
            if cancelled && current["retry"]["nextAttemptAt"].is_number() {
                db.event(
                    &owner,
                    &EventKind::Status,
                    RunStatus::Cancelled.as_str(),
                    None,
                )?;
            }
            Ok(())
        })
        .await?;
    s.store
        .event(
            run_id,
            &EventKind::Status,
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
async fn resume_at_destination(
    s: &Service,
    current: &Value,
    backup: &Value,
    expected: Option<&crate::worker::checkpoint::RunCheckpoint>,
) -> Result<()> {
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
    let patch = crate::runs::RunChanges {
        account_wait_reason: Some(None),
        session_id: Some(serde_json::from_value(backup["sessionId"].clone())?),
        recovery_pending: Some(!idle),
        status: Some(status),
        placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
            movement_error: Some(None),
            node_id: Some(serde_json::from_value(serde_json::json!(node))?),
            node_state: Some(serde_json::from_value(serde_json::json!(node_state))?),
            resources: Some(serde_json::from_value(serde_json::json!(
                movement["resources"]
            ))?),
            required_tags: Some(serde_json::from_value(serde_json::json!(required_tags))?),
            move_request: Some(None),
            move_reservation: Some(serde_json::from_value(serde_json::json!(reservation))?),
            ..Default::default()
        })),
        boundary: Some(
            serde_json::json!({"storage": { "mode": super::ON_DEMAND },"restoredAt": captured_at}),
        ),
        ..Default::default()
    };
    let owner = run_id.clone();
    let expected = expected.cloned();
    let movement = movement.clone();
    s.store
        .transaction(move |db| {
            let mut checkpoint = db
                .checkpoint_boundary(&owner)?
                .ok_or_else(|| Error::conflict("Missing resume checkpoint."))?;
            let current = db
                .run(&owner)?
                .ok_or_else(|| Error::not_found("Conversation removed."))?;
            if !current["cancelRequestedAt"].is_null() {
                return Err(Error::conflict("Conversation cancelled during restore."));
            }
            let saved = crate::worker::checkpoint::RunCheckpoint::load_in(db, &owner)?;
            if !crate::recovery::same_execution(saved.as_ref(), expected.as_ref())
                || current["moveRequest"] != movement
            {
                return Err(Error::conflict("Execution or movement changed during restore."));
            }
            if !idle
                && let Some(saved) = saved.as_ref()
                && saved.has_controller_recovery_reservation()
                && let Some(attempt) = saved.runner_id()
            {
                checkpoint["moveRecoveryReservation"] = attempt.into();
            }
            checkpoint["nodeId"] = node;
            checkpoint["process"] = Value::Null;
            checkpoint["settled"] = Value::Null;
            db.save_checkpoint_boundary(&owner, &checkpoint)?;
            db.change_run(&owner, patch)?;
            db.event(
                &owner, &EventKind::Status,
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

/// A terminal recovery cannot resume the interrupted turn on a destination.
/// Release only the pending destination reservation, retaining the source and
/// its checkpoint until fencing and finalization have been resolved.
pub(crate) async fn abort_terminal_recovery(
    s: &Service,
    run_id: &str,
    expected_attempt: Option<&crate::worker::checkpoint::RunCheckpoint>,
) -> Result<()> {
    let id = run_id.to_owned();
    let expected = expected_attempt.cloned();
    s.store
        .transaction(move |db| {
            let checkpoint = crate::worker::checkpoint::RunCheckpoint::load_in(db, &id)?;
            if !crate::recovery::same_execution(checkpoint.as_ref(), expected.as_ref()) {
                return Err(Error::conflict(
                    "Execution attempt changed during recovery.",
                ));
            }
            release_terminal_destination_in(db, &id)
        })
        .await
}

/// Only unclaimed destination reservations are released; retained disks and the
/// source execution stay owned by their existing fencing/storage lifecycle.
pub(crate) fn release_terminal_destination_in(db: &crate::store::Db<'_>, id: &str) -> Result<()> {
    let placement = db.run_placement(id)?;
    if placement.move_request.is_none() && placement.move_reservation.is_none() {
        return Ok(());
    }
    for reservation in [
        placement
            .move_request
            .as_ref()
            .map(|movement| movement.reservation.as_str()),
        placement.move_reservation.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        super::placement::release_in(db, reservation)?;
    }
    db.update_run_placement(
        id,
        super::run_placement::PlacementChanges {
            move_request: Some(None),
            move_reservation: Some(None),
            node_state: Some(None),
            movement_error: Some(Some(
                "Movement stopped because the execution will not resume automatically.".into(),
            )),
            ..Default::default()
        },
    )?;
    Ok(())
}
