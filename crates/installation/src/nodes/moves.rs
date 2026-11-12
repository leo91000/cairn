//! Confirmed node changes: the working environment expires and the next one starts
//! on the destination. An existing environment never moves on its own.
use super::NodeState;
use crate::{
    environment_expiration::{Expiration, WARNING},
    error::{Error, Result},
    native::policy::Origin,
    service::Service,
    store::Db,
    validation::text,
};
use cairn_contracts::{EnvironmentExpirationCause, EventKind};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio_util::sync::CancellationToken;

const MOVE_DESCRIPTION: &str = "Change this conversation to another authorized node, including a node of the other execution backend. The working environment expires: its files, unpushed changes, installed tools and engine session are deleted; the conversation history, attachments and artifacts are kept. Requires discardWorkingFiles: true. Your turn stops, then the conversation continues automatically on the destination in a fresh environment with a warning. Call list_nodes first to see ids, tags, shared budgets and free slots. The new environment waits for capacity on the destination while that node stays authorized.";
const LIST_NODES_DESCRIPTION: &str = "List authorized execution nodes with their tags, shared CPU/RAM/disk budgets, slot availability and current resource pressure. Use a returned node id with move_to_node. Resources are shared automatically; agents do not request allocations.";
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
    backend: &'a Value,
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
                backend: &node["capabilities"]["backend"],
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

/// Owner-initiated node change from the web or Android conversation controls.
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
    origin: Option<Origin>,
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
            origin: None,
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

    pub(crate) fn mark_admitted(&self) {
        self.admitted.store(true, Ordering::Release);
    }

    pub(crate) fn admitted(&self) -> bool {
        self.admitted.load(Ordering::Acquire)
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
    requested(s, run, args, true, authorize).await
}

