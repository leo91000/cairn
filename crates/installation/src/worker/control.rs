use super::{ResumeSelectionContext, Worker, checkpoint::RunCheckpoint};
use crate::{
    config::now,
    error::{Error, Result, required},
    process::{Environment, Output, bounded_output, command},
    run_status::RunStatus,
    service::{Service, run_projects},
    store::Db,
    validation::text,
};
use cairn_contracts::{ChatExecution, EventKind, MessageStatus, ResumeExecutionSelection};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

impl Worker {
    pub async fn cancel(&self, s: &Service, id: &str) -> Result<()> {
        self.cancel_live(s, id, crate::authorization::Authorization::default())
            .await
    }

    pub(crate) async fn cancel_live(
        &self,
        s: &Service,
        id: &str,
        authorize: crate::authorization::Authorization,
    ) -> Result<()> {
        authorize.recheck().await?;
        let active = self.active.lock().await;
        authorize.recheck().await?;
        let is_active = active.contains_key(id);
        let run_id = id.to_owned();
        s.store
            .transaction(move |db| {
                authorize.authorize(db)?;
                request_cancel(db, &run_id, is_active)
            })
            .await?;
        s.accepted_jobs.revoke_run(id);
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
        self.stop_chat_live(s, id, crate::authorization::Authorization::once(authorize))
            .await
    }

    pub(crate) async fn stop_chat_live(
        &self,
        s: &Service,
        id: &str,
        authorize: crate::authorization::Authorization,
    ) -> Result<Value> {
        authorize.recheck().await?;
        let active = self.active.lock().await;
        authorize.recheck().await?;
        let executing: std::collections::HashSet<_> = active.keys().cloned().collect();
        let id = id.to_owned();
        let chat = s
            .store
            .transaction(move |db| {
                authorize.authorize(db)?;
                stop_chat_in(db, &id, &executing)
            })
            .await?;
        s.accepted_jobs.revoke_chat(text(&chat, "id"));
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
        self.resume_selected_authorized(s, id, ResumeSelectionContext::default(), authorize)
            .await
    }

    pub(crate) async fn resume_live(
        &self,
        s: &Service,
        id: &str,
        authorize: crate::authorization::Authorization,
    ) -> Result<Value> {
        self.resume_selected_live(s, id, ResumeSelectionContext::default(), authorize)
            .await
    }

