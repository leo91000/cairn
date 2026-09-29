use super::{
    checkpoint::{Checkpoint, RunCheckpoint, Settled},
    launch,
};
use crate::{
    accounts::Lease,
    config::now,
    error::{Error, Result},
    provider::Provider,
    recovery, run_limits, run_output,
    run_status::RunStatus,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const MAX_CONTROLLER_RECOVERIES: u64 = 3;
const VM_STOP_WAIT_REASON: &str = "Waiting for the previous VM to stop.";

/// Executes a run, records how it ended and releases what it held once its
/// process is confirmed stopped.
pub(super) async fn execute(
    s: &Arc<Service>,
    mut run: Value,
    mut account: Option<Lease>,
    cancel: CancellationToken,
) -> Result<()> {
    let run_id = text(&run, "id").to_owned();
    let saved = RunCheckpoint::load(&s.store, &run_id).await?;
    let existing = saved.is_some();
    let state = saved
        .unwrap_or_else(|| RunCheckpoint::fresh(run_limits::budget_ms(&run["snapshot"]["agent"])));
    let checkpoint = Arc::new(Checkpoint::new(s.store.clone(), run_id, state));
    let stop_heartbeat = CancellationToken::new();
    let _heartbeat_guard = stop_heartbeat.clone().drop_guard();
    let heartbeat = tokio::spawn(heartbeat(
        s.clone(),
        checkpoint.clone(),
        stop_heartbeat.clone(),
    ));
    let mut sensitive = Vec::new();
    let result = launch::execute(
        s,
        &mut run,
        &mut account,
        &cancel,
        &checkpoint,
        existing,
        &mut sensitive,
    )
    .await;
    if let Err(error) = result {
        record_failure(s, &checkpoint, &cancel, &error, &sensitive).await?;
    }
    let fenced = fence(s, &checkpoint).await?;
    stop_heartbeat.cancel();
    let _ = heartbeat.await;
    checkpoint.persist().await?;
    release(s, &checkpoint, account.as_ref(), fenced).await
}

/// Keeps the checkpoint, node lease and pending moves current while the run executes.
async fn heartbeat(s: Arc<Service>, checkpoint: Arc<Checkpoint>, stop: CancellationToken) {
    let mut timer = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            _ = timer.tick() => {
                let run_id = checkpoint.id.as_str();
                // Both fail routinely once the run is stopping, moving or revoked;
                // the next beat retries and execution notices the revocation itself.
                let _ = crate::nodes::moves::pause_pending(&s, run_id).await;
                let _ = crate::nodes::placement::renew_local(&s, run_id).await;
                if let Err(error) = checkpoint.persist().await {
                    tracing::warn!(run_id, error = %error, "Could not save the run checkpoint");
                }
            }
        }
    }
}

async fn record_failure(
    s: &Service,
    checkpoint: &Checkpoint,
    cancel: &CancellationToken,
    error: &Error,
    sensitive: &[String],
) -> Result<()> {
    let run_id = checkpoint.id.as_str();
    // Another conversation took the last room after the scheduler checked:
    // wait again instead of failing.
    if !cancel.is_cancelled() && crate::nodes::placement::is_no_capacity(error) {
        let patch = json!({
            "status": RunStatus::Queued,
            "recoveryPending": true,
            "finishedAt": null,
        });
        let run = s.store.patch_run(run_id, patch).await?;
        return super::schedule::report_capacity_wait(s, &run, &error.message).await;
    }
    let saved = checkpoint.snapshot().await;
    let recover_controller = recovers_controller(s, checkpoint, &saved, cancel, error).await?;
    if recover_controller {
        let recoveries = saved.controller_recoveries() + 1;
        checkpoint
            .update(|c| c.controller_recoveries = Some(recoveries))
            .await?;
    }
    let status = if cancel.is_cancelled() {
        RunStatus::Cancelled
    } else if s.shutdown.is_cancelled() || recover_controller {
        RunStatus::Queued
    } else {
        RunStatus::Failed
    };
    let needs_fence = saved.isolated() && saved.runner_id().is_some();
    let summary = run_output::redact(&error.message, sensitive);
    if needs_fence && status != RunStatus::Queued {
        let settled = Settled::failure(status, &summary);
        checkpoint
            .update(|c| c.settled = Some(Some(settled)))
            .await?;
    }
    s.store
        .patch_run(run_id, failure_patch(status, needs_fence, &summary))
        .await?;
    let kind = if s.shutdown.is_cancelled() {
        "status"
    } else {
        "error"
    };
    s.store.event(run_id, kind, &summary, None).await
}

