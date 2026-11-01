use super::{Worker, checkpoint::RunCheckpoint};
use crate::{
    config::now,
    error::{Error, Result, required},
    process::{Environment, Output, bounded_output, command},
    run_status::RunStatus,
    service::{Service, run_projects},
    store::Db,
    validation::text,
};
use cairn_contracts::{EventKind, ResumeExecutionSelection};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

impl Worker {
    pub async fn cancel(&self, s: &Service, id: &str) -> Result<()> {
        self.cancel_with_grant(s, id, None).await
    }

    pub(crate) async fn cancel_from_run(&self, s: &Service, id: &str, bearer: &str) -> Result<()> {
        self.cancel_with_grant(s, id, Some(bearer.to_owned())).await
    }

    async fn cancel_with_grant(&self, s: &Service, id: &str, bearer: Option<String>) -> Result<()> {
        let active = self.active.lock().await;
        let is_active = active.contains_key(id);
        let run_id = id.to_owned();
        s.store
            .transaction(move |db| {
                if let Some(bearer) = bearer {
                    crate::installation_mcp::authorize_in(db, &bearer)?;
                }
                request_cancel(db, &run_id, is_active)
            })
            .await?;
        if let Some(cancel) = active.get(id) {
            cancel.cancel();
        }
        self.notify();
        Ok(())
    }

    /// Pauses queued follow-ups and stops the chat's current turn together.
    pub(crate) async fn stop_chat_authorized<F>(
        &self,
        s: &Service,
        id: &str,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let active = self.active.lock().await;
        let executing: std::collections::HashSet<_> = active.keys().cloned().collect();
        let id = id.to_owned();
        let chat = s
            .store
            .transaction(move |db| {
                authorize(db)?;
                let chat = crate::chats::pause_in(db, &id, true)?;
                if let Some(run_id) = chat["runId"].as_str() {
                    crate::chats::coding_agent::cancel_in(db, run_id)?;
                    crate::chats::continuation::cancel_for_run(db, run_id)?;
                    let run = required(db.run_state(run_id)?, "Run not found")?;
                    if run.status.is_active() {
                        request_cancel(db, run_id, executing.contains(run_id))?;
                    }
                }
                required(db.conversation_boundary(&id)?, "Chat not found")
            })
            .await?;
        if let Some(cancel) = chat["runId"].as_str().and_then(|id| active.get(id)) {
            cancel.cancel();
        }
        self.notify();
        Ok(chat)
    }

    pub async fn resume(&self, s: &Service, id: &str) -> Result<Value> {
        self.resume_authorized(s, id, |_| Ok(())).await
    }

    /// Rechecks delegated authority in the same transaction as the resume effect.
    pub(crate) async fn resume_authorized<F>(
        &self,
        s: &Service,
        id: &str,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        self.resume_selected_authorized(s, id, None, None, None, authorize)
            .await
    }

    /// Resume a stopped run after checking the exact turn and the selected coding agent.
    pub(crate) async fn resume_selected_authorized<F>(
        &self,
        s: &Service,
        id: &str,
        expected_turn_id: Option<String>,
        selection: Option<ResumeExecutionSelection>,
        selected_provider: Option<crate::provider::Provider>,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        if selection.is_some()
            && expected_turn_id
                .as_deref()
                .is_none_or(|turn| turn.is_empty() || turn.chars().count() > 200)
        {
            return Err(Error::bad(
                "A valid turn identifier is required when selecting a model for resume.",
            ));
        }
        let preparation = self.run_preparation_lock(id).await;
        let _preparation = preparation.lock_owned().await;
        let active = self.active.lock().await;
        if active.contains_key(id) {
            return Err(Error::conflict(
                "Wait for this run to finish stopping before resuming.",
            ));
        }
        let id = id.to_owned();
        let result = s
            .store
            .transaction(move |db| {
                authorize(db)?;
                requeue(
                    db,
                    &id,
                    expected_turn_id.as_deref(),
                    selection.as_ref(),
                    selected_provider,
                )
            })
            .await?;
        self.notify();
        Ok(result)
    }

    pub async fn cleanup(&self, s: &Service, id: &str) -> Result<Value> {
        self.cleanup_authorized(s, id, |_| Ok(())).await
    }

