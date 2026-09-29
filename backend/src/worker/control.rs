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
        let active = self.active.lock().await;
        let is_active = active.contains_key(id);
        let run_id = id.to_owned();
        s.store
            .transaction(move |db| request_cancel(db, &run_id, is_active))
            .await?;
        if let Some(cancel) = active.get(id) {
            cancel.cancel();
        }
        self.notify();
        Ok(())
    }

    pub async fn resume(&self, s: &Service, id: &str) -> Result<Value> {
        let active = self.active.lock().await;
        if active.contains_key(id) {
            return Err(Error::conflict(
                "Wait for this run to finish stopping before resuming.",
            ));
        }
        let id = id.to_owned();
        let result = s.store.transaction(move |db| requeue(db, &id)).await?;
        self.notify();
        Ok(result)
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
        let patch = json!({
            "workspace": null,
            "resumeAvailable": false,
            "workspaceCleanedAt": now(),
        });
        s.store.patch_run(id, patch).await?;
        s.store
            .audit("run.workspace.cleaned", json!({ "id": id }))
            .await?;
        Ok(json!({ "cleaned": true }))
    }
}

fn request_cancel(db: &Db<'_>, id: &str, is_active: bool) -> Result<()> {
    let run = required(db.run(id)?, "Run not found")?;
    if !RunStatus::of(&run).is_some_and(RunStatus::is_active) {
        return Err(Error::conflict("This run has already finished."));
    }
    db.patch_run(id, &json!({ "cancelRequestedAt": now() }))?;
    // An executing or recovering run is settled by its worker once it stops.
    if !is_active && run["recoveryPending"] != true {
        let patch = json!({
            "status": RunStatus::Cancelled,
            "finishedAt": now(),
            "summary": "Cancelled before execution.",
        });
        db.patch_run(id, &patch)?;
    }
    db.audit("run.cancelled", &json!({ "id": id }))
}

/// Queues a stopped run to continue its saved conversation.
fn requeue(db: &Db<'_>, id: &str) -> Result<Value> {
    let run = required(db.run(id)?, "Run not found")?;
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
    if let Some(mut checkpoint) = checkpoint {
        checkpoint.remaining_ms = Some(crate::run_limits::budget_ms(&run["snapshot"]["agent"]));
        if !before_launch {
            checkpoint.completed = Some(false);
        }
        checkpoint.settled = None;
        checkpoint.store_in(db, id)?;
    }
    let wait_reason = (!before_launch).then_some("Resuming saved conversation.");
    let patch = json!({
        "status": RunStatus::Queued,
        "error": null,
        "outcome": null,
        "recoveryPending": true,
        "cancelRequestedAt": null,
        "finishedAt": null,
        "accountWaitReason": wait_reason,
    });
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
        ["runs", id, "resume"] => s.worker.resume(s, id).await,
        ["runs", id, "cleanup"] => s.worker.cleanup(s, id).await,
        ["chats", id, "pause"] => pause_chat(s, input, id).await,
        ["chats", id, "stop"] => stop_chat(s, id).await,
        _ => return None,
    };
    Some(result)
}

async fn pause_chat(s: &Service, input: &crate::http::Input, id: &str) -> Result<Value> {
    let paused = input.boolean("paused")?;
    let chat = s.get("chats", id).await?;
    crate::conversation_lifecycle::require_active(&chat)?;
    if !paused && let Some(run_id) = chat["runId"].as_str() {
        let run = s.store.run(run_id).await?;
        if RunStatus::of(&run).is_some_and(RunStatus::is_resumable)
            && chat["cancelledByDeletion"] != true
            && chat["sessionRestartRequested"] != true
        {
            s.worker.resume(s, run_id).await?;
        }
    }
    s.store.delete(&format!("chat-error:{id}")).await?;
    s.chat_pause(id, paused).await
}

async fn stop_chat(s: &Service, id: &str) -> Result<Value> {
    let chat = s.get("chats", id).await?;
    s.chat_pause(id, true).await?;
    if let Some(run_id) = chat["runId"].as_str()
        && RunStatus::of(&s.store.run(run_id).await?).is_some_and(RunStatus::is_active)
    {
        s.worker.cancel(s, run_id).await?;
    }
    Ok(json!({ "stopped": true }))
}