fn validate_destination(args: &Value) -> Result<()> {
    crate::validation::uuid(text(args, "nodeId"))?;
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

/// The backend of the run's existing environment, once it has been placed.
pub(crate) fn environment_backend<'a>(run: &'a Value, checkpoint: &'a Value) -> Option<&'a str> {
    checkpoint["prepared"]["backend"].as_str().or_else(|| {
        match run["snapshot"]["agent"]["access"]["backend"].as_str() {
            Some(backend @ ("native" | "firecracker")) => Some(backend),
            _ => None,
        }
    })
}

/// Checks, in the change's transaction, that `node` can host the next environment of
/// `run`, whose snapshot already names the destination backend.
pub(crate) fn require_destination(
    db: &Db<'_>,
    run: &Value,
    node: &str,
    required_tags: &Value,
) -> Result<Value> {
    let record = db
        .node_boundary(node)?
        .ok_or_else(|| Error::not_found("Destination node not found."))?;
    let agent = db
        .agent_snapshot(super::run_agent(run))?
        .ok_or_else(|| Error::forbidden("Agent removed."))?;
    let native = crate::native::policy::node_native(&record);
    let backend = if native { "native" } else { "firecracker" };
    let profile = &crate::service::policy(&agent)["backend"];
    if record["revoked"] == true
        || !super::agent_allows(&agent, node)
        || !crate::native::policy::supports(&run["snapshot"]["agent"], backend)
        || !(profile.is_null() || profile == "auto" || profile == backend)
        || native && !crate::native::policy::allows(db, run, &record)?
    {
        return Err(Error::forbidden(
            "Destination is not authorized for this conversation.",
        ));
    }
    if required_tags
        .as_array()
        .into_iter()
        .flatten()
        .any(|tag| !super::node_tags(&record).any(|present| present == tag))
    {
        return Err(Error::conflict("Destination lacks required tags."));
    }
    Ok(record)
}

async fn requested(
    s: &Service,
    run: &Value,
    args: &Value,
    by_agent: bool,
    authorize: Authorization,
) -> Result<Value> {
    authorize.recheck(s).await?;
    let expired = run["environmentState"] == "expired";
    if ["cpu", "memoryMiB", "diskMiB"]
        .iter()
        .any(|key| args.get(key).is_some())
    {
        return Err(Error::bad(
            "Choose a node; resource requests are no longer supported.",
        ));
    }
    validate_destination(args)?;
    if !expired {
        crate::environment_expiration::require_discard(
            args["discardWorkingFiles"] == true,
            WARNING,
        )?;
    }

    let run_id = text(run, "id").to_owned();
    let node = text(args, "nodeId").to_owned();
    let checkpoint = super::checkpoint(s, &run_id).await?;
    let current = checkpoint["nodeId"].as_str().or(run["nodeId"].as_str());
    if !expired && current == Some(node.as_str()) {
        return Err(Error::conflict(
            "This conversation already runs on that node.",
        ));
    }
    let destination = s.node_boundary(&node).await?;
    let target = destination["capabilities"]["backend"]
        .as_str()
        .unwrap_or("firecracker")
        .to_owned();
    let crossing = environment_backend(run, if expired { &Value::Null } else { &checkpoint })
        .is_some_and(|from| from != target);
    if crossing && target == "native" {
        crate::native::policy::refresh(s).await?;
    }
    let source = if by_agent {
        Origin::Stored(run.clone())
    } else {
        authorize
            .origin
            .clone()
            .unwrap_or_else(crate::native::policy::origin)
    };
    let continue_turn = by_agent || matches!(&source, Origin::Run(id) if *id == run_id);
    let supplied_tags = args
        .get("requiredTags")
        .filter(|tags| !tags.is_null())
        .cloned();
    let expiration = Expiration {
        cause: EnvironmentExpirationCause::NodeChange,
        destination: Some(node.clone()),
        continue_turn,
    };
    let (owner, chosen) = (run_id.clone(), node.clone());
    let check = authorize.clone();
    let prepare = move |db: &Db<'_>| {
        check.check(db)?;
        crate::conversation_lifecycle::require_active_run(db, &owner)?;
        let mut current = db
            .run(&owner)?
            .ok_or_else(|| Error::not_found("Conversation removed."))?;
        if (current["environmentState"] == "expired") != expired {
            return Err(Error::conflict(
                "The working environment changed. Review the node change again.",
            ));
        }
        let required_tags = supplied_tags
            .as_ref()
            .unwrap_or(&current["requiredTags"])
            .clone();
        let grant = if crossing {
            let mut agent = db
                .agent_snapshot(super::run_agent(&current))?
                .ok_or_else(|| Error::forbidden("Agent removed."))?;
            agent["access"]["backend"] = target.clone().into();
            current = crate::backend_migration::prospective(db, &current, &agent, &source)?;
            let mut grant = json!({});
            crate::native::policy::apply(&mut grant, &current);
            Some(grant)
        } else if by_agent {
            None
        } else {
            Some(crate::native::policy::run_grant(db, &current, &source)?)
        };
        if let Some(grant) = &grant {
            crate::native::policy::apply(&mut current, grant);
        }
        if crossing {
            current["snapshot"]["agent"]["access"]["backend"] = target.clone().into();
        }
        let record = require_destination(db, &current, &chosen, &required_tags)?;
        if let Some(grant) = &grant {
            if let Some(mut chat) = crate::chats::chat_for_run(db, &owner)? {
                crate::native::policy::apply(&mut chat, grant);
                db.save_conversation_boundary(&chat)?;
            }
            db.update_run_boundary(&owner, grant)?;
        }
        db.change_run(
            &owner,
            crate::runs::RunChanges {
                snapshot: crossing
                    .then(|| serde_json::from_value(current["snapshot"].clone()))
                    .transpose()?,
                placement: Some(Box::new(super::run_placement::PlacementChanges {
                    target_node_id: expired.then(|| Some(chosen.clone())),
                    pinned_node_id: expired.then_some(None),
                    preferred_node_id: expired.then_some(None),
                    required_tags: Some(serde_json::from_value(required_tags.clone())?),
                    execution_backend: crossing.then(|| Some(target.clone())),
                    ..Default::default()
                })),
                ..Default::default()
            },
        )?;
        if expired {
            db.update_run_boundary(
                &owner,
                &json!({ "environmentAdmissionRevision": crate::config::id() }),
            )?;
        }
        let name = record["name"].as_str().unwrap_or("another node");
        // The owner sees in the conversation that the agent moved itself.
        let message = if continue_turn {
            format!("The agent changed this conversation's node to {name}.")
        } else {
            format!("Changing this conversation's node to {name}.")
        };
        db.event(&owner, &EventKind::Status, &message, None)?;
        db.audit(
            "node.move.requested",
            &json!({ "runId": owner, "nodeId": chosen, "crossing": crossing }),
        )
    };
    if expired {
        s.store.transaction(move |db| prepare(db)).await?;
        s.worker.notify();
    } else {
        s.expire_environment(&run_id, expiration, prepare).await?;
    }
    authorize.mark_admitted();
    Ok(json!({
        "status": "moved",
        "nodeId": node,
    }))
}

