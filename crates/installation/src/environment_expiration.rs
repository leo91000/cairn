//! Expiration of a conversation's working environment (ADR-0083, local conversation
//! disks). The conversation keeps its history, attachments and artifacts; its next
//! turn starts an empty environment. Expiration detaches the old environment in one
//! transaction. Its physical retirement continues from a durable queue: it never
//! blocks messages or a node change, only the admission of the next environment.
use crate::{
    config::{id, now},
    error::{Error, Result},
    recovery::ProcessIdentity,
    run_status::RunStatus,
    service::Service,
    store::Db,
    validation::text,
    worker::checkpoint::RunCheckpoint,
};
use cairn_contracts::{
    ConversationEnvironmentState, EnvironmentExpirationCause as Cause, EventKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const WARNING: &str = "This conversation's working files will be lost. Its history, attachments and artifacts are kept.";
pub const NODE_WARNING: &str = "The working files of this node's conversations will be lost. Their history, attachments and artifacts are kept.";

/// A fresh placement choice invalidates any preparation that captured its predecessor.
pub(crate) fn require_admission_revision(run: &Value, expected: &Value) -> Result<()> {
    if run["environmentAdmissionRevision"] != *expected {
        return Err(Error::unavailable(format!(
            "{} can admit the previous placement selection: it changed during preparation. Retrying the current destination.",
            crate::nodes::placement::NO_CAPACITY
        )));
    }
    Ok(())
}

/// Expirations waiting for the previous execution to stop, keyed by run.
const PENDING: &str = "environment-expirations";
/// Detached environments whose physical cleanup has not finished.
const RETIREMENTS: &str = "environment-retirements";
const RETRY_MS: i64 = 60_000;

/// The distinct confirmation of every operation that discards working files.
pub(crate) fn require_discard(confirmed: bool, warning: &str) -> Result<()> {
    if confirmed {
        return Ok(());
    }
    Err(Error::conflict(format!(
        "{warning} Repeat with discardWorkingFiles: true to confirm."
    )))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Expiration {
    pub cause: Cause,
    /// Node imposed on the first admission of the next environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// The conversation's own agent changed node: its turn continues there.
    #[serde(default)]
    pub continue_turn: bool,
}

impl Expiration {
    pub(crate) fn new(cause: Cause) -> Self {
        Self {
            cause,
            destination: None,
            continue_turn: false,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pending {
    id: String,
    run_id: String,
    expired_at: i64,
    #[serde(flatten)]
    expiration: Expiration,
    /// Detached environment metadata while the interrupted worker finishes fencing.
    #[serde(default)]
    environment: Value,
    #[serde(default)]
    cancelled: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetiredAttempt {
    id: String,
    node_id: String,
}

/// What the retired environment still occupies, captured when it was detached.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Retirement {
    id: String,
    run_id: String,
    cause: Option<Cause>,
    expired_at: i64,
    /// Nodes that may still hold a disk of the retired environment.
    nodes: Vec<String>,
    attempt: Option<String>,
    attempt_node: Option<String>,
    /// Includes admission that materialized before checkpoint publication failed.
    #[serde(default)]
    attempts: Vec<RetiredAttempt>,
    process: Option<ProcessIdentity>,
    isolated: bool,
    account_id: Option<String>,
    workspaces: Vec<Value>,
    projects: Vec<Value>,
    /// The previous execution is confirmed stopped, or its node lease lapsed.
    fenced: bool,
    /// Manager namespaces and recovery/account references are logically detached.
    local: bool,
    #[serde(default)]
    publications: crate::nodes::publication::RetiredPublications,
    /// Physical erasure may keep retrying after the new environment is admitted.
    #[serde(default)]
    cleaned: bool,
    error: Option<String>,
    retry_after: Option<i64>,
}

/// Why the next environment of `run` cannot start yet, if it cannot.
pub fn admission_blocked(db: &Db<'_>, run: &str) -> Result<bool> {
    if db.get(PENDING, run)?.is_some() {
        return Ok(true);
    }
    Ok(retirements(db, run)?
        .iter()
        .any(|retirement| !retirement.fenced || !retirement.local))
}

/// Nodes that must delete the retired disk of `run` before admitting its next environment.
pub fn retiring_nodes(db: &Db<'_>, run: &str) -> Result<Vec<String>> {
    Ok(retirements(db, run)?
        .into_iter()
        .flat_map(|retirement| retirement.nodes)
        .collect())
}

pub fn pending(db: &Db<'_>, run: &str) -> Result<bool> {
    Ok(db.get(PENDING, run)?.is_some())
}

/// Runs whose working environment is on `node`.
pub(crate) fn runs_on_node(db: &Db<'_>, node: &str) -> Result<Vec<String>> {
    let mut runs =
        db.0.prepare_cached(
            "SELECT runs.id FROM runs LEFT JOIN run_checkpoints ON run_checkpoints.run_id=runs.id
             WHERE (runs.placement_node_id=?1 OR json_extract(run_checkpoints.state,'$.nodeId')=?1
                OR EXISTS(SELECT 1 FROM placements p JOIN attempts a ON a.id=p.id
                    WHERE p.run_id=runs.id AND p.node_id=?1 AND p.role='execution' AND a.released=0))
               AND json_type(runs.boundary,'$.environmentState') IS NOT 'text'",
        )?
        .query_map([node], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    runs.sort();
    runs.dedup();
    Ok(runs)
}

/// A revoked node's environments expire with it. `idle` names the runs whose
/// previous executions already stopped.
pub(crate) fn expire_node_in(
    db: &Db<'_>,
    node: &str,
    idle: &std::collections::HashSet<String>,
) -> Result<Vec<String>> {
    let mut expired = Vec::new();
    for run in runs_on_node(db, node)? {
        if db.get(PENDING, &run)?.is_some()
            || crate::conversation_lifecycle::require_active_run(db, &run).is_err()
        {
            continue;
        }
        request_in(
            db,
            &run,
            &Expiration::new(Cause::NodeRevocation),
            !idle.contains(&run),
        )?;
        expired.push(run);
    }
    Ok(expired)
}

/// Explicit Stop wins over the automatic continuation recorded by a self move or mission.
pub(crate) fn cancel_continuation_in(db: &Db<'_>, run: &str) -> Result<()> {
    if let Some(mut pending) = db.get(PENDING, run)? {
        pending["continueTurn"] = false.into();
        pending["cancelled"] = true.into();
        db.put(PENDING, &pending)?;
    }
    Ok(())
}

/// Runs whose expiration waits for their interrupted execution to stop.
pub(crate) async fn pending_runs(s: &Service) -> Result<std::collections::HashSet<String>> {
    Ok(s.store
        .list(PENDING)
        .await?
        .iter()
        .map(|pending| text(pending, "runId").to_owned())
        .collect())
}

/// Captured worktree registrations must also be removed by explicit deletion.
pub(crate) fn detached_worktrees(db: &Db<'_>, run: &str) -> Result<Vec<(Vec<Value>, Vec<Value>)>> {
    let mut environments = retirements(db, run)?
        .into_iter()
        .filter(|retirement| !retirement.local)
        .map(|retirement| (retirement.workspaces, retirement.projects))
        .collect::<Vec<_>>();
    if let Some(pending) = db.get(PENDING, run)? {
        let environment = &pending["environment"];
        environments.push((
            environment["workspaces"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            crate::service::run_projects(environment),
        ));
    }
    Ok(environments)
}

/// A purged conversation deletes everything of its run, including retired environments.
pub(crate) fn forget_in(db: &Db<'_>, run: &str) -> Result<()> {
    db.remove(PENDING, run)?;
    for retirement in retirements(db, run)? {
        db.remove(RETIREMENTS, &retirement.id)?;
    }
    Ok(())
}

fn retirements(db: &Db<'_>, run: &str) -> Result<Vec<Retirement>> {
    db.list(RETIREMENTS)?
        .into_iter()
        .filter(|retirement| retirement["runId"] == run)
        .map(|retirement| Ok(serde_json::from_value(retirement)?))
        .collect()
}

/// Detaches the environment immediately. A live worker retains its execution
/// checkpoint solely to finish stopping; admission waits for that fence.
pub(crate) fn request_in(
    db: &Db<'_>,
    run_id: &str,
    expiration: &Expiration,
    live: bool,
) -> Result<()> {
    let run = crate::error::required(db.run(run_id)?, "Run not found")?;
    if db.get(PENDING, run_id)?.is_some() {
        return Err(Error::conflict(
            "The working environment is already being retired.",
        ));
    }
    let expired_at = now();
    crate::chats::questions::close_open(db, run_id)?;
    if let Some(mut chat) = db.conversation_for_run(run_id)? {
        chat.environment_state = Some(ConversationEnvironmentState::Expired);
        chat.environment_expired_at = Some(expired_at);
        chat.environment_error = None;
        chat.environment_retry_after = None;
        db.save_conversation(&chat)?;
    }
    db.update_run_boundary(
        run_id,
        &json!({
            "environmentState": "expired",
            "environmentExpiredAt": expired_at,
            "environmentExpirationCause": expiration.cause,
            "environmentAdmissionRevision": id(),
        }),
    )?;
    db.revoke_mcp_run(run_id)?;
    let executing = matches!(
        RunStatus::of(&run),
        Some(RunStatus::Queued | RunStatus::Running)
    );
    // An active mission resumes on its own; a scheduled wait keeps its continuation.
    let stopped = !run["cancelRequestedAt"].is_null();
    let mut expiration = expiration.clone();
    expiration.continue_turn = !stopped
        && (expiration.continue_turn
            || run["trigger"] != "chat" && executing && !run["wakeup"].is_object());
    if !live {
        return detach_in(db, run_id, &expiration, expired_at);
    }
    if run["cancelRequestedAt"].is_null() {
        db.change_run(
            run_id,
            crate::runs::RunChanges {
                cancel_requested_at: Some(Some(now())),
                ..Default::default()
            },
        )?;
    }
    db.event(
        run_id,
        &EventKind::Status,
        &format!(
            "Environment expired {}. {WARNING}",
            expiration.cause.reason()
        ),
        Some(&json!({
            "environmentExpiredAt": expired_at,
            "environmentExpirationCause": expiration.cause,
        })),
    )?;
    let pending = Pending {
        id: run_id.to_owned(),
        run_id: run_id.to_owned(),
        expired_at,
        expiration: expiration.clone(),
        environment: run,
        cancelled: stopped,
    };
    db.change_run(
        run_id,
        crate::runs::RunChanges {
            session_id: Some(None),
            resume_available: Some(false),
            workspace: Some(None),
            placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
                node_id: Some(None),
                target_node_id: Some(expiration.destination),
                pinned_node_id: (expiration.cause != Cause::NodeRevocation).then_some(None),
                preferred_node_id: (expiration.cause != Cause::NodeRevocation).then_some(None),
                ..Default::default()
            })),
            boundary: Some(json!({ "workspaces": [], "backup": null, "storage": null, "environmentRetirementId":null })),
            ..Default::default()
        },
    )?;
    db.put(PENDING, &serde_json::to_value(pending)?)?;
    Ok(())
}

/// Applies an expiration recorded while its execution was still live.
fn detach_pending_in(db: &Db<'_>, run_id: &str) -> Result<()> {
    let Some(pending) = db.get(PENDING, run_id)? else {
        return Ok(());
    };
    let pending: Pending = serde_json::from_value(pending)?;
    let current = crate::error::required(db.run(run_id)?, "Run not found")?;
    let mut expiration = pending.expiration;
    if pending.cancelled {
        expiration.continue_turn = false;
    } else if current["trigger"] != "chat" && pending.environment["wakeup"].is_object() {
        // Expiration interrupts the worker, whose ordinary cancellation settlement
        // clears its wait. Restore that continuation once this worker has stopped.
        db.change_run(
            run_id,
            crate::runs::RunChanges {
                status: Some(RunStatus::Waiting),
                wakeup: Some(Some(serde_json::from_value(
                    pending.environment["wakeup"].clone(),
                )?)),
                cancel_requested_at: Some(None),
                ..Default::default()
            },
        )?;
    } else if db
        .conversation_for_run(run_id)?
        .is_some_and(|chat| chat.wakeup.is_some())
    {
        db.change_run(
            run_id,
            crate::runs::RunChanges {
                status: Some(RunStatus::Succeeded),
                cancel_requested_at: Some(None),
                ..Default::default()
            },
        )?;
    }
    expiration.destination = current["targetNodeId"].as_str().map(str::to_owned);
    detach_environment_in(
        db,
        run_id,
        &expiration,
        pending.expired_at,
        Some(pending.environment),
    )
}

/// Releases the old environment from the run and queues its physical retirement.
fn detach_in(db: &Db<'_>, run_id: &str, expiration: &Expiration, expired_at: i64) -> Result<()> {
    detach_environment_in(db, run_id, expiration, expired_at, None)
}

fn detach_environment_in(
    db: &Db<'_>,
    run_id: &str,
    expiration: &Expiration,
    expired_at: i64,
    environment: Option<Value>,
) -> Result<()> {
    let stopped_live_worker = environment.is_some();
    db.remove(PENDING, run_id)?;
    let Some(run) = db.run(run_id)? else {
        return Ok(());
    };
    crate::nodes::placement::release_unclaimed(db, Some(run_id))?;
    let environment = environment
        .filter(Value::is_object)
        .unwrap_or_else(|| run.clone());
    let checkpoint = RunCheckpoint::load_in(db, run_id)?.unwrap_or_default();
    let placement = db.run_placement(run_id)?;

    let mut nodes = db
        .list("node-volumes")?
        .into_iter()
        .filter(|volume| volume["runId"] == run_id)
        .map(|volume| text(&volume, "nodeId").to_owned())
        .collect::<Vec<_>>();
    for node in [
        checkpoint.node_id.clone().flatten(),
        environment["nodeId"].as_str().map(str::to_owned),
    ]
    .into_iter()
    .flatten()
    {
        if !nodes.contains(&node) {
            nodes.push(node);
        }
    }
    let attempts = db
        .attempts()?
        .into_iter()
        .filter(|attempt| {
            attempt.run_id == run_id
                && !attempt.released
                && attempt.role == crate::nodes::placement::AttemptRole::Execution
        })
        .map(|attempt| RetiredAttempt {
            id: attempt.id,
            node_id: attempt.node_id,
        })
        .collect::<Vec<_>>();
    for attempt in &attempts {
        if !nodes.contains(&attempt.node_id) {
            nodes.push(attempt.node_id.clone());
        }
    }
    let retirement = Retirement {
        id: id(),
        run_id: run_id.to_owned(),
        cause: Some(expiration.cause),
        expired_at,
        nodes,
        attempt: checkpoint.runner_id().map(str::to_owned),
        attempt_node: checkpoint.node_id.clone().flatten(),
        attempts,
        process: checkpoint.process().cloned(),
        isolated: run["isolated"] == true || checkpoint.isolated(),
        account_id: run["accountId"].as_str().map(str::to_owned),
        workspaces: environment["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|workspace| workspace["kind"] == "worktree")
            .cloned()
            .collect(),
        projects: crate::service::run_projects(&environment)
            .into_iter()
            .map(|project| json!({ "id": project["id"], "path": project["path"] }))
            .collect(),
        ..Default::default()
    };
    let retirement_id = retirement.id.clone();
    db.put(RETIREMENTS, &serde_json::to_value(retirement)?)?;

    // Unclaimed destinations of an interrupted earlier move are not part of any environment.
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
        crate::nodes::placement::release_in(db, &reservation)?;
    }

    let chat = db.conversation_for_run(run_id)?;
    let mission = run["trigger"] != "chat";
    let interrupted = RunStatus::of(&run).is_some_and(RunStatus::is_active);
    let waiting = chat.as_ref().is_some_and(|chat| chat.wakeup.is_some());
    let chat_ready = chat.as_ref().is_none_or(|chat| {
        chat.lifecycle == cairn_contracts::ConversationLifecycle::Active && !chat.paused
    });
    let resume =
        expiration.continue_turn && chat_ready && (mission || run["chatExecution"].is_object());
    let node_change = expiration.cause == Cause::NodeChange;
    let keep_pins = expiration.cause == Cause::NodeRevocation;

    let mut boundary = json!({
        "environmentState": "expired",
        "environmentExpiredAt": expired_at,
        "environmentExpirationCause": expiration.cause,
        "environmentRetirementId": retirement_id,
        // The retirement fences the old execution. Recovery must not contact
        // its node before the next environment is placed.
        "isolated": false,
        "backup": null,
        "storage": null,
        "requiredRuntimeId": null,
        "requiredRuntime": null,
        "runtimeId": null,
        "workspaces": [],
        "restoredAt": null,
        "contextUsage": null,
        "backendMigration": null,
        "backendMigrationError": null,
        "resumeMissionAfterConversion": null,
    });
    let mut changes = crate::runs::RunChanges {
        session_id: Some(None),
        resume_available: Some(false),
        workspace: Some(None),
        workspace_cleaned_at: Some(None),
        recovery_pending: Some(false),
        capacity_wait_until: Some(None),
        account_wait_reason: Some(None),
        placement: Some(Box::new(crate::nodes::run_placement::PlacementChanges {
            node_id: Some(None),
            target_node_id: Some(expiration.destination.clone()),
            preferred_node_id: (!keep_pins && !stopped_live_worker).then_some(None),
            pinned_node_id: (!keep_pins && !stopped_live_worker).then_some(None),
            node_state: Some(None),
            movement_error: Some(None),
            move_request: Some(None),
            move_reservation: Some(None),
            ..Default::default()
        })),
        ..Default::default()
    };
    let mut fresh = RunCheckpoint {
        completed: Some(true),
        ..Default::default()
    };
    if mission {
        fresh.remaining_ms = checkpoint.remaining_ms;
        boundary["sessionHandoff"] = crate::chats::handoff_context(db, run_id)?.into();
    }
    if resume {
        fresh.completed = Some(false);
        fresh.launched = Some(false);
        if !mission {
            fresh.turn_pending = Some(true);
            fresh.remaining_ms = Some(crate::run_limits::budget_ms(&run["snapshot"]["agent"]));
            let mut execution = run["chatExecution"].clone();
            execution["context"] = crate::chats::handoff_context(db, run_id)?.into();
            execution["recovery"] = false.into();
            boundary["chatExecution"] = execution;
        }
        changes.status = Some(RunStatus::Queued);
        changes.cancel_requested_at = Some(None);
        changes.error = Some(None);
        changes.finished_at = Some(None);
        changes.retry = Some(None);
    } else if interrupted && waiting {
        // The turn already ended with a scheduled wait, which keeps its continuation.
        changes.status = Some(RunStatus::Succeeded);
        changes.cancel_requested_at = Some(None);
    } else if interrupted && !(mission && run["wakeup"].is_object()) {
        changes.status = Some(RunStatus::Cancelled);
        changes.finished_at = Some(Some(now()));
        changes.retry = Some(None);
        changes.summary = Some(format!(
            "Stopped: working environment removed {}.",
            expiration.cause.reason()
        ));
    }
    changes.boundary = Some(boundary);
    db.change_run(run_id, changes)?;
    fresh.store_in(db, run_id)?;

    let destination = expiration
        .destination
        .as_deref()
        .and_then(|node| db.node_boundary(node).ok().flatten())
        .and_then(|node| node["name"].as_str().map(str::to_owned));
    let next = if resume {
        "The conversation continues in a fresh environment"
    } else if mission {
        "The next run starts a fresh environment"
    } else {
        "The next message starts a fresh environment"
    };
    let next = match (&destination, node_change) {
        (Some(name), true) => format!("{next} on {name}."),
        _ => format!("{next}."),
    };
    db.event(
        run_id,
        &EventKind::Status,
        &format!(
            "Environment removed {}. Conversation history, uploads and published artifacts are retained. {next}",
            expiration.cause.reason()
        ),
        Some(&json!({
            "environmentExpiredAt": expired_at,
            "environmentExpirationCause": expiration.cause,
        })),
    )?;

    if let Some(mut chat) = chat {
        chat.environment_state = if resume {
            None
        } else {
            Some(ConversationEnvironmentState::Expired)
        };
        chat.environment_expired_at = Some(expired_at);
        chat.environment_error = None;
        chat.environment_retry_after = None;
        if resume {
            chat.cancelled_by_deletion = false;
            chat.session_restart_requested = false;
        }
        db.save_conversation(&chat)?;
    }
    db.revoke_mcp_run(run_id)?;
    db.audit(
        "chat.environment.expired",
        &json!({ "runId": run_id, "cause": expiration.cause, "nodeId": expiration.destination }),
    )
}

/// Conversations left removing their environment by an earlier version expire now,
/// with their previous expiration time and inactivity as cause.
pub(crate) fn finish_legacy_cleaning(db: &Db<'_>) -> Result<()> {
    for chat in db.conversations()? {
        if chat.environment_state != Some(ConversationEnvironmentState::Cleaning) {
            continue;
        }
        let expired_at = chat.environment_expired_at.unwrap_or_else(now);
        match chat.run_id.as_deref() {
            Some(run) if db.run(run)?.is_some() => {
                detach_in(db, run, &Expiration::new(Cause::Inactivity), expired_at)?;
            }
            _ => {
                let mut chat = chat;
                chat.environment_state = Some(ConversationEnvironmentState::Expired);
                chat.environment_error = None;
                chat.environment_retry_after = None;
                db.save_conversation(&chat)?;
            }
        }
    }
    Ok(())
}

impl Service {
    /// Expires the environment of `run` now, interrupting its live execution first.
    pub(crate) async fn expire_environment<F>(
        &self,
        run: &str,
        expiration: Expiration,
        authorize: F,
    ) -> Result<()>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let preparation = self.worker.run_preparation_lock(run).await;
        let idle = preparation.try_lock_owned().ok();
        let active = self.worker.active.lock().await;
        let live = idle.is_none() || active.contains_key(run);
        let owner = run.to_owned();
        self.store
            .transaction(move |db| {
                authorize(db)?;
                request_in(db, &owner, &expiration, live)
            })
            .await?;
        if let Some(cancel) = active.get(run) {
            cancel.cancel();
        }
        drop(active);
        drop(idle);
        self.worker.notify_retirement();
        self.worker.notify();
        Ok(())
    }

    /// Detaches environments whose interrupted execution has stopped since its expiration.
    pub async fn detach_stopped_environments(&self) -> Result<()> {
        for pending in self.store.list(PENDING).await? {
            let run = text(&pending, "runId").to_owned();
            let preparation = self.worker.run_preparation_lock(&run).await;
            let Ok(_preparation) = preparation.try_lock_owned() else {
                continue;
            };
            let active = self.worker.active.lock().await;
            if active.contains_key(&run) {
                continue;
            }
            self.store
                .transaction(move |db| detach_pending_in(db, &run))
                .await?;
            drop(active);
        }
        Ok(())
    }

    /// Manual whole-disk cleanup shares the durable admission barrier and request
    /// identity of expiration. Never send an independent unqualified deletion.
    pub(crate) async fn retire_stale_disk(&self, run: &str, node: &str) -> Result<()> {
        let (run, node) = (run.to_owned(), node.to_owned());
        let requested_node = node.clone();
        let retirement = self
            .store
            .transaction(move |db| {
                let current = db.run(&run)?.unwrap_or_default();
                let checkpoint = RunCheckpoint::load_in(db, &run)?.unwrap_or_default();
                let occupied = current["nodeId"] == node
                    || checkpoint.node_id.as_ref().and_then(|node| node.as_deref())
                        == Some(node.as_str())
                    || db.attempts()?.iter().any(|attempt| {
                        attempt.run_id == run
                            && attempt.node_id == node
                            && !attempt.released
                            && attempt.role == crate::nodes::placement::AttemptRole::Execution
                    });
                if occupied {
                    return Err(Error::conflict(
                        "The conversation still owns this node's disk.",
                    ));
                }
                if let Some(retirement) = retirements(db, &run)?
                    .into_iter()
                    .find(|retirement| retirement.nodes.contains(&node))
                {
                    return Ok(retirement.id);
                }
                let volume = format!("{run}:{node}");
                if db.get("node-volumes", &volume)?.is_none() {
                    return Err(Error::conflict(
                        "The stale disk inventory changed; refresh before retrying.",
                    ));
                }
                let retirement = Retirement {
                    id: id(),
                    run_id: run,
                    nodes: vec![node],
                    expired_at: now(),
                    isolated: current["isolated"] == true,
                    fenced: true,
                    local: true,
                    ..Default::default()
                };
                db.put(RETIREMENTS, &serde_json::to_value(&retirement)?)?;
                Ok(retirement.id)
            })
            .await?;
        self.worker.notify_retirement();
        self.retire_environments().await?;
        if self
            .store
            .get(RETIREMENTS, &retirement)
            .await?
            .is_some_and(|retirement| {
                retirement["nodes"]
                    .as_array()
                    .is_some_and(|nodes| nodes.iter().any(|node| node == &requested_node))
            })
        {
            return Err(Error::unavailable(
                "The stale disk retirement is queued and will retry in the background.",
            ));
        }
        Ok(())
    }

    /// Fencing and logical namespace detachment run independently of slow erasure.
    pub(crate) async fn detach_retired_environments(&self) -> Result<()> {
        for value in self.store.list(RETIREMENTS).await? {
            let mut retirement: Retirement = serde_json::from_value(value)?;
            if retirement.local || retirement.retry_after.unwrap_or(0) > now() {
                continue;
            }
            let preparation = self.worker.run_preparation_lock(&retirement.run_id).await;
            let Ok(_preparation) = preparation.try_lock_owned() else {
                continue;
            };
            let Some(current) = self.store.get(RETIREMENTS, &retirement.id).await? else {
                continue;
            };
            retirement = serde_json::from_value(current)?;
            if retirement.local {
                continue;
            }
            if let Err(error) = self.detach_retirement(&mut retirement).await {
                self.store
                    .transaction(move |db| {
                        save_progress(db, retirement, Some(error.message), false)
                    })
                    .await?;
            }
        }
        Ok(())
    }

    /// Convenience for explicit cleanup and tests; the worker uses two independent loops.
    pub async fn retire_environments(&self) -> Result<()> {
        self.detach_retired_environments().await?;
        self.cleanup_retired_environments().await
    }

    /// Physical retirement cannot hold the logical-detachment loop or admission.
    pub(crate) async fn cleanup_retired_environments(&self) -> Result<()> {
        let _cleanup = self.worker.retirement_cleanup.lock().await;
        let mut due = self
            .store
            .list(RETIREMENTS)
            .await?
            .into_iter()
            .map(serde_json::from_value::<Retirement>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        due.retain(|retirement| {
            retirement.local && retirement.fenced && retirement.retry_after.unwrap_or(0) <= now()
        });
        due.sort_by_key(|retirement| retirement.expired_at);
        for mut retirement in due {
            if self.shutdown.is_cancelled() {
                break;
            }
            let error = self
                .retire(&mut retirement)
                .await
                .err()
                .map(|error| error.message);
            let done = error.is_none() && retirement.nodes.is_empty() && retirement.cleaned;
            self.store
                .transaction(move |db| save_progress(db, retirement, error, done))
                .await?;
        }
        self.worker.notify();
        Ok(())
    }

    /// Explicit purge still waits for every detached namespace to be erased.
    /// The caller owns the retirement cleanup gate and has fenced all disks/ACKs.
    pub(crate) async fn purge_retired_resources(&self, run: &str) -> Result<()> {
        let owned = run.to_owned();
        for retirement in self.store.read(move |db| retirements(db, &owned)).await? {
            let directory = crate::conversation_deletion::retired::directory(self, &retirement.id);
            if tokio::fs::try_exists(directory).await? {
                crate::conversation_deletion::retired::erase(
                    self,
                    run,
                    &retirement.id,
                    &retirement.workspaces,
                    &retirement.projects,
                    &retirement.publications,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn detach_retirement(&self, retirement: &mut Retirement) -> Result<()> {
        if !retirement.fenced {
            crate::recovery::fence_retired(
                self,
                retirement.attempt.as_deref(),
                retirement.attempt_node.as_deref(),
                retirement.process.as_ref(),
                retirement.isolated,
            )
            .await?;
            for attempt in &retirement.attempts {
                if retirement.attempt.as_deref() == Some(attempt.id.as_str()) {
                    continue;
                }
                crate::recovery::fence_retired(
                    self,
                    Some(&attempt.id),
                    Some(&attempt.node_id),
                    None,
                    true,
                )
                .await?;
            }
            retirement.fenced = true;
        }
        if !retirement.local {
            let run = retirement.run_id.clone();
            let _publication = self.node_backup_operation.lock(&run).await;
            let _readers = self.node_backup_operation.drain(&run).await;
            crate::conversation_deletion::retired::detach(self, &run, &retirement.id).await?;
            let mut detached = retirement.clone();
            *retirement = self
                .store
                .transaction(move |db| {
                    detached.publications =
                        crate::nodes::publication::detach_environment_in(db, &run)?;
                    db.remove_account_lease(&run)?;
                    db.update_run_boundary(&run, &json!({ "backup":null, "storage":null }))?;
                    detached.local = true;
                    db.put(RETIREMENTS, &serde_json::to_value(&detached)?)?;
                    Ok(detached)
                })
                .await?;
            // This commit opens admission, independently of physical cleanup.
            self.worker.notify();
            self.worker.notify_retirement();
        }
        Ok(())
    }

    async fn retire(&self, retirement: &mut Retirement) -> Result<()> {
        let mut failure = None;
        let mut remaining = Vec::new();
        for node in std::mem::take(&mut retirement.nodes) {
            if let Err(error) = crate::conversation_deletion::delete_retired_disk(
                self,
                &retirement.run_id,
                &node,
                retirement.isolated,
                &retirement.id,
            )
            .await
            {
                failure.get_or_insert(error);
                remaining.push(node);
            }
        }
        retirement.nodes = remaining;
        let released = serde_json::to_value(&retirement)?;
        self.store
            .write(move |db| db.put(RETIREMENTS, &released))
            .await?;
        self.worker.notify();
        if !retirement.cleaned {
            match crate::conversation_deletion::retired::erase(
                self,
                &retirement.run_id,
                &retirement.id,
                &retirement.workspaces,
                &retirement.projects,
                &retirement.publications,
            )
            .await
            {
                Ok(()) => retirement.cleaned = true,
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

fn save_progress(
    db: &Db<'_>,
    mut retirement: Retirement,
    error: Option<String>,
    done: bool,
) -> Result<()> {
    let run = retirement.run_id.clone();
    let current = db
        .run(&run)?
        .is_some_and(|run| run["environmentRetirementId"] == retirement.id);
    if done {
        db.remove(RETIREMENTS, &retirement.id)?;
    } else {
        retirement.retry_after = error.as_ref().map(|_| now() + RETRY_MS);
        retirement.error.clone_from(&error);
        db.put(RETIREMENTS, &serde_json::to_value(retirement)?)?;
    }
    // The expired conversation shows why its previous environment is still retained.
    if let Some(mut chat) = db.conversation_for_run(&run)?
        && current
        && chat.environment_error != error
        && (error.is_none()
            || chat.environment_state == Some(ConversationEnvironmentState::Expired))
    {
        chat.environment_error = error;
        db.save_conversation(&chat)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_fixture::DbRunFixture;

    #[tokio::test]
    async fn an_older_retirement_cannot_replace_or_clear_the_new_environments_error() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            let (run,chat,newest) = (id(),id(),id());
            db.add_run_fixture(&json!({ "id":run, "taskId":chat, "status":"succeeded", "createdAt":0, "environmentState":"expired", "environmentRetirementId":newest }),None)?;
            db.save_conversation_boundary(&json!({ "id":chat,"runId":run,"environmentState":"expired","environmentExpiredAt":42,"environmentError":"new cleanup failure" }))?;
            let old = Retirement { id:id(),run_id:run.clone(),expired_at:42,..Retirement::default() };
            save_progress(db,old.clone(),Some("old cleanup failure".into()),false)?;
            assert_eq!(db.conversation_boundary(&chat)?.unwrap()["environmentError"],"new cleanup failure");
            save_progress(db,old,None,true)?;
            assert_eq!(db.conversation_boundary(&chat)?.unwrap()["environmentError"],"new cleanup failure");
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn startup_finishes_legacy_cleaning_once_and_preserves_its_expiration_time() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        let run = id();
        store.transaction(move |db| {
            db.add_run_fixture(&json!({ "id": run, "taskId": "chat", "trigger": "chat", "status": "succeeded", "createdAt": 0 }), None)?;
            db.save_conversation_boundary(&json!({ "id": "chat", "runId": run, "environmentState": "cleaning", "environmentExpiredAt": 1234, "environmentError": "offline" }))?;
            finish_legacy_cleaning(db)?;
            finish_legacy_cleaning(db)?;
            let chat = db.conversation_boundary("chat")?.unwrap();
            assert_eq!(chat["environmentState"], "expired");
            assert_eq!(chat["environmentExpiredAt"], 1234);
            assert!(chat["environmentError"].is_null());
            assert_eq!(db.run(&run)?.unwrap()["environmentExpirationCause"], "inactivity");
            assert_eq!(db.list(RETIREMENTS)?.len(), 1);
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn late_engine_session_events_keep_history_without_reattaching_the_expired_environment() {
        use crate::worker::checkpoint::{Checkpoint, EventChanges};
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        let run = id();
        let saved = RunCheckpoint {
            completed: Some(false),
            ..Default::default()
        };
        let (owner, checkpoint) = (run.clone(), saved.clone());
        store.transaction(move |db| {
            db.add_run_fixture(&json!({ "id": owner, "taskId": "chat", "trigger": "chat", "status": "running", "createdAt": 0, "sessionId": "old-session" }), None)?;
            db.save_conversation_boundary(&json!({ "id": "chat", "runId": owner }))?;
            checkpoint.store_in(db, &owner)?;
            request_in(db, &owner, &Expiration::new(Cause::NodeChange), true)
        }).await.unwrap();
        let checkpoint = Checkpoint::new(store.clone(), run.clone(), saved);
        checkpoint
            .record_event(
                EventKind::Status,
                "Late engine metadata".into(),
                json!({}),
                EventChanges {
                    run: Some(crate::runs::RunChanges {
                        session_id: Some(Some("late-session".into())),
                        resume_available: Some(true),
                        ..Default::default()
                    }),
                    completed: false,
                    failure: None,
                    progress_complete: true,
                    last_message: None,
                },
            )
            .await
            .unwrap();
        let current = store.run(&run).await.unwrap();
        assert!(current["sessionId"].is_null());
        assert_eq!(current["resumeAvailable"], false);
        store
            .read(move |db| {
                let recorded: i64 = db.0.query_row(
                    "SELECT COUNT(*) FROM events WHERE run_id=? AND text='Late engine metadata'",
                    [run],
                    |row| row.get(0),
                )?;
                assert_eq!(recorded, 1);
                Ok(())
            })
            .await
            .unwrap();
    }
}
