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
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

impl Worker {
    pub async fn cancel(&self, s: &Service, id: &str) -> Result<()> {
        self.cancel_authorized(s, id, |_| Ok(())).await
    }

    pub(crate) async fn cancel_from_run(&self, s: &Service, id: &str, bearer: &str) -> Result<()> {
        let bearer = bearer.to_owned();
        self.cancel_authorized(s, id, move |db| {
            crate::installation_mcp::authorize_in(db, &bearer).map(|_| ())
        })
        .await
    }

    pub(crate) async fn cancel_authorized<F>(
        &self,
        s: &Service,
        id: &str,
        authorize: F,
    ) -> Result<()>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let active = self.active.lock().await;
        let is_active = active.contains_key(id);
        let run_id = id.to_owned();
        s.store
            .transaction(move |db| {
                authorize(db)?;
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
                    crate::chats::continuation::cancel_for_run(db, run_id)?;
                    let run = required(db.run(run_id)?, "Run not found")?;
                    if RunStatus::of(&run).is_some_and(RunStatus::is_active) {
                        request_cancel(db, run_id, executing.contains(run_id))?;
                    }
                }
                required(db.get("chats", &id)?, "Chat not found")
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
                requeue(db, &id)
            })
            .await?;
        self.notify();
        Ok(result)
    }

    /// An old process must finish preparation and stopping before another author
    /// can reuse its workspace with a fresh native session.
    pub(crate) async fn request_new_turn_authorized<F>(
        &self,
        s: &Service,
        run: &str,
        chat: &str,
        authorize: F,
    ) -> Result<bool>
    where
        F: FnOnce(&Db<'_>) -> Result<bool> + Send + 'static,
    {
        let preparation = self.run_preparation_lock(run).await;
        let Ok(_preparation) = preparation.try_lock_owned() else {
            return Ok(false);
        };
        let Ok(active) = self.active.try_lock() else {
            return Ok(false);
        };
        if active.contains_key(run) {
            return Ok(false);
        }
        let (run, chat) = (run.to_owned(), chat.to_owned());
        s.store
            .transaction(move |db| {
                if !authorize(db)? {
                    return Ok(false);
                }

                crate::chats::request_next_turn_in(db, &chat, &run)
            })
            .await
    }

    pub async fn cleanup(&self, s: &Service, id: &str) -> Result<Value> {
        let active = self.active.lock().await;
        let run = s.store.run(id).await?;
        if RunStatus::of(&run).is_some_and(RunStatus::is_active) || active.contains_key(id) {
            return Err(Error::conflict(
                "Wait for this run to finish before cleaning up.",
            ));
        }
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
        for workspace in &managed {
            remove_workspace(&run, workspace, &env).await?;
        }
        let patch =
            json!({ "workspace": null, "resumeAvailable": false, "workspaceCleanedAt": now() });
        s.store.patch_run(id, patch).await?;
        s.store
            .audit("run.workspace.cleaned", json!({ "id": id }))
            .await?;
        Ok(json!({ "cleaned": true }))
    }
}

fn request_cancel(db: &Db<'_>, id: &str, is_active: bool) -> Result<()> {
    let run = required(db.run(id)?, "Run not found")?;
    let waiting = crate::chats::continuation::cancel_for_run(db, id)?;
    if !RunStatus::of(&run).is_some_and(RunStatus::is_active) {
        if waiting {
            return db.audit("run.wakeup.cancelled", &json!({ "id": id }));
        }
        return Err(Error::conflict("This run has already finished."));
    }
    db.patch_run(id, &json!({ "cancelRequestedAt": now() }))?;
    // Waiting means the agent process already exited. Release may still hold
    // its in-memory slot; cancellation must settle this wait even in that gap.
    // Executing or recovering runs are settled by their worker once stopped.
    if (!is_active || run["status"] == RunStatus::Waiting) && run["recoveryPending"] != true {
        let patch = json!({
            "status": RunStatus::Cancelled,
            "finishedAt": now(),
            "summary": "Cancelled before execution.",
            "retry": null,
        });
        db.patch_run(id, &patch)?;
        // A run waiting for an automatic retry shows the stop in its conversation; a wait's
        // own card already shows its cancellation.
        if run["retry"]["nextAttemptAt"].is_number() {
            db.event(id, "status", RunStatus::Cancelled.as_str(), None)?;
        }
    }
    db.audit("run.cancelled", &json!({ "id": id }))
}