    pub(crate) async fn resume_selected_authorized<F>(
        &self,
        s: &Service,
        id: &str,
        resume: ResumeSelectionContext,
        authorize: F,
    ) -> Result<Value>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        self.resume_selected_live(
            s,
            id,
            resume,
            crate::authorization::Authorization::once(authorize),
        )
        .await
    }

    /// Resume a stopped run after checking authority, the exact turn, and selected agent.
    pub(crate) async fn resume_selected_live(
        &self,
        s: &Service,
        id: &str,
        resume: ResumeSelectionContext,
        authorize: crate::authorization::Authorization,
    ) -> Result<Value> {
        if resume.selection.is_some()
            && resume
                .expected_turn_id
                .as_deref()
                .is_none_or(|turn| turn.is_empty() || turn.chars().count() > 200)
        {
            return Err(Error::bad(
                "A valid turn identifier is required when selecting a model for resume.",
            ));
        }
        let preparation = self.run_preparation_lock(id).await;
        let _preparation = preparation.lock_owned().await;
        authorize.recheck().await?;
        let active = self.active.lock().await;
        authorize.recheck().await?;
        if active.contains_key(id) {
            return Err(Error::conflict(
                "Wait for this run to finish stopping before resuming.",
            ));
        }
        let id = id.to_owned();
        let result = s
            .store
            .transaction(move |db| {
                authorize.authorize(db)?;
                requeue(
                    db,
                    &id,
                    resume.expected_turn_id.as_deref(),
                    resume.selection.as_ref(),
                    resume.selected_provider,
                    resume.saved_auto_catalog.as_ref(),
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
        self.cleanup_live(s, id, crate::authorization::Authorization::local(authorize))
            .await
    }

    pub(crate) async fn cleanup_live(
        &self,
        s: &Service,
        id: &str,
        authorize: crate::authorization::Authorization,
    ) -> Result<Value> {
        authorize.recheck().await?;
        let active = self.active.lock().await;
        authorize.recheck().await?;
        let proof = authorize.clone();
        let run_id = id.to_owned();
        let run = s
            .store
            .read(move |db| {
                proof.authorize(db)?;
                let run = required(db.run_record(&run_id)?, "Run not found")?;
                require_cleanup_ready(db, &run)?;
                Ok(run)
            })
            .await?;
        if active.contains_key(id) {
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
        if managed
            .iter()
            .any(|workspace| !matches!(workspace["kind"].as_str(), Some("worktree" | "clone")))
        {
            return Err(Error::conflict(
                "This run contains workspace copies whose changes cannot be checked before cleanup.",
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
            if workspace["kind"] == "worktree" {
                ensure_removable_worktree(&run, workspace, &env).await?;
            }
        }
        authorize.recheck().await?;
        let proof = authorize.clone();
        let run_id = id.to_owned();
        let (cleaned_at, resume_available) = s
            .store
            .transaction(move |db| {
                proof.authorize(db)?;
                let current = required(db.run_record(&run_id)?, "Run not found")?;
                require_cleanup_ready(db, &current)?;
                // Reserve the workspace against new input before removing files.
                // Keep the path until every removal succeeds so cleanup can retry.
                db.change_run(
                    &run_id,
                    crate::runs::RunChanges {
                        workspace_cleaned_at: Some(Some(
                            current.state.workspace_cleaned_at.unwrap_or_else(now),
                        )),
                        resume_available: Some(false),
                        ..Default::default()
                    },
                )?;
                Ok((
                    current.state.workspace_cleaned_at,
                    current.state.resume_available,
                ))
            })
            .await?;
        if let Err(error) = authorize.recheck().await {
            // No removal has started. Restore the state that this claim replaced.
            let run_id = id.to_owned();
            s.store
                .transaction(move |db| {
                    db.change_run(
                        &run_id,
                        crate::runs::RunChanges {
                            workspace_cleaned_at: Some(cleaned_at),
                            resume_available: Some(resume_available),
                            ..Default::default()
                        },
                    )?;
                    Ok(())
                })
                .await?;
            return Err(error);
        }
        remove_workspace(&run, &managed[0], &env).await?;
        for workspace in &managed[1..] {
            authorize.recheck().await?;
            remove_workspace(&run, workspace, &env).await?;
        }
        let patch = crate::runs::RunChanges {
            workspace: Some(None),
            ..Default::default()
        };
        let id = id.to_owned();
        authorize.recheck().await?;
        s.store
            .transaction(move |db| {
                authorize.authorize(db)?;
                db.change_run(&id, patch)?;
                db.audit("run.workspace.cleaned", &json!({ "id": id }))
            })
            .await?;
        Ok(json!({ "cleaned": true }))
    }
}

fn require_cleanup_ready(db: &Db<'_>, run: &crate::runs::RunRecord) -> Result<()> {
    if run.state.status.is_active() || run.state.recovery_pending {
        return Err(Error::conflict(
            "Wait for this run to finish before cleaning up.",
        ));
    }
    if run.wakeup.is_some() {
        return Err(Error::conflict(
            "Finish or cancel the scheduled continuation before cleaning up.",
        ));
    }
    if db.conversation_has_pending_work(&run.state.id)? {
        return Err(Error::conflict(
            "Resolve pending questions and messages and finish any coding agent change before cleaning up.",
        ));
    }
    Ok(())
}

/// Pauses queued follow-ups and requests the chat's current turn to stop, in the
/// caller's transaction. `executing` holds the runs this process is executing.
pub(crate) fn stop_chat_in(
    db: &Db<'_>,
    id: &str,
    executing: &std::collections::HashSet<String>,
) -> Result<Value> {
    let chat = crate::chats::pause_in(db, id, true)?;
    if let Some(run_id) = chat["runId"].as_str() {
        crate::chats::coding_agent::cancel_in(db, run_id)?;
        crate::environment_expiration::cancel_continuation_in(db, run_id)?;
        crate::chats::continuation::cancel_for_run(db, run_id)?;
        let run = required(db.run_state(run_id)?, "Run not found")?;
        let held = executing.contains(run_id)
            && RunCheckpoint::load_in(db, run_id)?
                .is_some_and(|saved| saved.held_environment.is_some());
        if run.status.is_active() || held {
            request_cancel(db, run_id, executing.contains(run_id))?;
        }
    }
    required(db.conversation_boundary(id)?, "Chat not found")
}

/// Cancelling a conversation's run or its wakeup pauses it as Stop does, in the same
/// transaction: no rule or later writer slips in before a person continues it (ADR-0091).
fn pause_conversation(db: &Db<'_>, run: &str) -> Result<()> {
    if let Some(chat) = db.conversation_for_run(run)?
        && !chat.paused
        && chat.lifecycle == cairn_contracts::ConversationLifecycle::Active
    {
        crate::chats::pause_in(db, &chat.id, true)?;
    }
    Ok(())
}

fn request_cancel(db: &Db<'_>, id: &str, is_active: bool) -> Result<()> {
    crate::environment_expiration::cancel_continuation_in(db, id)?;
    crate::chats::coding_agent::cancel_in(db, id)?;
    let run = required(db.run_state(id)?, "Run not found")?;
    let waiting = crate::chats::continuation::cancel_for_run(db, id)?;
    let held =
        RunCheckpoint::load_in(db, id)?.is_some_and(|saved| saved.held_environment.is_some());
    if !run.status.is_active() && !(is_active && held) {
        if waiting || crate::environment_expiration::pending(db, id)? {
            pause_conversation(db, id)?;
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
    pause_conversation(db, id)?;
    // Waiting means the agent process already exited. Release may still hold
    // its in-memory slot; cancellation must settle this wait even in that gap.
    // Executing or recovering runs are settled by their worker once stopped.
    if (!is_active || run.status == RunStatus::Waiting) && !run.recovery_pending {
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
pub(crate) fn requeue(
    db: &Db<'_>,
    id: &str,
    expected_turn_id: Option<&str>,
    selection: Option<&ResumeExecutionSelection>,
    selected_provider: Option<crate::provider::Provider>,
    saved_auto_catalog: Option<&Value>,
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
    crate::conversation_lifecycle::require_active_run(db, id)?;
    crate::chats::require_workspace_not_cleaned(&run)?;
    let replay_refused = db.conversations_boundary()?.iter().any(|chat| {
        chat["runId"] == id
            && (chat["cancelledByDeletion"] == true
                || chat["sessionRestartRequested"] == true
                || chat["environmentState"].is_string())
    });
    if replay_refused {
        return Err(Error::conflict(
            "Send a new message to continue; cancelled work will not replay.",
        ));
    }
    let checkpoint = RunCheckpoint::load_in(db, id)?;
    let status = state.status;
    // The wrapper can start and record a launch before Antigravity's first CLI
    // probe fails. Require evidence from that same attempt before retrying it.
    let antigravity_retry = if status == RunStatus::Failed
        && state.session_id.is_none()
        && crate::provider::Provider::of_run(&run).ok()
            == Some(crate::provider::Provider::Antigravity)
        && let Some(progress) = checkpoint.as_ref().and_then(|saved| {
            saved.progress.as_ref().filter(|progress| {
                saved.launched()
                    && saved.provider_failed()
                    && (saved.antigravity_probe_failed == Some(true)
                        || saved.last_error() == crate::antigravity::CLI_UNAVAILABLE)
                    && state.error.as_deref() == Some(saved.last_error())
                    && saved.launched_turn_before.is_none()
                    && saved.prepared().is_some()
                    && saved.output_interrupted != Some(true)
                    && saved.finalization_error().is_none()
                    && progress.complete
                    && progress.advanced_at.is_none()
                    && progress.runner_id.is_some()
                    && progress.runner_id.as_deref() == saved.runner_id()
                    && progress.turn_id.is_some()
                    && progress.turn_id.as_deref() == saved.launched_turn.as_deref()
                    && progress.turn_id.as_deref() == run["chatExecution"]["messageId"].as_str()
            })
        }) {
        Some(antigravity_retry_input(db, &run, progress.started_at)?)
    } else {
        None
    };
    let antigravity_probe_failed = antigravity_retry.is_some();
    // A chat that failed before its agent launched simply starts again.
    let before_launch = run["chatExecution"].is_object()
        && matches!(status, RunStatus::Failed | RunStatus::Cancelled)
        && (checkpoint.as_ref().is_none_or(|c| !c.launched()) || antigravity_probe_failed);
    let resumable = status.is_resumable()
        && state.resume_available
        && checkpoint.as_ref().is_some_and(|c| c.prepared().is_some());
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
    crate::resume_selection::validate_saved_opencode_auto_catalog(&run, saved_auto_catalog)?;
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
    // A manual resume may arrive before chat_tick observes the terminal turn.
    // Its scheduled continuation must not survive into the resumed execution.
    crate::chats::continuation::cancel_for_run(db, id)?;
    if crate::chats::turn_returned(db, id)? {
        return Err(Error::conflict(
            "This turn's message went back to the person who wrote it. Sending it again continues the conversation.",
        ));
    }
    if pending_switch && selection.is_some() {
        crate::chats::coding_agent::cancel_in(db, id)?;
    }
    if let Some(mut checkpoint) = checkpoint {
        // Manual Resume relinquishes reuse of the retained environment.
        // Keep its runner identity so recovery still fences it before launch.
        if let Some(held) = checkpoint.held_environment.take()
            && checkpoint.runner_id() == Some(held.attempt.as_str())
        {
            super::jobs::unknown(
                db,
                id,
                &held.attempt,
                "The previous environment will be stopped for manual resume; background completion is unknown.",
            )?;
        }
        checkpoint.remaining_ms = Some(crate::run_limits::budget_ms(&run["snapshot"]["agent"]));
        checkpoint.controller_recoveries = None;
        checkpoint.controller_recovery_reservation = None;
        checkpoint.provider_failure_attempt = None;
        checkpoint.acknowledge_finalization_observation();
        checkpoint.move_recovery_reservation = None;
        checkpoint.provider_retry_reservation = None;
        checkpoint.pending_paid_admission = None;
        checkpoint.controller_observation = None;
        checkpoint.turn_pending = Some(true);
        checkpoint.recovery_deadline_ms = None;
        checkpoint.output_interrupted = None;
        checkpoint.provider_failed = None;
        if let Some((_, execution)) = &antigravity_retry {
            // Keep the wrapper launch recorded so a different failure before
            // the next launch cannot use the generic before-launch replay.
            checkpoint.antigravity_probe_retry = Some(super::checkpoint::AntigravityProbeRetry {
                message_id: execution.message_id.clone(),
                started: false,
            });
            checkpoint.workspace_initialized = Some(true);
        }
        if !before_launch {
            checkpoint.completed = Some(false);
            checkpoint.terminal_code = Some(None);
        }
        checkpoint.settled = None;
        checkpoint.retired_environment_deadline = None;
        checkpoint.retained_completion = None;
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
    if let Some((mut message, mut execution)) = antigravity_retry {
        // Fence edits and withdrawals while the scheduler stops the old attempt.
        if message.status == MessageStatus::Queued {
            message.status = MessageStatus::Sending;
            db.save_message(&message)?;
        }
        execution.recovery = false;
        patch.boundary.as_mut().unwrap()["chatExecution"] = serde_json::to_value(execution)?;
    }
    db.change_run(id, patch)?;
    let result = required(db.run(id)?, "Run not found")?;
    let previous_turn = json!({
        "status": state.status.as_str(),
        "error": run["error"],
    });
    let context = crate::rules::engine::conversations::resume_context(db, id, &previous_turn)?;
    db.event(id, &EventKind::Status, "Resume requested", context.as_ref())?;
    Ok(result)
}

fn antigravity_retry_input(
    db: &Db<'_>,
    run: &Value,
    launched_at: i64,
) -> Result<(crate::chats::Message, ChatExecution)> {
    let changed = || {
        Error::conflict(
            "The failed Antigravity turn's message is no longer pending or has changed.",
        )
    };
    if run["chatMessageChanged"] == true {
        return Err(changed());
    }
    let execution: ChatExecution = serde_json::from_value(run["chatExecution"].clone())?;
    let chat = db
        .conversation_for_run(text(run, "id"))?
        .ok_or_else(changed)?;
    let message = db
        .conversation_message(&chat.id, &execution.message_id)?
        .ok_or_else(changed)?;
    if !matches!(
        message.status,
        MessageStatus::Queued | MessageStatus::Sending
    ) || message.created_at > launched_at
    {
        return Err(changed());
    }
    if let Some(digest) = run["chatMessageDigest"].as_str() {
        if crate::conversation_writers::entry_digest(&message)? != digest {
            return Err(changed());
        }
        return Ok((message, execution));
    }
    // Older turns did not record a digest. Check their frozen execution inputs
    // and refuse known edits after the original launch.
    let agent = &run["snapshot"]["agent"];
    let choices = [
        ("provider", message.provider.as_str()),
        ("model", message.model.as_str()),
        ("reasoning", message.reasoning.as_str()),
        ("speed", message.speed.as_str()),
    ];
    let same_input = message.question_id.is_none()
        && message.plan == execution.plan
        && message.attachments == execution.attachments
        && crate::chats::with_invoked_skills(&message.text, &run["snapshot"]["skills"])
            == execution.text
        && choices
            .iter()
            .all(|(field, value)| value.is_empty() || text(agent, field) == *value);
    if !same_input || crate::conversation_writers::message_written_after(db, &message, launched_at)?
    {
        return Err(changed());
    }
    Ok((message, execution))
}

/// Workspaces the run created; direct workspaces belong to the user.
fn managed_workspaces(run: &Value) -> Vec<Value> {
    let workspaces = run["workspaces"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| legacy_worktree(run).into_iter().collect());
    workspaces
        .into_iter()
        .filter(|workspace| workspace["kind"] != "direct")
        .collect()
}

pub(crate) fn legacy_worktree(run: &Value) -> Option<Value> {
    if !run["workspace"].is_string() || !run["snapshot"]["project"].is_object() {
        return None;
    }
    Some(json!({
        "projectId": run["snapshot"]["project"]["id"],
        "path": run["workspace"],
        "kind": "worktree",
    }))
}

async fn ensure_clean(workspace: &Value, root: &PathBuf, env: &Environment) -> Result<()> {
    let target = Path::new(text(workspace, "path"));
    if !target.starts_with(root)
        || target.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(Error::conflict(
            "Workspace is outside this run’s managed directory.",
        ));
    }
    // A revocation can interrupt a cleanup between workspaces. A retry may
    // acknowledge an already absent directory, but may never follow an alias.
    let boundary = root
        .parent()
        .ok_or_else(|| Error::conflict("Managed directory is missing."))?;
    let mut ancestor = target;
    loop {
        match tokio::fs::symlink_metadata(ancestor).await {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && ancestor != boundary => {
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| Error::conflict("Managed directory is missing."))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    if crate::skills::workspace(ancestor, &[boundary.to_path_buf()]).await? != ancestor {
        return Err(Error::conflict(
            "Workspace is outside this run’s managed directory.",
        ));
    }
    if ancestor != target {
        return Ok(());
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
    match tokio::fs::symlink_metadata(path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    if workspace["kind"] == "clone" {
        tokio::fs::remove_dir_all(path).await?;
        return Ok(());
    }
    let project = cleanup_project_path(run, workspace)?;
    let output = git(&["-C", &project, "worktree", "remove", path], env).await?;
    if !output.success {
        return Err(Error::conflict("Git could not remove this worktree."));
    }
    Ok(())
}

fn cleanup_project_path(run: &Value, workspace: &Value) -> Result<String> {
    run_projects(run)
        .into_iter()
        .find(|project| project["id"] == workspace["projectId"])
        .map(|project| text(&project, "path").to_owned())
        .filter(|path| !path.is_empty())
        .ok_or_else(|| Error::bad("Project not found"))
}

async fn ensure_removable_worktree(
    run: &Value,
    workspace: &Value,
    env: &Environment,
) -> Result<()> {
    let target = text(workspace, "path");
    if !tokio::fs::try_exists(target).await? {
        return Ok(());
    }
    let project = cleanup_project_path(run, workspace)?;
    let registered = git(
        &["-C", &project, "worktree", "list", "--porcelain", "-z"],
        env,
    )
    .await?;
    let path = format!("worktree {target}");
    let entry = registered
        .stdout
        .split("\0\0")
        .skip(1)
        .find(|entry| entry.split('\0').next() == Some(path.as_str()));
    let Some(entry) = entry.filter(|_| registered.success) else {
        return Err(Error::conflict(
            "Git could not verify this linked worktree.",
        ));
    };
    if entry
        .split('\0')
        .any(|field| field.split_ascii_whitespace().next() == Some("locked"))
    {
        return Err(Error::conflict("Unlock this worktree before cleaning up."));
    }
    let submodules = git(&["-C", target, "submodule", "status"], env).await?;
    if !submodules.success {
        return Err(Error::conflict(
            "Git could not verify the worktree's submodules.",
        ));
    }
    if submodules
        .stdout
        .lines()
        .any(|line| !line.is_empty() && !line.starts_with('-'))
    {
        return Err(Error::conflict(
            "This worktree has initialized submodules. Preserve their files before cleaning up.",
        ));
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
        ["runs", id, action @ ("cancel" | "cleanup")] => {
            async {
                let name = match *action {
                    "cancel" => "cancel_run",
                    _ => "cleanup_run",
                };
                crate::capabilities::run_operation(name)
                    .expect("run operation")
                    .invoke(
                        s,
                        json!({ "runId": id }),
                        crate::capabilities::ResumeContext::http(input)?,
                        crate::capabilities::member_account(input.identity.as_ref()),
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
                    .invoke(
                        s,
                        command,
                        context,
                        crate::capabilities::member_account(input.identity.as_ref()),
                    )
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

    #[tokio::test]
    async fn manual_resume_retries_only_a_confirmed_antigravity_cli_start_failure() {
        let unavailable = crate::antigravity::CLI_UNAVAILABLE;
        let redacted_probe = "The Antigravity CLI is not [redacted].";
        let original = "Use this workspace";
        let queued = Some(("queued", original, false, "queue"));
        for (error, final_error, provider_started, pending, should_retry) in [
            (unavailable, unavailable, false, queued, true),
            (redacted_probe, redacted_probe, false, queued, true),
            (
                unavailable,
                unavailable,
                false,
                Some(("queued", original, false, "steer")),
                true,
            ),
            (unavailable, unavailable, true, queued, false),
            (
                "Antigravity stopped unexpectedly.",
                "Antigravity stopped unexpectedly.",
                false,
                queued,
                false,
            ),
            (
                unavailable,
                "Run exceeded its time limit.",
                false,
                queued,
                false,
            ),
            (
                unavailable,
                unavailable,
                false,
                Some(("delivered", original, false, "queue")),
                false,
            ),
            (
                unavailable,
                unavailable,
                false,
                Some(("queued", "Edited", false, "queue")),
                false,
            ),
            (
                unavailable,
                unavailable,
                false,
                Some(("queued", original, true, "queue")),
                false,
            ),
            (unavailable, unavailable, false, None, false),
        ] {
            let root = tempfile::tempdir().unwrap();
            let store = crate::store::Store::open(root.path()).unwrap();
            store
                .transaction(move |db| {
                    db.add_run_fixture(
                        &json!({
                            "id": "run",
                            "taskId": "chat",
                            "status": "failed",
                            "error": final_error,
                            "trigger": "chat",
                            "createdAt": 0,
                            "workspace": "/workspace",
                            "chatExecution": {
                                "messageId": "queued-message",
                                "text": original,
                                "attachments": [],
                                "recovery": true,
                            },
                            "snapshot": {
                                "agent": {
                                    "id": crate::config::MAIN_AGENT_ID,
                                    "provider": "antigravity",
                                    "model": "gemini-4-neon",
                                    "reasoning": "high",
                                },
                            },
                        }),
                        None,
                    )?;
                    db.save_conversation_boundary(&json!({ "id": "chat", "runId": "run" }))?;
                    if let Some((status, text, changed_after_launch, mode)) = pending {
                        db.put_message(&json!({
                            "id": "queued-message",
                            "chatId": "chat",
                            "text": text,
                            "mode": mode,
                            "status": status,
                            "createdAt": 0,
                        }))?;
                        if changed_after_launch {
                            crate::conversation_writers::record_message(
                                db,
                                "chat",
                                "queued-message",
                                &crate::conversation_writers::EntryWriter::Agent,
                            )?;
                        }
                    }
                    let mut progress = super::super::progress::Progress::launched(
                        "execution".into(),
                        Some("runner".into()),
                        None,
                        Some("queued-message".into()),
                        0,
                    );
                    if provider_started {
                        progress.observe(&json!({ "type": "turn.started" }), 1);
                    }
                    progress.observe(&json!({ "type": "turn.failed" }), 2);
                    let checkpoint = RunCheckpoint {
                        launched: Some(true),
                        launched_turn: Some("queued-message".into()),
                        provider_failed: Some(true),
                        antigravity_probe_failed: (error == redacted_probe).then_some(true),
                        last_error: Some(Some(error.into())),
                        runner_id: Some(Some("runner".into())),
                        prepared: Some(Some(json!({ "cwd": "/workspace" }))),
                        progress: Some(progress),
                        ..Default::default()
                    };
                    checkpoint.store_in(db, "run")?;

                    let result = requeue(db, "run", Some("queued-message"), None, None, None);
                    if should_retry {
                        let run = result?;
                        assert_eq!(run["status"], "queued");
                        assert_eq!(run["workspace"], "/workspace");
                        assert_eq!(run["chatExecution"]["messageId"], "queued-message");
                        assert_eq!(run["chatExecution"]["text"], original);
                        assert_eq!(run["chatExecution"]["recovery"], false);
                        assert_eq!(run["snapshot"]["agent"]["model"], "gemini-4-neon");
                        let mut message =
                            db.conversation_message("chat", "queued-message")?.unwrap();
                        assert_eq!(message.status, MessageStatus::Sending);
                        assert!(
                            crate::chats::edit(db, "chat", "queued-message", None)
                                .unwrap_err()
                                .is_conflict()
                        );
                        let mut saved = RunCheckpoint::load_in(db, "run")?.unwrap();
                        assert!(saved.launched());
                        assert!(saved.retries_antigravity_without_session(Some("queued-message")));
                        assert_eq!(saved.workspace_initialized, Some(true));
                        assert_eq!(saved.runner_id(), Some("runner"));

                        // A second attempt can hit the same probe failure while
                        // the executable is still unavailable.
                        saved.record_launch(Some("queued-message".into()));
                        saved.provider_failed = Some(true);
                        saved.start_rejected = Some(false);
                        saved.last_error = Some(Some(unavailable.into()));
                        saved.runner_id = Some(Some("retry-runner".into()));
                        let mut progress = super::super::progress::Progress::launched(
                            "retry-execution".into(),
                            Some("retry-runner".into()),
                            None,
                            Some("queued-message".into()),
                            3,
                        );
                        progress.observe(&json!({ "type": "turn.failed" }), 4);
                        saved.progress = Some(progress);
                        saved.store_in(db, "run")?;
                        message.status = MessageStatus::Queued;
                        db.save_message(&message)?;
                        db.change_run(
                            "run",
                            crate::runs::RunChanges {
                                status: Some(RunStatus::Failed),
                                error: Some(Some(unavailable.into())),
                                recovery_pending: Some(false),
                                ..Default::default()
                            },
                        )?;
                        let retry = requeue(db, "run", Some("queued-message"), None, None, None)?;
                        assert_eq!(retry["status"], "queued");
                        assert_eq!(retry["chatExecution"]["messageId"], "queued-message");
                        assert_eq!(
                            db.conversation_message("chat", "queued-message")?
                                .unwrap()
                                .status,
                            MessageStatus::Sending
                        );

                        // A different failure before the next launch must not
                        // enter the generic replay path for a never-launched run.
                        message.status = MessageStatus::Queued;
                        db.save_message(&message)?;
                        db.change_run(
                            "run",
                            crate::runs::RunChanges {
                                status: Some(RunStatus::Failed),
                                error: Some(Some("Native cleanup is incomplete.".into())),
                                recovery_pending: Some(false),
                                ..Default::default()
                            },
                        )?;
                        crate::chats::edit(db, "chat", "queued-message", None)?;
                        assert!(
                            requeue(db, "run", Some("queued-message"), None, None, None)
                                .unwrap_err()
                                .is_conflict()
                        );
                        assert_eq!(db.run("run")?.unwrap()["status"], "failed");
                    } else {
                        assert!(result.unwrap_err().is_conflict());
                        assert_eq!(db.run("run")?.unwrap()["status"], "failed");
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn antigravity_probe_retry_rejects_edits_and_withdrawn_message_ids() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let run = json!({
                    "id": "run",
                    "taskId": "chat",
                    "trigger": "chat",
                    "status": "failed",
                    "createdAt": 0,
                    "chatExecution": { "messageId": "message" },
                    "snapshot": {
                        "agent": {
                            "id": crate::config::MAIN_AGENT_ID,
                            "provider": "antigravity",
                        },
                    },
                });
                db.add_run_fixture(&run, None)?;
                db.save_conversation_boundary(&json!({
                    "id": "chat",
                    "runId": "run",
                    "agentId": crate::config::MAIN_AGENT_ID,
                    "nativeProvenanceUnknown": true,
                }))?;
                db.put_message(&json!({
                    "id": "message",
                    "chatId": "chat",
                    "text": "Original input",
                    "mode": "queue",
                    "status": "queued",
                }))?;
                let mut message = db.conversation_message("chat", "message")?.unwrap();
                let digest = crate::conversation_writers::entry_digest(&message)?;
                db.change_run(
                    "run",
                    crate::runs::RunChanges {
                        boundary: Some(json!({ "chatMessageDigest": digest })),
                        ..Default::default()
                    },
                )?;
                let run = db.run("run")?.unwrap();
                antigravity_retry_input(db, &run, 0)?;
                message.mode = cairn_contracts::MessageMode::Steer;
                db.save_message(&message)?;
                assert!(
                    antigravity_retry_input(db, &run, 0)
                        .unwrap_err()
                        .is_conflict()
                );

                message.mode = cairn_contracts::MessageMode::Queue;
                db.save_message(&message)?;
                let values = serde_json::to_value(&message)?;
                crate::chats::edit(db, "chat", "message", Some(values))?;
                let edited_run = db.run("run")?.unwrap();
                assert!(
                    antigravity_retry_input(db, &edited_run, 0)
                        .unwrap_err()
                        .is_conflict()
                );

                // Exercise withdrawal independently of the preceding same-content edit.
                db.update_run_boundary("run", &json!({ "chatMessageChanged": null }))?;
                crate::chats::edit(db, "chat", "message", None)?;
                assert!(db.conversation_message("chat", "message")?.is_none());
                db.save_message(&message)?;
                let run = db.run("run")?.unwrap();
                assert!(
                    antigravity_retry_input(db, &run, 0)
                        .unwrap_err()
                        .is_conflict()
                );
                Ok(())
            })
            .await
            .unwrap();
    }

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
                    None,
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
            .transaction(move |db| {
                requeue(
                    db,
                    "chat-run",
                    Some("chat-turn"),
                    Some(&preserve),
                    Some(crate::provider::Provider::Codex),
                    None,
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
            .transaction(move |db| {
                requeue(
                    db,
                    "chat-run",
                    Some("chat-turn"),
                    Some(&explicit),
                    Some(crate::provider::Provider::Codex),
                    None,
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
    async fn worktree_refusals_are_identified_before_cleanup_claims_the_run() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let module = root.path().join("module");
        let target = root.path().join("worktree");
        let source = source.to_str().unwrap();
        let module = module.to_str().unwrap();
        let target = target.to_str().unwrap();
        let env = std::env::vars().collect();
        for command in [
            vec!["init", "-q", source],
            vec!["init", "-q", module],
            vec![
                "-C",
                module,
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "--allow-empty",
                "-qm",
                "Module",
            ],
            vec![
                "-C",
                source,
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                module,
                "nested",
            ],
            vec![
                "-C",
                source,
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-qm",
                "Project",
            ],
            vec!["-C", source, "worktree", "add", "-q", "--detach", target],
        ] {
            assert!(git(&command, &env).await.unwrap().success, "{command:?}");
        }
        let run = json!({ "snapshot": { "projects": [{ "id": "project", "path": source }] } });
        let workspace = json!({ "kind": "worktree", "projectId": "project", "path": target });
        ensure_removable_worktree(&run, &workspace, &env)
            .await
            .unwrap();
        assert!(
            git(&["-C", source, "worktree", "lock", target], &env)
                .await
                .unwrap()
                .success
        );
        let locked = ensure_removable_worktree(&run, &workspace, &env)
            .await
            .unwrap_err();
        assert!(locked.message.contains("Unlock this worktree"));
        assert!(
            git(&["-C", source, "worktree", "unlock", target], &env)
                .await
                .unwrap()
                .success
        );
        assert!(
            git(
                &[
                    "-C",
                    target,
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "update",
                    "--init"
                ],
                &env,
            )
            .await
            .unwrap()
            .success
        );
        let initialized = ensure_removable_worktree(&run, &workspace, &env)
            .await
            .unwrap_err();
        assert!(initialized.message.contains("initialized submodules"));
        assert!(Path::new(target).join("nested/.git").is_file());
    }

    #[tokio::test]
    async fn cleanup_preserves_the_workspace_needed_to_answer_a_question() {
        let f = crate::plugin_grant_fixture::Fixture::new().await;
        let running = f.s.store.run(&f.run).await.unwrap();
        f.s.chat_acknowledge(&f.run, text(&running["chatExecution"], "messageId"))
            .await
            .unwrap();
        let run = f.run.clone();
        f.s.store
            .transaction(move |db| {
                let chat = db.conversation_for_run(&run)?.unwrap();
                db.change_run(
                    &run,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Succeeded),
                        workspace: Some(Some("/saved/workspace".into())),
                        ..Default::default()
                    },
                )?;
                db.save_question(&cairn_contracts::ChatQuestion {
                    id: crate::config::id(),
                    chat_id: chat.id,
                    run_id: run,
                    blocking: true,
                    source: String::new(),
                    status: crate::chats::QuestionStatus::Pending,
                    fields: vec![serde_json::from_value(json!({
                        "id": "review",
                        "title": "What should I review next?",
                    }))?],
                    created_at: Some(now()),
                    message_id: None,
                })
            })
            .await
            .unwrap();
        let refused = f.s.worker.cleanup(&f.s, &f.run).await.unwrap_err();
        assert_eq!(refused.status, 409);
        assert!(refused.message.contains("pending questions and messages"));
        let run = f.s.store.run(&f.run).await.unwrap();
        assert_eq!(run["workspace"], "/saved/workspace");
        assert!(run["workspaceCleanedAt"].is_null());
        f.s.shutdown.cancel();
    }

    #[tokio::test]
    async fn cleanup_can_retry_after_revocation_between_managed_directories() {
        let f = crate::plugin_grant_fixture::Fixture::new().await;
        let running = f.s.store.run(&f.run).await.unwrap();
        f.s.chat_acknowledge(&f.run, text(&running["chatExecution"], "messageId"))
            .await
            .unwrap();
        let root =
            f.s.config
                .data_dir
                .join("runs")
                .join(&f.run)
                .join("workspace");
        let first = root.join("first");
        let second = root.join("second");
        let env = std::env::vars().collect();
        for path in [&first, &second] {
            tokio::fs::create_dir_all(path).await.unwrap();
            assert!(
                git(&["-C", path.to_str().unwrap(), "init"], &env)
                    .await
                    .unwrap()
                    .success
            );
        }
        let mut snapshot = f.s.store.run(&f.run).await.unwrap()["snapshot"].clone();
        snapshot["task"]["worktree"] = true.into();
        f.s.store
            .change_run(
                &f.run,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Cancelled),
                    snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                    workspace: Some(Some(root.to_string_lossy().into_owned())),
                    resume_available: Some(true),
                    boundary: Some(json!({
                        "isolated": false,
                        "workspaces": [
                            { "path": first, "kind": "clone" },
                            { "path": second, "kind": "clone" },
                        ],
                    })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let claimed_run = f.run.clone();
        let revoke_before_removal = crate::authorization::Authorization::new(&f.s, move |db| {
            let state = db.run_state(&claimed_run)?.unwrap();
            if state.workspace_cleaned_at.is_some() {
                return Err(Error::forbidden("Revoked before removal."));
            }
            Ok(())
        });
        let error =
            f.s.worker
                .cleanup_live(&f.s, &f.run, revoke_before_removal)
                .await
                .unwrap_err();
        assert_eq!(error.status, 403);
        assert!(first.exists() && second.exists());
        let untouched = f.s.store.run(&f.run).await.unwrap();
        assert!(untouched["workspaceCleanedAt"].is_null());
        assert_eq!(untouched["resumeAvailable"], true);

        let watched = first.clone();
        let authorize = crate::authorization::Authorization::new(&f.s, move |_| {
            if !watched.exists() {
                return Err(Error::forbidden("Revoked during cleanup."));
            }
            Ok(())
        });
        let error =
            f.s.worker
                .cleanup_live(&f.s, &f.run, authorize)
                .await
                .unwrap_err();
        assert_eq!(error.status, 403);
        assert!(!first.exists());
        assert!(second.exists());
        let retry = f.s.worker.cleanup(&f.s, &f.run).await.unwrap();
        assert_eq!(retry["cleaned"], true);
        assert!(!second.exists());
        assert!(f.s.store.run(&f.run).await.unwrap()["workspace"].is_null());
        f.s.shutdown.cancel();
    }

    #[tokio::test]
    async fn absent_cleanup_paths_must_still_resolve_under_the_managed_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = root.path().join("alias");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let env = std::env::vars().collect();
        let removed_root = root.path().join("removed-workspace-root");
        ensure_clean(&json!({ "path": removed_root }), &removed_root, &env)
            .await
            .unwrap();
        let missing = root.path().join("already-removed");
        ensure_clean(
            &json!({ "path": missing }),
            &root.path().to_path_buf(),
            &env,
        )
        .await
        .unwrap();
        assert!(
            ensure_clean(
                &json!({ "path": link.join("missing") }),
                &root.path().to_path_buf(),
                &env
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn cleanup_claim_prevents_resuming_a_run_that_has_not_launched() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({
                        "id": "run", "taskId": "chat", "trigger": "chat", "status": "cancelled",
                        "workspace": "/saved/workspace", "workspaceCleanedAt": 10,
                        "chatExecution": { "messageId": "held-turn" },
                    }),
                    None,
                )?;
                let error = requeue(db, "run", Some("held-turn"), None, None, None).unwrap_err();
                assert!(error.message.contains("workspace has been cleaned up"));
                assert_eq!(db.run("run")?.unwrap()["status"], "cancelled");
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn manual_resume_keeps_the_saved_turn_and_clears_transient_state() {
        for completed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = crate::store::Store::open(root.path()).unwrap();
            store
                .transaction(move |db| {
                    db.add_run_fixture(&json!({
                    "id": "run", "taskId": "chat", "trigger": "chat", "status": "cancelled", "createdAt": 0,
                    "resumeAvailable": true, "chatExecution": { "messageId": "previous-turn" },
                }), None)?;
                    db.save_checkpoint_boundary("run", &json!({
                    "completed": completed, "outputInterrupted": true, "launched": true, "prepared": { "cwd": "/workspace" },
                        "retiredEnvironmentDeadline": 1000,
                        "retainedCompletion": {"messageId": "previous", "finishedAt": 900},
                }))?;
                    let resumed = requeue(db, "run", None, None, None, None)?;
                    assert_eq!(resumed["status"], "queued");
                    let checkpoint = db.checkpoint_boundary("run")?.unwrap();
                    assert!(checkpoint["outputInterrupted"].is_null());
                    assert!(checkpoint["retiredEnvironmentDeadline"].is_null());
                    assert!(checkpoint["retainedCompletion"].is_null());
                    assert_eq!(resumed["chatExecution"]["messageId"], "previous-turn");
                    Ok(())
                })
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn cancelling_a_conversation_run_pauses_it_at_once() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({
                        "id": "run",
                        "taskId": "chat",
                        "status": "running",
                        "trigger": "chat",
                        "createdAt": 0,
                    }),
                    None,
                )?;
                db.save_conversation_boundary(&json!({ "id": "chat", "runId": "run" }))?;
                // As from the run view or cancel_run: no rule can write before it pauses.
                request_cancel(db, "run", true)?;
                assert_eq!(db.conversation_boundary("chat")?.unwrap()["paused"], true);
                Ok(())
            })
            .await
            .unwrap();
        // Cancelling only a scheduled wakeup pauses the conversation too.
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({
                        "id": "waiting",
                        "taskId": "waiting-chat",
                        "status": "succeeded",
                        "trigger": "chat",
                        "createdAt": 0,
                    }),
                    None,
                )?;
                db.save_conversation_boundary(&json!({
                    "id": "waiting-chat",
                    "runId": "waiting",
                    "wakeup": {
                        "id": "wake", "messageId": null, "reason": "Wait for CI",
                        "seconds": 3600, "createdAt": 0, "wakeAt": 3600000,
                    },
                }))?;
                request_cancel(db, "waiting", false)?;
                let chat = db.conversation_boundary("waiting-chat")?.unwrap();
                assert_eq!(chat["paused"], true);
                assert!(chat["wakeup"].is_null());
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn completed_provider_with_live_jobs_can_be_cancelled() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({"id":"held", "taskId":"task", "status":"succeeded"}),
                    None,
                )?;
                let mut saved = RunCheckpoint::fresh(None);
                saved.runner_id = Some(Some("attempt".into()));
                saved.held_environment = Some(super::super::checkpoint::HeldEnvironment {
                    attempt: "attempt".into(),
                    generation: "generation".into(),
                    expires_at: now() + 60_000,
                });
                saved.store_in(db, "held")?;
                request_cancel(db, "held", true)?;
                assert!(db.run_state("held")?.unwrap().cancel_requested_at.is_some());
                Ok(())
            })
            .await
            .unwrap();
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
