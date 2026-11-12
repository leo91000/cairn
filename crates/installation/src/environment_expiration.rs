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
}

/// What the retired environment still occupies, captured when it was detached.
#[derive(Default, Serialize, Deserialize)]
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
    process: Option<ProcessIdentity>,
    isolated: bool,
    account_id: Option<String>,
    workspaces: Vec<Value>,
    projects: Vec<Value>,
    /// The previous execution is confirmed stopped, or its node lease lapsed.
    fenced: bool,
    /// Manager files, recovery references and the provider run home are removed.
    local: bool,
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
             WHERE (runs.placement_node_id=?1 OR json_extract(run_checkpoints.state,'$.nodeId')=?1)
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
) -> Result<u64> {
    let mut expired = 0;
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
        expired += 1;
    }
    Ok(expired)
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
        }),
    )?;
    db.revoke_mcp_run(run_id)?;
    let executing = matches!(
        RunStatus::of(&run),
        Some(RunStatus::Queued | RunStatus::Running)
    );
    // An active mission resumes on its own; a scheduled wait keeps its continuation.
    let mut expiration = expiration.clone();
    expiration.continue_turn |= run["trigger"] != "chat" && executing && !run["wakeup"].is_object();
    if !live {
        return detach_in(db, run_id, &expiration, expired_at);
    }
    if executing && run["cancelRequestedAt"].is_null() {
        db.change_run(
            run_id,
            crate::runs::RunChanges {
                cancel_requested_at: Some(Some(now())),
                ..Default::default()
            },
        )?;
    }
    let pending = Pending {
        id: run_id.to_owned(),
        run_id: run_id.to_owned(),
        expired_at,
        expiration: expiration.clone(),
        environment: run,
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
            boundary: Some(json!({ "workspaces": [], "backup": null, "storage": null })),
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
    let retirement = Retirement {
        id: id(),
        run_id: run_id.to_owned(),
        cause: Some(expiration.cause),
        expired_at,
        nodes,
        attempt: checkpoint.runner_id().map(str::to_owned),
        attempt_node: checkpoint.node_id.clone().flatten(),
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
    let resume = expiration.continue_turn && (mission || run["chatExecution"].is_object());
    let node_change = expiration.cause == Cause::NodeChange;
    let keep_pins = expiration.cause == Cause::NodeRevocation;

    let mut boundary = json!({
        "environmentState": if resume { Value::Null } else { "expired".into() },
        "environmentExpiredAt": expired_at,
        "environmentExpirationCause": expiration.cause,
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
    if resume {
        fresh.completed = Some(false);
        fresh.launched = Some(false);
        if mission {
            fresh.remaining_ms = checkpoint.remaining_ms;
            boundary["sessionHandoff"] = crate::chats::handoff_context(db, run_id)?.into();
        } else {
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

    /// Removes what retired environments still occupy, oldest first. A failed step
    /// retries later without delaying other retirements.
    pub async fn retire_environments(&self) -> Result<()> {
        let mut due = self
            .store
            .list(RETIREMENTS)
            .await?
            .into_iter()
            .map(serde_json::from_value::<Retirement>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        due.retain(|retirement| retirement.retry_after.unwrap_or(0) <= now());
        due.sort_by_key(|retirement| retirement.expired_at);
        let mut progressed = false;
        for mut retirement in due {
            if self.shutdown.is_cancelled() {
                break;
            }
            let result = self.retire(&mut retirement).await;
            progressed |= retirement.fenced && retirement.local;
            let error = result.err().map(|error| error.message);
            let done = error.is_none() && retirement.nodes.is_empty();
            self.store
                .transaction(move |db| save_progress(db, retirement, error, done))
                .await?;
        }
        if progressed {
            self.worker.notify();
        }
        Ok(())
    }

    async fn retire(&self, retirement: &mut Retirement) -> Result<()> {
        if !retirement.fenced {
            crate::recovery::fence_retired(
                self,
                retirement.attempt.as_deref(),
                retirement.attempt_node.as_deref(),
                retirement.process.as_ref(),
                retirement.isolated,
            )
            .await?;
            retirement.fenced = true;
        }
        if !retirement.local {
            let run = retirement.run_id.clone();
            let preparation = self.worker.run_preparation_lock(&run).await;
            let _preparation = preparation.lock_owned().await;
            self.accounts
                .recover_run(
                    self,
                    &json!({ "id": run, "accountId": retirement.account_id }),
                )
                .await?;
            crate::nodes::publication::expire_environment(self, &run).await?;
            crate::conversation_deletion::detach_retired_worktrees(
                self,
                &run,
                &retirement.workspaces,
                &retirement.projects,
            )
            .await?;
            crate::conversation_deletion::remove_run_files(self, &run).await?;
            retirement.local = true;
        }
        let mut failure = None;
        let mut remaining = Vec::new();
        for node in std::mem::take(&mut retirement.nodes) {
            if let Err(error) = crate::conversation_deletion::delete_retired_disk(
                self,
                &retirement.run_id,
                &node,
                retirement.isolated,
            )
            .await
            {
                failure.get_or_insert(error);
                remaining.push(node);
            }
        }
        retirement.nodes = remaining;
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
    if done || db.run(&run)?.is_none() {
        db.remove(RETIREMENTS, &retirement.id)?;
    } else {
        retirement.retry_after = error.as_ref().map(|_| now() + RETRY_MS);
        retirement.error.clone_from(&error);
        db.put(RETIREMENTS, &serde_json::to_value(retirement)?)?;
    }
    // The expired conversation shows why its previous environment is still retained.
    if let Some(mut chat) = db.conversation_for_run(&run)?
        && chat.environment_error != error
        && (error.is_none()
            || chat.environment_state == Some(ConversationEnvironmentState::Expired))
    {
        chat.environment_error = error;
        db.save_conversation(&chat)?;
    }
    Ok(())
}