    pub(crate) async fn cleanup_authorized<F>(
        &self,
        s: &Service,
        id: &str,
        authorize: F,
    ) -> Result<Value>
    where
        F: Fn(&Db<'_>) -> Result<()> + Clone + Send + Sync + 'static,
    {
        let active = self.active.lock().await;
        let proof = authorize.clone();
        let run_id = id.to_owned();
        let run = s
            .store
            .read(move |db| {
                proof(db)?;
                required(db.run_record(&run_id)?, "Run not found")
            })
            .await?;
        if run.state.status.is_active() || active.contains_key(id) {
            return Err(Error::conflict(
                "Wait for this run to finish before cleaning up.",
            ));
        }
        let run = run.wire()?;
        if run["isolated"] == true {
            return Err(Error::conflict(
                "This workspace is retained on a private VM disk. Resume the run to review and preserve its work.",
            ));
        }
        let managed = managed_workspaces(&run);
        if run["snapshot"]["task"]["worktree"] != true
            || !run["workspace"].is_string()
            || managed.is_empty()
        {
            return Err(Error::conflict(
                "This run has no managed worktree to clean up.",
            ));
        }
        let generation = RunCheckpoint::load(&s.store, id)
            .await?
            .and_then(|checkpoint| checkpoint.generation);
        let root = s
            .config
            .data_dir
            .join("runs")
            .join(id)
            .join(generation.map_or_else(|| "workspace".into(), |g| format!("workspace-{g}")));
        let env = std::env::vars().collect();
        for workspace in &managed {
            ensure_clean(workspace, &root, &env).await?;
        }
        let proof = authorize.clone();
        s.store.read(move |db| proof(db)).await?;
        for workspace in &managed {
            remove_workspace(&run, workspace, &env).await?;
        }
        let patch = crate::runs::RunChanges {
            workspace: Some(None),
            resume_available: Some(false),
            workspace_cleaned_at: Some(Some(now())),
            ..Default::default()
        };
        let id = id.to_owned();
        s.store
            .transaction(move |db| {
                authorize(db)?;
                db.change_run(&id, patch)?;
                db.audit("run.workspace.cleaned", &json!({ "id": id }))
            })
            .await?;
        Ok(json!({ "cleaned": true }))
    }
}

fn request_cancel(db: &Db<'_>, id: &str, is_active: bool) -> Result<()> {
    crate::chats::coding_agent::cancel_in(db, id)?;
    let run = required(db.run_state(id)?, "Run not found")?;
    let waiting = crate::chats::continuation::cancel_for_run(db, id)?;
    if !run.status.is_active() {
        if waiting {
            return db.audit("run.wakeup.cancelled", &json!({ "id": id }));
        }
        return Err(Error::conflict("This run has already finished."));
    }
    db.change_run(
        id,
        crate::runs::RunChanges {
            cancel_requested_at: Some(Some(now())),
            ..Default::default()
        },
    )?;
    // Waiting means the agent process already exited. Release may still hold
    // its in-memory slot; cancellation must settle this wait even in that gap.
    // Executing or recovering runs are settled by their worker once stopped.
    if (!is_active || run.status == RunStatus::Waiting) && !run.recovery_pending {
        crate::nodes::moves::release_terminal_destination_in(db, id)?;
        let patch = crate::runs::RunChanges {
            status: Some(RunStatus::Cancelled),
            finished_at: Some(Some(now())),
            summary: Some(("Cancelled before execution.").to_owned()),
            retry: Some(None),
            ..Default::default()
        };
        db.change_run(id, patch)?;
        // A run waiting for an automatic retry shows the stop in its conversation; a wait's
        // own card already shows its cancellation.
        if run
            .retry
            .as_ref()
            .is_some_and(|r| r.next_attempt_at.is_some())
        {
            db.event(id, &EventKind::Status, RunStatus::Cancelled.as_str(), None)?;
        }
    }
    db.audit("run.cancelled", &json!({ "id": id }))
}

/// Queues a stopped run to continue its saved conversation.
fn requeue(
    db: &Db<'_>,
    id: &str,
    expected_turn_id: Option<&str>,
    selection: Option<&ResumeExecutionSelection>,
    selected_provider: Option<crate::provider::Provider>,
) -> Result<Value> {
    let record = required(db.run_record(id)?, "Run not found")?;
    let state = record.state.clone();
    let run = record.wire()?;
    let turn_mismatch = if selection.is_some() {
        expected_turn_id.is_none_or(|expected| {
            text(&run, "trigger") != "chat"
                || run["chatExecution"]["messageId"].as_str() != Some(expected)
        })
    } else {
        expected_turn_id.is_some_and(|expected| {
            run["chatExecution"]["messageId"].as_str() != Some(expected)
                && run["missionTurnId"].as_str() != Some(expected)
        })
    };
    if turn_mismatch {
        return Err(Error::conflict(
            "This turn changed before it could be resumed. Refresh the conversation.",
        ));
    }
    if run["backendMigration"].is_object() {
        return Err(Error::conflict(
            "Wait for the backend migration to finish before resuming.",
        ));
    }
    crate::conversation_lifecycle::require_active_run(db, id)?;
    let replay_refused = db.conversations_boundary()?.iter().any(|chat| {
        chat["runId"] == id
            && (chat["cancelledByDeletion"] == true
                || chat["sessionRestartRequested"] == true
                || chat["environmentState"].is_string())
    });
    if replay_refused {
        return Err(Error::conflict(
            "Send a new message and resume the conversation queue to continue; cancelled work will not replay.",
        ));
    }
    let checkpoint = RunCheckpoint::load_in(db, id)?;
    let status = state.status;
    // A chat that failed before its process launched simply starts again.
    let before_launch = run["chatExecution"].is_object()
        && matches!(status, RunStatus::Failed | RunStatus::Cancelled)
        && checkpoint.as_ref().is_none_or(|c| !c.launched());
    let resumable = status.is_resumable()
        && state.resume_available
        && checkpoint.as_ref().is_some_and(|c| c.prepared().is_some())
        && state.workspace_cleaned_at.is_none();
    if !before_launch && !resumable {
        return Err(Error::conflict(
            "This run has no saved conversation available to resume.",
        ));
    }
    let pending_switch =
        state.trigger == "chat" && crate::chats::coding_agent::pending_for_run(db, id)?;
    if pending_switch && selection.is_none() {
        return Err(Error::conflict(
            "Wait for the provider switch to finish before resuming without a model selection.",
        ));
    }
    let selected_snapshot = if let Some(selection) = selection {
        if !resumable {
            return Err(Error::conflict(
                "A model can only be selected for a saved conversation resume.",
            ));
        }
        let provider = crate::provider::Provider::of_run(&run)?;
        if selected_provider != Some(provider) {
            return Err(Error::conflict(
                "The saved coding agent changed before it could be resumed. Refresh the conversation.",
            ));
        }
        crate::resume_selection::validate_preserved_model(&run, selection)?;
        let mut snapshot: cairn_contracts::RunSnapshot =
            serde_json::from_value(run["snapshot"].clone())?;
        if !selection.preserve_model {
            snapshot.agent.settings.model.clone_from(&selection.model);
        }
        snapshot
            .agent
            .settings
            .reasoning
            .clone_from(&selection.reasoning);
        snapshot.agent.settings.speed.clone_from(&selection.speed);
        Some(snapshot)
    } else {
        None
    };
    if db
        .active_run_states()?
        .iter()
        .any(|a| a.task_id == state.task_id)
    {
        return Err(Error::conflict("This task already has an active run."));
    }
    // A parked mission completed its provider turn. Explicitly continuing it
    // creates a new turn, while recovery of interrupted turns keeps its identity.
    let completed_mission =
        state.trigger != "chat" && checkpoint.as_ref().is_some_and(RunCheckpoint::completed);
    // A manual resume may arrive before chat_tick observes the terminal turn.
    // Its scheduled continuation must not survive into the resumed execution.
    crate::chats::continuation::cancel_for_run(db, id)?;
    if pending_switch && selection.is_some() {
        crate::chats::coding_agent::cancel_in(db, id)?;
    }
    if let Some(mut checkpoint) = checkpoint {
        checkpoint.remaining_ms = Some(crate::run_limits::budget_ms(&run["snapshot"]["agent"]));
        checkpoint.controller_recoveries = None;
        checkpoint.controller_recovery_reservation = None;
        checkpoint.provider_failure_attempt = None;
        checkpoint.finalization_failure_attempt = None;
        checkpoint.move_recovery_reservation = None;
        checkpoint.provider_retry_reservation = None;
        checkpoint.pending_paid_admission = None;
        checkpoint.controller_observation = None;
        checkpoint.turn_pending = Some(true);
        checkpoint.recovery_deadline_ms = None;
        checkpoint.output_interrupted = None;
        checkpoint.provider_failed = None;
        if !before_launch {
            checkpoint.completed = Some(false);
            checkpoint.terminal_code = Some(None);
        }
        checkpoint.settled = None;
        // A confirmed pre-turn failure may have no session yet. Its safety
        // evidence survives a manual budget reset until the next supervised launch.
        let pending_startup = state.session_id.is_none()
            && checkpoint
                .retry_cause
                .as_ref()
                .and_then(Option::as_ref)
                .is_some_and(|cause| cause.startup);
        if !pending_startup {
            checkpoint.retry_cause = Some(None);
        }
        checkpoint.store_in(db, id)?;
    }
    let wait_reason = (!before_launch).then_some("Resuming saved conversation.");
    let mut patch = crate::runs::RunChanges {
        snapshot: selected_snapshot,
        status: Some(RunStatus::Queued),
        error: Some(None),
        recovery_pending: Some(true),
        cancel_requested_at: Some(None),
        finished_at: Some(None),
        account_wait_reason: Some(wait_reason.map(str::to_owned)),
        retry: Some(None),
        boundary: Some(serde_json::json!({"outcome": null})),
        ..Default::default()
    };
    if completed_mission {
        patch.boundary.as_mut().unwrap()["missionTurnId"] = crate::config::id().into();
        patch.boundary.as_mut().unwrap()["waitContinuation"] = Value::Null;
    }
    db.change_run(id, patch)?;
    let result = required(db.run(id)?, "Run not found")?;
    db.event(id, &EventKind::Status, "Resume requested", None)?;
    Ok(result)
}

/// Worktrees and clones the run created; direct workspaces belong to the user.
fn managed_workspaces(run: &Value) -> Vec<Value> {
    let workspaces = run["workspaces"].as_array().cloned().unwrap_or_else(|| {
        if !run["workspace"].is_string() || !run["snapshot"]["project"].is_object() {
            return Vec::new();
        }
        vec![json!({
            "projectId": run["snapshot"]["project"]["id"],
            "path": run["workspace"],
            "kind": "worktree",
        })]
    });
    workspaces
        .into_iter()
        .filter(|workspace| workspace["kind"] != "direct")
        .collect()
}

async fn ensure_clean(workspace: &Value, root: &PathBuf, env: &Environment) -> Result<()> {
    let target = Path::new(text(workspace, "path"));
    if !target.starts_with(root)
        || crate::skills::workspace(target, std::slice::from_ref(root)).await? != target
    {
        return Err(Error::conflict(
            "Workspace is outside this run’s managed directory.",
        ));
    }
    let target = target.to_string_lossy().into_owned();
    let output = git(&["-C", &target, "status", "--porcelain", "--ignored"], env).await?;
    if !output.success || !output.stdout.trim().is_empty() {
        return Err(Error::conflict(
            "This worktree contains changes or untracked files. Commit or move them before cleanup.",
        ));
    }
    Ok(())
}

async fn remove_workspace(run: &Value, workspace: &Value, env: &Environment) -> Result<()> {
    let path = text(workspace, "path");
    if workspace["kind"] == "clone" {
        tokio::fs::remove_dir_all(path).await?;
        return Ok(());
    }
    let project = run_projects(run)
        .into_iter()
        .find(|p| p["id"] == workspace["projectId"])
        .ok_or_else(|| Error::bad("Project not found"))?;
    let output = git(
        &["-C", text(&project, "path"), "worktree", "remove", path],
        env,
    )
    .await?;
    if !output.success {
        return Err(Error::conflict("Git could not remove this worktree."));
    }
    Ok(())
}

async fn git(args: &[&str], env: &Environment) -> Result<Output> {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    bounded_output(
        command("git", &args, env, None),
        Duration::from_secs(10),
        100_000,
    )
    .await
}

pub async fn routes(s: &Arc<Service>, input: &crate::http::Input) -> Option<Result<Value>> {
    if input.method != "POST" {
        return None;
    }
    let segments = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    let result = match segments.as_slice() {
        ["runs", id, action @ ("cancel" | "cleanup" | "retry")] => {
            async {
                let name = match *action {
                    "cancel" => "cancel_run",
                    "cleanup" => "cleanup_run",
                    _ => "retry_run",
                };
                crate::capabilities::run_operation(name)
                    .expect("run operation")
                    .invoke(
                        s,
                        json!({ "runId": id }),
                        crate::capabilities::ResumeContext::http(input)?,
                    )
                    .await
            }
            .await
        }
        ["runs", id, "resume"] => {
            async {
                let mut body = if input.body.is_null() {
                    json!({})
                } else {
                    input.body.clone()
                };
                let Some(fields) = body.as_object_mut() else {
                    return Err(Error::bad("Resume input must be an object."));
                };
                if fields
                    .get("runId")
                    .is_some_and(|requested| requested.as_str() != Some(id))
                {
                    return Err(Error::conflict(
                        "Resume runId must match the run in the request path.",
                    ));
                }
                fields.insert("runId".into(), (*id).into());
                let command = crate::capabilities::RESUME_RUN.decode(body)?;
                let context = crate::capabilities::ResumeContext::http(input)?;
                let output = crate::capabilities::RESUME_RUN
                    .invoke(s, command, context)
                    .await?;
                Ok(serde_json::to_value(output)?)
            }
            .await
        }
        _ => return None,
    };
    Some(result)
}

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests {
    use super::*;

    async fn chat_resume_store() -> (tempfile::TempDir, crate::store::Store) {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({
                        "id": "chat-run",
                        "taskId": "chat-task",
                        "status": "failed",
                        "trigger": "chat",
                        "createdAt": 0,
                        "resumeAvailable": true,
                        "sessionId": "saved-session",
                        "chatExecution": { "messageId": "chat-turn" },
                        "snapshot": {
                            "agent": {
                                "id": crate::config::MAIN_AGENT_ID,
                                "provider": "codex",
                                "model": "",
                                "reasoning": "low",
                                "speed": "",
                            },
                        },
                    }),
                    None,
                )?;
                db.save_checkpoint_boundary(
                    "chat-run",
                    &json!({
                        "launched": true,
                        "prepared": { "cwd": "/workspace" },
                        "generation": "saved-generation",
                    }),
                )?;
                let chat = serde_json::from_value::<crate::chats::Conversation>(json!({
                    "id": "chat",
                    "agentId": crate::config::MAIN_AGENT_ID,
                    "runId": "chat-run",
                    "createdAt": 0,
                    "updatedAt": 0,
                }))
                .unwrap();
                db.save_conversation(&chat)?;
                db.save_engine_switch(&crate::chats::engine_switch::EngineSwitch {
                    chat_id: "chat".into(),
                    request_id: "switch-request".into(),
                    run_id: "chat-run".into(),
                    turn_id: "chat-turn".into(),
                    provider: "claude".into(),
                    model: "claude-sonnet".into(),
                    pending: true,
                    error: None,
                    retry_at: None,
                    receipt_expires_at: crate::config::now() + 86_400_000,
                    current_expires_at: None,
                })?;
                Ok(())
            })
            .await
            .unwrap();

        (root, store)
    }

    #[tokio::test]
    async fn preserve_model_rechecks_current_snapshot_before_requeue_mutations() {
        let (_root, store) = chat_resume_store().await;
        let selection = ResumeExecutionSelection {
            model: String::new(),
            preserve_model: true,
            reasoning: "high".into(),
            speed: "standard".into(),
        };
        let preflight_run = store.read(|db| db.run("chat-run")).await.unwrap().unwrap();
        crate::resume_selection::validate_preserved_model(&preflight_run, &selection).unwrap();

        store
            .transaction(|db| {
                let mut snapshot = db.run("chat-run")?.unwrap()["snapshot"].clone();
                snapshot["agent"]["model"] = "gpt-6-luna".into();
                db.change_run(
                    "chat-run",
                    crate::runs::RunChanges {
                        snapshot: Some(serde_json::from_value(snapshot)?),
                        ..Default::default()
                    },
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let before_run = store.read(|db| db.run("chat-run")).await.unwrap().unwrap();
        let before_checkpoint = store
            .read(|db| db.checkpoint_boundary("chat-run"))
            .await
            .unwrap()
            .unwrap();
        let before_switch = store
            .read(|db| db.engine_switch("chat"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before_run["snapshot"]["agent"]["model"], "gpt-6-luna");
        assert!(before_switch.pending);

        let rejected = store
            .transaction(move |db| {
                requeue(
                    db,
                    "chat-run",
                    Some("chat-turn"),
                    Some(&selection),
                    Some(crate::provider::Provider::Codex),
                )
            })
            .await
            .unwrap_err();
        assert_eq!(rejected.status, 400);
        assert!(rejected.message.contains("Preserving the saved model"));

        let after_run = store.read(|db| db.run("chat-run")).await.unwrap().unwrap();
        let after_checkpoint = store
            .read(|db| db.checkpoint_boundary("chat-run"))
            .await
            .unwrap()
            .unwrap();
        let after_switch = store
            .read(|db| db.engine_switch("chat"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after_run, before_run);
        assert_eq!(after_checkpoint, before_checkpoint);
        assert_eq!(
            serde_json::to_value(after_switch).unwrap(),
            serde_json::to_value(before_switch).unwrap()
        );
    }

    #[tokio::test]
    async fn preserve_model_blank_and_explicit_model_requeue_controls() {
        let (_blank_root, blank_store) = chat_resume_store().await;
        let preserve = ResumeExecutionSelection {
            model: String::new(),
            preserve_model: true,
            reasoning: "high".into(),
            speed: "standard".into(),
        };
        let blank_before = blank_store
            .read(|db| db.run("chat-run"))
            .await
            .unwrap()
            .unwrap();
        crate::resume_selection::validate_preserved_model(&blank_before, &preserve).unwrap();
        let blank_after = blank_store
            .transaction(|db| {
                requeue(
                    db,
                    "chat-run",
                    Some("chat-turn"),
                    Some(&preserve),
                    Some(crate::provider::Provider::Codex),
                )
            })
            .await
            .unwrap();
        assert_eq!(blank_after["status"], "queued");
        assert_eq!(blank_after["snapshot"]["agent"]["model"], "");
        assert_eq!(blank_after["snapshot"]["agent"]["reasoning"], "high");
        assert_eq!(blank_after["snapshot"]["agent"]["speed"], "standard");
        let cancelled_switch = blank_store
            .read(|db| db.engine_switch("chat"))
            .await
            .unwrap()
            .unwrap();
        assert!(!cancelled_switch.pending);

        let (_explicit_root, explicit_store) = chat_resume_store().await;
        let explicit = ResumeExecutionSelection {
            model: "gpt-6-luna".into(),
            preserve_model: false,
            reasoning: "xhigh".into(),
            speed: "standard".into(),
        };
        let explicit_after = explicit_store
            .transaction(|db| {
                requeue(
                    db,
                    "chat-run",
                    Some("chat-turn"),
                    Some(&explicit),
                    Some(crate::provider::Provider::Codex),
                )
            })
            .await
            .unwrap();
        assert_eq!(explicit_after["status"], "queued");
        assert_eq!(explicit_after["snapshot"]["agent"]["model"], "gpt-6-luna");
        assert_eq!(explicit_after["snapshot"]["agent"]["reasoning"], "xhigh");
        assert_eq!(explicit_after["snapshot"]["agent"]["speed"], "standard");
    }

    #[tokio::test]
    async fn manual_mission_resume_changes_completed_turn_identity_but_keeps_interrupted_turns() {
        for completed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = crate::store::Store::open(root.path()).unwrap();
            store
                .transaction(move |db| {
                    db.add_run_fixture(&json!({
                    "id": "mission", "taskId": "task", "status": "cancelled", "createdAt": 0,
                    "resumeAvailable": true, "missionTurnId": "previous-turn",
                    "waitContinuation": { "observation": { "state": "completed" } },
                }), None)?;
                    db.save_checkpoint_boundary("mission", &json!({
                    "completed": completed, "outputInterrupted": true, "launched": true, "prepared": { "cwd": "/workspace" },
                }))?;
                    let resumed = requeue(db, "mission", None, None, None)?;
                    assert_eq!(resumed["status"], "queued");
                    let checkpoint = db.checkpoint_boundary("mission")?.unwrap();
                    assert!(checkpoint["outputInterrupted"].is_null());
                    if completed {
                        assert_ne!(resumed["missionTurnId"], "previous-turn");
                        assert!(resumed["waitContinuation"].is_null());
                    } else {
                        assert_eq!(resumed["missionTurnId"], "previous-turn");
                        assert!(resumed["waitContinuation"].is_object());
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn cancellation_settles_a_wait_even_while_release_still_holds_the_worker_slot() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({
                        "id": "mission", "taskId": "task", "status": "waiting", "createdAt": 0,
                        "wakeup": { "id": "wake" },
                    }),
                    None,
                )?;
                request_cancel(db, "mission", true)?;
                let run = db.run("mission")?.unwrap();
                assert_eq!(run["status"], "cancelled");
                assert!(run["finishedAt"].is_number());
                assert!(run["wakeup"].is_null());
                Ok(())
            })
            .await
            .unwrap();
    }
}