/// Queues a stopped run to continue its saved conversation.
fn requeue(db: &Db<'_>, id: &str) -> Result<Value> {
    let run = required(db.run(id)?, "Run not found")?;
    if run["backendMigration"].is_object() {
        return Err(Error::conflict(
            "Wait for the backend migration to finish before resuming.",
        ));
    }
    crate::conversation_lifecycle::require_active_run(db, id)?;
    let replay_refused = db.list("chats")?.iter().any(|chat| {
        chat["runId"] == id
            && (chat["cancelledByDeletion"] == true || chat["sessionRestartRequested"] == true)
    });
    if replay_refused {
        return Err(Error::conflict(
            "Send a new message and resume the conversation queue to continue; cancelled work will not replay.",
        ));
    }
    let checkpoint = RunCheckpoint::load_in(db, id)?;
    let status = RunStatus::of(&run);
    // A chat that failed before its process launched simply starts again.
    let before_launch = run["chatExecution"].is_object()
        && matches!(status, Some(RunStatus::Failed | RunStatus::Cancelled))
        && checkpoint.as_ref().is_none_or(|c| !c.launched());
    let resumable = status.is_some_and(RunStatus::is_resumable)
        && run["resumeAvailable"] == true
        && checkpoint.as_ref().is_some_and(|c| c.prepared().is_some())
        && run["workspaceCleanedAt"].is_null();
    if !before_launch && !resumable {
        return Err(Error::conflict(
            "This run has no saved conversation available to resume.",
        ));
    }
    if db.active()?.iter().any(|a| a["taskId"] == run["taskId"]) {
        return Err(Error::conflict("This task already has an active run."));
    }
    // A parked mission completed its provider turn. Explicitly continuing it
    // creates a new turn, while recovery of interrupted turns keeps its identity.
    let completed_mission =
        run["trigger"] != "chat" && checkpoint.as_ref().is_some_and(RunCheckpoint::completed);
    // A manual resume may arrive before chat_tick observes the terminal turn.
    // Its scheduled continuation must not survive into the resumed execution.
    crate::chats::continuation::cancel_for_run(db, id)?;
    if let Some(mut checkpoint) = checkpoint {
        checkpoint.remaining_ms = Some(crate::run_limits::budget_ms(&run["snapshot"]["agent"]));
        if !before_launch {
            checkpoint.completed = Some(false);
        }
        checkpoint.settled = None;
        // A confirmed pre-turn failure may have no session yet. Its safety
        // evidence survives a manual budget reset until the next supervised launch.
        let pending_startup = !run["sessionId"].is_string()
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
    let mut patch = json!({
        "status": RunStatus::Queued,
        "error": null,
        "outcome": null,
        "recoveryPending": true,
        "cancelRequestedAt": null,
        "finishedAt": null,
        "accountWaitReason": wait_reason,
        "retry": null,
    });
    if completed_mission {
        patch["missionTurnId"] = crate::config::id().into();
        patch["waitContinuation"] = Value::Null;
    }
    let result = db.patch_run(id, &patch)?;
    db.event(id, "status", "Resume requested", None)?;
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
        ["runs", id, "cancel"] => s.worker.cancel(s, id).await.map(|()| json!({ "ok": true })),
        ["runs", id, "resume"] => {
            async {
                let command = crate::capabilities::RESUME_RUN.decode(json!({ "runId": id }))?;
                let context = crate::capabilities::ResumeContext::http(input)?;
                let output = crate::capabilities::RESUME_RUN
                    .invoke(s, command, context)
                    .await?;
                Ok(serde_json::to_value(output)?)
            }
            .await
        }
        ["runs", id, "cleanup"] => s.worker.cleanup(s, id).await,
        ["chats", id, "pause"] => pause_chat(s, input, id).await,
        ["chats", id, "stop"] => stop_chat(s, id).await,
        _ => return None,
    };
    Some(result)
}

async fn pause_chat(s: &Service, input: &crate::http::Input, id: &str) -> Result<Value> {
    let paused = input.boolean("paused")?;
    let authorize = crate::native::policy::chat_authorization(id);
    let chat = s.get("chats", id).await?;
    crate::conversation_lifecycle::require_active(&chat)?;
    if !paused && let Some(run_id) = chat["runId"].as_str() {
        let run = s.store.run(run_id).await?;
        let queued_new_turn = s
            .store
            .read({
                let (chat, run) = (chat.clone(), run.clone());
                move |db| crate::plugins::turns::pending(db, &chat, &run)
            })
            .await?;
        if RunStatus::of(&run).is_some_and(RunStatus::is_resumable)
            && !queued_new_turn
            && chat["cancelledByDeletion"] != true
            && chat["sessionRestartRequested"] != true
        {
            s.worker
                .resume_authorized(s, run_id, authorize.clone())
                .await?;
        }
    }
    s.store.delete(&format!("chat-error:{id}")).await?;
    if paused {
        return s.chat_pause_by_user(id, true).await;
    }
    let id = id.to_owned();
    let result = s
        .store
        .transaction(move |db| {
            authorize(db)?;
            crate::chats::pause_in(db, &id, false)
        })
        .await?;
    s.worker.notify();
    Ok(result)
}

async fn stop_chat(s: &Service, id: &str) -> Result<Value> {
    let chat = s.get("chats", id).await?;
    s.chat_pause_by_user(id, true).await?;
    s.chat_cancel_wait(id).await?;
    if let Some(run_id) = chat["runId"].as_str()
        && RunStatus::of(&s.store.run(run_id).await?).is_some_and(RunStatus::is_active)
    {
        s.worker.cancel(s, run_id).await?;
    }
    Ok(json!({ "stopped": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manual_mission_resume_changes_completed_turn_identity_but_keeps_interrupted_turns() {
        for completed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = crate::store::Store::open(root.path()).unwrap();
            store
                .transaction(move |db| {
                    db.add_run(&json!({
                    "id": "mission", "taskId": "task", "status": "cancelled", "createdAt": 0,
                    "resumeAvailable": true, "missionTurnId": "previous-turn",
                    "waitContinuation": { "observation": { "state": "completed" } },
                }), None)?;
                    db.set("run-checkpoint:mission", &json!({
                    "completed": completed, "launched": true, "prepared": { "cwd": "/workspace" },
                }), None)?;
                    let resumed = requeue(db, "mission")?;
                    assert_eq!(resumed["status"], "queued");
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
                db.add_run(
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