const WAITING_TITLE: &str = "Conversation waiting for its node";

/// Whether an existing environment can resume on its node now. It never moves
/// elsewhere: an unavailable node, or a node its agent lost, keeps it waiting.
pub async fn node_ready(s: &Service, run: &Value) -> Result<bool> {
    let checkpoint =
        crate::worker::checkpoint::RunCheckpoint::load(&s.store, text(run, "id")).await?;
    node_ready_for_attempt(s, run, checkpoint.as_ref()).await
}

pub(crate) async fn node_ready_for_attempt(
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
        return Ok(true);
    }
    let checkpoint = super::checkpoint(s, run_id).await?;
    let source = text(&checkpoint, "nodeId");
    if source.is_empty() {
        return Ok(true);
    }
    if source == super::LOCAL_NODE_ID {
        // A stale local record only delays recovery until the next attempt.
        let _ = super::refresh_local(s).await;
    }
    let node = s.node_boundary(source).await?;
    if super::native_mode::unconfirmed(&node) {
        return Ok(false);
    }
    let agent = s.agent_snapshot(super::run_agent(run)).await?;
    let authorized = super::agent_allows(&agent, source);
    let available = super::seen_within(&node, super::HEARTBEAT_TIMEOUT_MS)
        && node["revoked"] != true
        && node["executionReady"] == true;
    if available && authorized {
        return Ok(true);
    }
    if run["nodeState"] != "waiting-for-node" {
        let patch = crate::runs::RunChanges {
            placement: Some(Box::new(super::run_placement::PlacementChanges {
                node_state: Some(serde_json::from_value(serde_json::json!(
                    NodeState::WaitingForNode
                ))?),
                ..Default::default()
            })),
            ..Default::default()
        };
        s.store.change_run(run_id, patch).await?;
    }
    let body = if authorized {
        "The node holding this conversation's working environment is unavailable. It resumes when that node returns, or after a confirmed node change."
    } else {
        "This conversation's agent no longer has access to the node holding its working environment. It stays stopped until that access returns, or after a confirmed node change."
    };
    super::alerts::raise(s, run_id, "waiting", WAITING_TITLE, body).await?;
    Ok(false)
}

/// Moves and conversions of an earlier version cannot finish: the environment stays
/// on its source and the destination's capacity is returned.
pub(crate) fn abandon_legacy_moves(db: &Db<'_>) -> Result<()> {
    let runs = db.json_rows(
        "SELECT data FROM run_views AS runs
         WHERE placement_move_request IS NOT NULL OR placement_move_reservation IS NOT NULL",
        [],
    )?;
    for run in runs {
        let id = text(&run, "id");
        let placement = db.run_placement(id)?;
        for reservation in [
            placement
                .move_request
                .as_ref()
                .map(|movement| movement.reservation.clone()),
            placement.move_reservation.clone(),
        ]
        .into_iter()
        .flatten()
        {
            super::placement::release_in(db, &reservation)?;
        }
        let mut changes = crate::runs::RunChanges {
            placement: Some(Box::new(super::run_placement::PlacementChanges {
                move_request: Some(None),
                move_reservation: Some(None),
                node_state: Some(None),
                movement_error: Some(Some(
                    "This move was stopped by an update. Changing node now discards the working files; confirm a new node change to continue elsewhere.".into(),
                )),
                ..Default::default()
            })),
            ..Default::default()
        };
        // An idle move had queued the stopped conversation only to transfer it.
        if let Some(movement) = placement.move_request.as_ref()
            && movement.idle == Some(true)
        {
            changes.status = Some(
                movement
                    .idle_status
                    .unwrap_or(crate::run_status::RunStatus::Succeeded),
            );
            changes.recovery_pending = Some(false);
        }
        db.change_run(id, changes)?;
    }
    Ok(())
}