/// A VM controller that became unavailable is retried a few times, and
/// indefinitely while the run is moving or placed on a remote node.
async fn recovers_controller(
    s: &Service,
    checkpoint: &Checkpoint,
    saved: &RunCheckpoint,
    cancel: &CancellationToken,
    error: &Error,
) -> Result<bool> {
    if !error.is_unavailable() || !saved.firecracker() {
        return Ok(false);
    }
    let run = s.store.run(&checkpoint.id).await?;
    let remote = saved
        .node_id()
        .is_some_and(|node| node != crate::nodes::LOCAL_NODE_ID);
    let may_retry = saved.controller_recoveries() < MAX_CONTROLLER_RECOVERIES
        || run["moveRequest"].is_object()
        || remote;
    Ok(may_retry
        && run["sessionId"].is_string()
        && !checkpoint.expired()
        && !cancel.is_cancelled()
        && !s.shutdown.is_cancelled())
}

/// A run whose VM may still be running stays queued for recovery; its final
/// fields are settled from the checkpoint once the VM is fenced.
fn failure_patch(status: RunStatus, needs_fence: bool, summary: &str) -> Value {
    let recovery_pending = needs_fence || status == RunStatus::Queued;
    let finished_at = (!recovery_pending).then(now);
    let error = (status == RunStatus::Failed).then_some(summary);
    let wait_reason = if needs_fence {
        Some(VM_STOP_WAIT_REASON)
    } else if status == RunStatus::Queued {
        Some("Paused for worker restart. This run will resume automatically.")
    } else {
        None
    };
    let status = if needs_fence {
        RunStatus::Queued
    } else {
        status
    };
    json!({
        "status": status,
        "recoveryPending": recovery_pending,
        "finishedAt": finished_at,
        "summary": summary,
        "error": error,
        "accountWaitReason": wait_reason,
    })
}

/// Returns whether the execution is confirmed stopped. Otherwise the run is
/// requeued for recovery with its outcome saved in the checkpoint.
async fn fence(s: &Service, checkpoint: &Checkpoint) -> Result<bool> {
    let run_id = checkpoint.id.as_str();
    let has_runner = checkpoint
        .read(|c| c.isolated() && c.runner_id().is_some())
        .await;
    if !has_runner
        || recovery::fence(s, &s.store.run(run_id).await?)
            .await
            .is_ok()
    {
        return Ok(true);
    }
    let current = s.store.run(run_id).await?;
    if current["status"] != RunStatus::Queued
        && let Some(settled) = Settled::from_run(&current)
    {
        checkpoint
            .update(|c| c.settled = Some(Some(settled)))
            .await?;
    }
    let patch = json!({
        "status": RunStatus::Queued,
        "recoveryPending": true,
        "finishedAt": null,
        "accountWaitReason": VM_STOP_WAIT_REASON,
    });
    s.store.patch_run(run_id, patch).await?;
    Ok(false)
}

async fn release(
    s: &Service,
    checkpoint: &Checkpoint,
    account: Option<&Lease>,
    fenced: bool,
) -> Result<()> {
    let run_id = checkpoint.id.as_str();
    let saved = checkpoint.snapshot().await;
    if fenced && let Some(settled) = saved.settled() {
        s.store.patch_run(run_id, settled.run_patch()?).await?;
    }
    if fenced
        && let Some(account) = account
        && s.accounts.release(account).await.is_err()
    {
        let detail = json!({ "id": account.account_id, "runId": run_id });
        s.store.audit("account.release_failed", detail).await?;
    }
    s.mcps.revoke_run(s, run_id).await?;
    requeue_pending_move(s, checkpoint).await?;
    if fenced && saved.firecracker() {
        let run = s.store.run(run_id).await?;
        if run["status"] == RunStatus::Succeeded {
            crate::nodes::publication::attempt(s, &run).await;
        }
    }
    if fenced {
        clear_credentials(s, run_id).await;
    }
    if let Some(runner) = saved.runner_id() {
        let plan = s
            .config
            .data_dir
            .join("runner-plans")
            .join(format!("{runner}.json"));
        // The plan is gone when the runner never started.
        let _ = tokio::fs::remove_file(plan).await;
    }
    Ok(())
}

/// A move requested during execution relaunches the run on its new node.
async fn requeue_pending_move(s: &Service, checkpoint: &Checkpoint) -> Result<()> {
    let run_id = checkpoint.id.as_str();
    let run = s.store.run(run_id).await?;
    if !run["moveRequest"].is_object() || !run["cancelRequestedAt"].is_null() {
        return Ok(());
    }
    checkpoint
        .update(|c| {
            c.completed = Some(false);
            c.settled = Some(None);
        })
        .await?;
    let patch = json!({
        "status": RunStatus::Queued,
        "recoveryPending": true,
        "finishedAt": null,
    });
    s.store.patch_run(run_id, patch).await?;
    Ok(())
}

async fn clear_credentials(s: &Service, run_id: &str) {
    for provider in Provider::ALL {
        if let Err(error) = provider.driver().recover(s, run_id).await {
            tracing::warn!(run_id, error = %error, "Could not clear run credentials");
        }
    }
    let github = s
        .config
        .data_dir
        .join("runs")
        .join(run_id)
        .join("home/.config/gh");
    // Most runs never configured GitHub CLI credentials.
    let _ = tokio::fs::remove_dir_all(github).await;
}
