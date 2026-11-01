use super::{
    Admission,
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
use cairn_contracts::EventKind;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub(super) const MAX_CONTROLLER_RECOVERIES: u64 = 3;
const VM_STOP_WAIT_REASON: &str = "Waiting for the previous VM to stop.";
const CONTROLLER_CAPACITY_RETRY_MS: i64 = 30_000;

/// Executes a run, records how it ended and releases what it held once its
/// process is confirmed stopped.
pub(super) async fn execute(
    s: &Arc<Service>,
    mut run: Value,
    mut admission: Admission,
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
        &mut admission,
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
    release(s, &checkpoint, admission.account.as_ref(), fenced).await
}

/// Keeps the checkpoint, node lease and pending moves current while the run executes.
async fn heartbeat(s: Arc<Service>, checkpoint: Arc<Checkpoint>, stop: CancellationToken) {
    let mut timer = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            _ = timer.tick() => {
                let run_id = checkpoint.id.as_str();
                if let Some(attempt) = checkpoint.snapshot().await.runner_id() {
                    let _ = crate::nodes::placement::renew_preparation(&s, run_id, attempt).await;
                }
                // Both fail routinely once the run is stopping, moving or revoked;
                // the next beat retries and execution notices the revocation itself.
                let _ = crate::nodes::moves::pause_pending(&s, run_id).await;
                let _ = crate::nodes::placement::renew_local(&s, run_id).await;
                let _ = checkpoint.refresh_pause(&s).await;
                if let Err(error) = checkpoint.persist().await {
                    tracing::warn!(run_id, error = %error, "Could not save the run checkpoint");
                }
            }
        }
    }
}

/// Last read of the same attempt before spending a replay or settling a failure.
/// The active heartbeat retains existing leases; cancellation and time limits
/// interrupt this read without opening or transferring disk ownership.
pub(super) async fn reconcile(
    s: &Service,
    checkpoint: &Checkpoint,
    cancel: &CancellationToken,
) -> Result<()> {
    let saved = checkpoint.snapshot().await;
    if !saved.managed()
        || saved.terminal_code.is_some()
        || saved.start_rejected == Some(true)
        || checkpoint.expired()
        || cancel.is_cancelled()
        || s.shutdown.is_cancelled()
    {
        return Ok(());
    }
    let Some(attempt) = saved.runner_id() else {
        return Ok(());
    };
    let remaining_ms = checkpoint
        .deadline()
        .map_or(5000, |deadline| deadline.saturating_sub(now()).max(0));
    recovery::terminal_code(
        s,
        attempt,
        saved.node_id.as_ref().and_then(|node| node.as_deref()),
        cancel,
        Duration::from_millis(remaining_ms as u64),
        |action| {
            let attempt = attempt.to_owned();
            let shutdown = s.shutdown.clone();
            let id = checkpoint.id.clone();
            async move {
                checkpoint
                    .decide(move |current, _db| {
                        let mut reserved = false;
                        match action {
                            recovery::ObservationAction::Reserve => {
                                reserved = current.reserve_observation(&attempt);
                            }
                            recovery::ObservationAction::Release => {
                                current.release_observation(&attempt);
                            }
                            recovery::ObservationAction::Exhausted => {
                                if !shutdown.is_cancelled()
                                    && current.runner_id() == Some(attempt.as_str())
                                    && current.terminal_code.is_none()
                                    && current.settled().is_none()
                                {
                                    current.release_observation(&attempt);
                                    current.terminal_code = Some(None);
                                }
                            }
                            recovery::ObservationAction::Record(code) => {
                                current.release_observation(&attempt);
                                if current.runner_id() == Some(attempt.as_str())
                                    && current.terminal_code.is_none()
                                    && current.settled().is_none()
                                    && (!shutdown.is_cancelled() || code.is_some())
                                {
                                    current.record_controller_result(_db, &id, &attempt, code)?;
                                }
                            }
                        }
                        Ok(reserved)
                    })
                    .await
            }
        },
    )
    .await
}

#[cfg(test)]
pub(super) async fn record_failure_fixture(
    s: &Service,
    checkpoint: &Checkpoint,
    cancel: &CancellationToken,
    error: &Error,
    sensitive: &[String],
) -> Result<()> {
    record_failure(s, checkpoint, cancel, error, sensitive).await
}

async fn record_failure(
    s: &Service,
    checkpoint: &Checkpoint,
    cancel: &CancellationToken,
    error: &Error,
    sensitive: &[String],
) -> Result<()> {
    let run_id = checkpoint.id.as_str();
    reconcile(s, checkpoint, cancel).await?;
    let saved = checkpoint.snapshot().await;
    let provider_error = saved.provider_failed().then(|| {
        Error::conflict(if saved.last_error().is_empty() {
            "Agent turn failed."
        } else {
            saved.last_error()
        })
    });
    let error = provider_error.as_ref().unwrap_or(error);
    let observation_deferred =
        s.shutdown.is_cancelled() && saved.managed() && saved.terminal_code.is_none();
    if !cancel.is_cancelled()
        && !observation_deferred
        && ((saved.terminal_code() == Some(0) && !saved.provider_failed())
            || saved.finalization_error().is_some()
            || saved.output_error().is_some())
    {
        // Do not let the transport error or its exhausted replay budget hide
        // a healthy controller result, or replay a completed failed flush.
        let id = run_id.to_owned();
        checkpoint
            .decide(move |current, db| {
                let may_resume = super::schedule::finish_recovery(db, &id, Some(current))?;
                if may_resume && let Some(paid) = RunCheckpoint::load_in(db, &id)? {
                    current.controller_recoveries = paid.controller_recoveries;
                    current.controller_recovery_reservation = paid.controller_recovery_reservation;
                    current.provider_retry_reservation = paid.provider_retry_reservation;
                }
                Ok(())
            })
            .await?;
        return Ok(());
    }
    if super::retry::schedule(s, checkpoint, cancel, error).await? {
        return Ok(());
    }
    let id = run_id.to_owned();
    let summary = run_output::redact(&error.message, sensitive);
    let unavailable = error.is_unavailable();
    let capacity = crate::nodes::placement::is_no_capacity(error);
    let cancel = cancel.clone();
    let shutdown = s.shutdown.clone();
    let capacity_detail = error.message.clone();
    let capacity_wait = checkpoint
        .decide(move |saved, db| {
            let run = db
                .run_record(&id)?
                .ok_or_else(|| Error::not_found("Run not found"))?;
            let cancelled = cancel.is_cancelled() || run.state.cancel_requested_at.is_some();
            let expired = matches!(saved.remaining_ms, Some(Some(i64::MIN..=0)));
            if capacity && !cancelled && !expired && !shutdown.is_cancelled() {
                let mut restored_retry = None;
                if saved.start_rejected == Some(true)
                    && let Some(mut paid) = saved.pending_paid_admission.take()
                    && saved.runner_id() == Some(paid.attempt.as_str())
                {
                    if paid.controller && saved.controller_recoveries() > 0 {
                        saved.controller_recovery_reservation = Some(paid.attempt.clone());
                        paid.capacity_refused = true;
                    }
                    if let Some(cause) = paid.provider_cause.as_ref()
                        && let Some(mut retry) = run.state.retry.clone()
                        && &retry.cause == cause
                    {
                        if retry.next_attempt_at.is_none() {
                            retry.next_attempt_at = Some(now());
                        }
                        restored_retry = Some(retry);
                        saved.retry_cause = Some(Some(cause.clone()));
                        saved.provider_retry_reservation = Some(paid.attempt.clone());
                        paid.capacity_refused = true;
                    }
                    if paid.capacity_refused {
                        saved.pending_paid_admission = Some(paid);
                    }
                }
                saved.capacity_retry_at = Some(now() + CONTROLLER_CAPACITY_RETRY_MS);
                db.change_run(
                    &id,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        recovery_pending: Some(true),
                        finished_at: Some(None),
                        retry: restored_retry.map(Some),
                        ..Default::default()
                    },
                )?;
                return Ok(true);
            }
            let recover_controller = can_recover_controller(
                saved,
                &run,
                unavailable,
                cancelled || expired || shutdown.is_cancelled(),
            );
            if recover_controller && !saved.has_controller_recovery_reservation() {
                saved.controller_recoveries = Some(saved.controller_recoveries() + 1);
                saved.controller_recovery_reservation = saved.runner_id().map(str::to_owned);
            }
            let status = if cancelled {
                RunStatus::Cancelled
            } else if expired {
                RunStatus::Failed
            } else if shutdown.is_cancelled() || recover_controller {
                RunStatus::Queued
            } else {
                RunStatus::Failed
            };
            let message = if cancelled {
                "Run stopped. You can resume this conversation."
            } else if expired {
                "Run exceeded its time limit."
            } else {
                &summary
            };
            let needs_fence = saved.isolated() && saved.runner_id().is_some();
            if needs_fence && status != RunStatus::Queued {
                saved.settled = Some(Some(Settled::failure(status, message)));
            }
            let mut patch = failure_changes(status, needs_fence, message);
            if status != RunStatus::Queued {
                patch.wakeup = Some(None);
            }
            let kind = if shutdown.is_cancelled() {
                EventKind::Status
            } else {
                EventKind::Error
            };
            db.change_run(&id, patch)?;
            db.event(&id, &kind, message, None)?;
            Ok(false)
        })
        .await?;
    if capacity_wait {
        let run = s.store.run_state(run_id).await?;
        super::schedule::report_capacity_wait(s, &run, &capacity_detail).await?;
    }
    Ok(())
}

/// Apply the same bounded recovery policy at the durable decision point.
fn can_recover_controller(
    saved: &RunCheckpoint,
    run: &crate::runs::RunRecord,
    unavailable: bool,
    stopped: bool,
) -> bool {
    let mode_change = run.boundary["nativeModeInterruption"].is_string();
    let may_retry = mode_change
        || saved.controller_recoveries() < MAX_CONTROLLER_RECOVERIES
        || saved.has_controller_recovery_reservation()
        || run.boundary["moveRequest"].is_object();
    unavailable
        && saved.managed()
        && !saved.completed()
        && !saved.provider_failed()
        && may_retry
        && (mode_change || run.state.session_id.is_some())
        && !stopped
}

#[cfg(test)]
async fn recovers_controller(
    s: &Service,
    checkpoint: &Checkpoint,
    saved: &RunCheckpoint,
    cancel: &CancellationToken,
    error: &Error,
) -> Result<bool> {
    let run = s.store.run_record(&checkpoint.id).await?;
    Ok(can_recover_controller(
        saved,
        &run,
        error.is_unavailable(),
        checkpoint.expired() || cancel.is_cancelled() || s.shutdown.is_cancelled(),
    ))
}

/// A run whose VM may still be running stays queued for recovery; its final
/// fields are settled from the checkpoint once the VM is fenced.
fn failure_changes(status: RunStatus, needs_fence: bool, summary: &str) -> crate::runs::RunChanges {
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
    crate::runs::RunChanges {
        status: Some(status),
        recovery_pending: Some(recovery_pending),
        finished_at: Some(finished_at),
        summary: Some(summary.to_owned()),
        error: Some(error.map(str::to_owned)),
        account_wait_reason: Some(wait_reason.map(str::to_owned)),
        ..Default::default()
    }
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
    let current = s.store.run_state(run_id).await?;
    if current.status != RunStatus::Queued {
        let settled = Settled::from_state(&current);
        checkpoint
            .update(|c| c.settled = Some(Some(settled)))
            .await?;
    }
    let patch = crate::runs::RunChanges {
        status: Some(RunStatus::Queued),
        recovery_pending: Some(true),
        finished_at: Some(None),
        account_wait_reason: Some(Some(
            (if current
                .account_wait_reason
                .as_deref()
                .unwrap_or_default()
                .starts_with(super::schedule::CAPACITY_WAIT_REASON)
            {
                current.account_wait_reason.as_deref().unwrap_or_default()
            } else {
                VM_STOP_WAIT_REASON
            })
            .to_owned(),
        )),
        ..Default::default()
    };
    s.store.change_run(run_id, patch).await?;
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
    if fenced && saved.settled().is_some() {
        let id = run_id.to_owned();
        let mut expected = saved.clone();
        expected.process = None;
        s.store
            .transaction(move |db| {
                let current = RunCheckpoint::load_in(db, &id)?;
                if !recovery::same_execution(current.as_ref(), Some(&expected)) {
                    return Err(Error::conflict(
                        "Execution attempt changed before release settlement.",
                    ));
                }
                // Cancellation accepted during fencing wins in this same commit.
                super::schedule::finish_recovery(db, &id, current.as_ref()).map(|_| ())
            })
            .await?;
    }
    if fenced
        && let Some(account) = account
        && s.accounts.release(s, account).await.is_err()
    {
        let detail = json!({ "id": account.account_id, "runId": run_id });
        s.store.audit("account.release_failed", detail).await?;
    }
    s.mcps.revoke_run(s, run_id).await?;
    requeue_pending_move(s, checkpoint, fenced).await?;
    if fenced && saved.managed() {
        let run = s.store.run_record(run_id).await?;
        let native = saved
            .prepared()
            .is_some_and(|prepared| prepared["backend"] == "native");
        let status = run.state.status;
        if crate::nodes::publication::protected(&run.wire()?)
            && (native || matches!(status, RunStatus::Succeeded | RunStatus::Waiting))
        {
            // Native work can only be captured after fencing, including failed,
            // cancelled and paused turns. Durable demand survives upload errors.
            crate::nodes::publication::request(s, run_id).await?;
        }
    }
    if fenced {
        clear_credentials(s, run_id).await;
    }
    if let Some(runner) = saved.runner_id() {
        if fenced {
            crate::nodes::placement::release(s, runner).await?;
        }
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
async fn requeue_pending_move(s: &Service, checkpoint: &Checkpoint, fenced: bool) -> Result<()> {
    let run_id = checkpoint.id.as_str();
    let run = s.store.run_record(run_id).await?;
    let pending_move = run.boundary["moveRequest"].is_object();
    let transferred_reservation = run.boundary["moveReservation"].is_string();
    if (!pending_move && !transferred_reservation) || run.state.cancel_requested_at.is_some() {
        return Ok(());
    }
    let saved = checkpoint.snapshot().await;
    if saved.managed() && saved.completed() && saved.terminal_code.is_none() {
        // A shutdown-interrupted observation is still pending, not a failed
        // finalization and not permission to replay on the destination.
        return Ok(());
    }
    let mut expected_fenced = saved.clone();
    if fenced {
        expected_fenced.process = None;
    }
    if saved.managed() && saved.completed() && saved.terminal_code() == Some(0) {
        // An owner can request an idle transfer after the success commit while
        // this execution is still releasing. Leave its paid reservation for the
        // scheduler; a move requested during the active turn is not idle.
        let explicit_idle_move = run.boundary["moveRequest"]["idle"] == true
            && run.boundary["moveRequest"]["automatic"] == false
            && run.state.status == RunStatus::Queued
            && run.state.recovery_pending
            && !saved.settled().is_some_and(|settled| {
                matches!(settled.status, RunStatus::Failed | RunStatus::Cancelled)
            });
        if fenced && run.state.status != RunStatus::Waiting && !explicit_idle_move {
            crate::nodes::moves::abort_terminal_recovery(s, run_id, Some(&expected_fenced)).await?;
        }
        return Ok(());
    }
    let terminal_failure = saved.output_error().is_some()
        || saved.finalization_error().is_some()
        || saved.settled().is_some_and(|settled| {
            matches!(settled.status, RunStatus::Failed | RunStatus::Cancelled)
        });
    if terminal_failure {
        if fenced {
            crate::nodes::moves::abort_terminal_recovery(s, run_id, Some(&expected_fenced)).await?;
        }
        return Ok(());
    }
    if !pending_move {
        // A transferred reservation alone is not a request to replay this turn.
        return Ok(());
    }
    checkpoint
        .update(|c| {
            // Retain the old attempt's evidence until recovery charges admission.
            c.settled = Some(None);
        })
        .await?;
    let patch = crate::runs::RunChanges {
        status: Some(RunStatus::Queued),
        recovery_pending: Some(true),
        finished_at: Some(None),
        ..Default::default()
    };
    s.store.change_run(run_id, patch).await?;
    Ok(())
}

async fn clear_credentials(s: &Service, run_id: &str) {
    for provider in Provider::ALL {
        if let Err(error) = provider.driver().recover(s, run_id).await {
            tracing::warn!(run_id, error = %error, "Could not clear run credentials");
        }
    }
    let directory = s.config.data_dir.join("runs").join(run_id);
    crate::execution::clear_run_credentials(&directory).await;
}

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests {
    use super::*;

    async fn release_fixture() -> (tempfile::TempDir, Arc<Service>) {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        (root, service)
    }

    #[tokio::test]
    async fn release_cleans_transferred_reservations_only_after_terminal_fencing() {
        let (_root, service) = release_fixture().await;
        for (failed, fenced) in [(true, false), (true, true), (false, true)] {
            let id = crate::config::id();
            let destination = crate::config::id();
            let unrelated = crate::config::id();
            let record = json!({
                "id": id,
                "taskId": id,
                "status": "queued",
                "moveRequest": null,
                "moveReservation": destination,
                "nodeState": "resuming",
            });
            service
                .store
                .transaction(move |db| db.add_run_fixture(&record, None).map(|_| ()))
                .await
                .unwrap();
            for reservation in [&destination, &unrelated] {
                service
                    .store
                    .put(
                        "node-attempts",
                        json!({
                            "id": reservation,
                            "runId": id,
                            "nodeId": "destination",
                            "released": false,
                        }),
                    )
                    .await
                    .unwrap();
            }
            let mut saved = RunCheckpoint::fresh(None);
            if failed {
                saved.settled = Some(Some(Settled::failure(
                    RunStatus::Failed,
                    "Access was removed before preparation.",
                )));
            }
            saved.store(&service.store, &id).await.unwrap();
            let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
            release(&service, &checkpoint, None, fenced).await.unwrap();

            let should_release = failed && fenced;
            let run = service.store.run(&id).await.unwrap();
            assert_eq!(
                run["status"],
                if should_release { "failed" } else { "queued" }
            );
            assert!(
                run["moveRequest"].is_null(),
                "cleanup must not invent a move request"
            );
            assert!(!service.store.run_state(&id).await.unwrap().recovery_pending);
            assert_eq!(
                service
                    .store
                    .get("node-attempts", &destination)
                    .await
                    .unwrap()
                    .unwrap()["released"],
                should_release
            );
            assert_eq!(run["moveReservation"].is_null(), should_release);
            assert_eq!(run["nodeState"].is_null(), should_release);
            assert_eq!(
                service
                    .store
                    .get("node-attempts", &unrelated)
                    .await
                    .unwrap()
                    .unwrap()["released"],
                false
            );
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn pending_move_does_not_requeue_completed_failed_finalization() {
        let (_root, service) = release_fixture().await;
        let id = crate::config::id();
        let run = json!({ "id": id, "taskId": id, "status": "failed", "moveRequest": { "nodeId": "destination" } });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
            "completed": true, "terminalCode": 70, "remainingMs": null,
        }))
        .unwrap();
        saved.store(&service.store, &id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        release(&service, &checkpoint, None, true).await.unwrap();
        let current = checkpoint.snapshot().await;
        assert!(current.completed());
        assert_eq!(current.terminal_code(), Some(70));
        assert_eq!(
            service.store.run_state(&id).await.unwrap().status,
            RunStatus::Failed
        );
        assert!(service.store.run(&id).await.unwrap()["moveRequest"].is_null());

        service
            .store
            .change_run(
                &id,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Queued),
                    boundary: Some(json!({ "moveRequest": { "nodeId": "destination" } })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let pending = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
            "completed": true, "remainingMs": null,
        }))
        .unwrap();
        pending.store(&service.store, &id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), pending);
        release(&service, &checkpoint, None, true).await.unwrap();
        let current = checkpoint.snapshot().await;
        assert!(current.completed() && current.terminal_code.is_none());
        assert!(service.store.run(&id).await.unwrap()["moveRequest"].is_object());

        for status in [RunStatus::Succeeded, RunStatus::Waiting] {
            service
                .store
                .change_run(
                    &id,
                    crate::runs::RunChanges {
                        status: Some(status),
                        wakeup: Some((status == RunStatus::Waiting).then(|| {
                            serde_json::from_value(json!({
                                "id": "wake", "messageId": null, "reason": "Waiting", "seconds": 60,
                                "createdAt": now(), "wakeAt": now() + 60_000,
                            }))
                            .unwrap()
                        })),
                        boundary: Some(json!({ "moveRequest": { "nodeId": "destination" } })),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
                "completed": true, "terminalCode": 0, "remainingMs": null,
            }))
            .unwrap();
            saved.store(&service.store, &id).await.unwrap();
            let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
            release(&service, &checkpoint, None, true).await.unwrap();
            let current = checkpoint.snapshot().await;
            assert!(current.completed());
            assert_eq!(current.terminal_code(), Some(0));
            let run = service.store.run(&id).await.unwrap();
            assert_eq!(run["status"], status);
            assert_eq!(run["moveRequest"].is_object(), status == RunStatus::Waiting);
            if status == RunStatus::Waiting {
                assert_eq!(run["wakeup"]["id"], "wake");
            }
        }
        service
            .store
            .change_run(
                &id,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Queued),
                    recovery_pending: Some(true),
                    boundary: Some(json!({"moveRequest":null})),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let saved = RunCheckpoint {
            remaining_ms: Some(None),
            settled: Some(Some(Settled::failure(
                RunStatus::Failed,
                "previous failure",
            ))),
            ..Default::default()
        };
        saved.store(&service.store, &id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        service.worker.cancel(&service, &id).await.unwrap();
        release(&service, &checkpoint, None, true).await.unwrap();
        let state = service.store.run_state(&id).await.unwrap();
        assert_eq!(
            state.status,
            RunStatus::Cancelled,
            "accepted cancellation wins over pre-fence settlement"
        );
        assert!(!state.recovery_pending);
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn provider_failure_with_lost_terminal_result_never_replays() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "unused".into(),
            claude_bin: "unused".into(),
            opencode_bin: "unused".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        for code in ["server_error", "authentication_error"] {
            let id = crate::config::id();
            let record = json!({
                "id": id,
                "taskId": id,
                "status": "running",
                "sessionId": "session",
                "snapshot": {"agent": {"id": crate::config::MAIN_AGENT_ID, "provider": "codex"}},
            });
            service
                .store
                .transaction(move |db| db.add_run_fixture(&record, None).map(|_| ()))
                .await
                .unwrap();
            let saved = RunCheckpoint::from_value(json!({
                "prepared": {"backend": "native", "isolated": true},
                "runnerId": "same-attempt",
                "remainingMs": null,
                "terminalCode": null,
            }))
            .unwrap();
            saved.store(&service.store, &id).await.unwrap();
            let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
            let mut activity =
                crate::performance::Activity::new(&id, "fixture", "host", Provider::Codex);
            let mut total = 0;
            let event = json!({
                "type": "turn.failed",
                "error": {"code": code, "message": "Confirmed provider failure"},
            });
            super::super::output::record(
                &service,
                &super::super::output::Output {
                    kind: super::super::output::OutputKind::Stdout,
                    raw: event.to_string(),
                    received: tokio::time::Instant::now(),
                },
                &checkpoint,
                false,
                &[],
                &mut total,
                &mut activity,
            )
            .await
            .unwrap();
            record_failure(
                &service,
                &checkpoint,
                &CancellationToken::new(),
                &Error::unavailable("Controller interrupted execution"),
                &[],
            )
            .await
            .unwrap();
            let saved = checkpoint.snapshot().await;
            assert_eq!(
                saved.controller_recoveries(),
                0,
                "a confirmed provider outcome does not spend controller recoveries"
            );
            let state = service.store.run_state(&id).await.unwrap();
            assert!(state.retry.is_none());
            assert_eq!(saved.settled().unwrap().status, RunStatus::Failed);
            assert_eq!(
                saved.settled().unwrap().summary.as_deref(),
                Some("Confirmed provider failure")
            );
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn failure_settlement_rechecks_deadline_after_waiting_for_store() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let id = crate::config::id();
        let run = json!({ "id": id, "taskId": id, "status": "running" });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
            "completed": true, "terminalCode": 0, "remainingMs": 200,
        }))
        .unwrap();
        saved.store(&service.store, &id).await.unwrap();
        let checkpoint = Arc::new(Checkpoint::new(service.store.clone(), id.clone(), saved));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let store = service.store.clone();
        let blocker = tokio::spawn(async move {
            store
                .transaction(move |_| {
                    let _ = entered_tx.send(());
                    release_rx.recv().unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
        entered_rx.await.unwrap();
        let active_service = service.clone();
        let active_checkpoint = checkpoint.clone();
        let settlement = tokio::spawn(async move {
            record_failure(
                &active_service,
                &active_checkpoint,
                &CancellationToken::new(),
                &Error::unavailable("transport interrupted"),
                &[],
            )
            .await
            .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        settlement.await.unwrap();
        let state = service.store.run_state(&id).await.unwrap();
        assert_eq!(state.status, RunStatus::Failed);
        assert_eq!(state.error.as_deref(), Some("Run exceeded its time limit."));
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn generic_failure_settlement_rechecks_cancel_and_deadline_in_transaction() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        for can_recover in [false, true] {
            for cancelled in [false, true] {
                let id = crate::config::id();
                let run = json!({ "id": id, "taskId": id, "status": "running", "sessionId": if can_recover { Some("session") } else { None } });
                service
                    .store
                    .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
                    .await
                    .unwrap();
                let saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
                    "completed": false, "terminalCode": null,
                    "remainingMs": if cancelled { None } else { Some(200) },
                }))
                .unwrap();
                saved.store(&service.store, &id).await.unwrap();
                let checkpoint =
                    Arc::new(Checkpoint::new(service.store.clone(), id.clone(), saved));
                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let store = service.store.clone();
                let blocked_id = id.clone();
                let blocker = tokio::spawn(async move {
                    store
                        .transaction(move |db| {
                            let _ = entered_tx.send(());
                            release_rx.recv().unwrap();
                            if cancelled {
                                db.change_run(
                                    &blocked_id,
                                    crate::runs::RunChanges {
                                        cancel_requested_at: Some(Some(now())),
                                        ..Default::default()
                                    },
                                )?;
                            }
                            Ok(())
                        })
                        .await
                        .unwrap();
                });
                entered_rx.await.unwrap();
                let active_service = service.clone();
                let active_checkpoint = checkpoint.clone();
                let settlement = tokio::spawn(async move {
                    record_failure(
                        &active_service,
                        &active_checkpoint,
                        &CancellationToken::new(),
                        &Error::unavailable("controller unavailable"),
                        &[],
                    )
                    .await
                    .unwrap();
                });
                tokio::time::sleep(Duration::from_millis(300)).await;
                release_tx.send(()).unwrap();
                blocker.await.unwrap();
                settlement.await.unwrap();
                let persisted = RunCheckpoint::load(&service.store, &id)
                    .await
                    .unwrap()
                    .unwrap();
                let settled = persisted.settled().unwrap();
                assert_eq!(
                    settled.status,
                    if cancelled {
                        RunStatus::Cancelled
                    } else {
                        RunStatus::Failed
                    }
                );
                if !cancelled {
                    assert!(
                        settled
                            .summary
                            .as_deref()
                            .unwrap_or_default()
                            .contains("time limit")
                    );
                }
                assert!(!persisted.has_controller_recovery_reservation());
                assert_eq!(
                    service.store.run_state(&id).await.unwrap().status,
                    RunStatus::Queued
                );
            }
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn transport_failure_schedules_only_confirmed_provider_ending_for_current_attempt() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        for same in [true, false] {
            let id = crate::config::id();
            let run =
                json!({ "id": id, "taskId": id, "status": "running", "sessionId": "session" });
            service
                .store
                .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
                .await
                .unwrap();
            let cause = crate::run_retry::Cause::classify(
                &json!({ "code": "server_overloaded", "message": "Provider busy" }),
            )
            .unwrap();
            let mut saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "firecracker", "isolated": true },
                "runnerId": "current", "providerFailureAttempt": if same { "current" } else { "old" },
                "completed": false, "terminalCode": 1, "outputInterrupted": true,
                "remainingMs": null, "controllerRecoveries": 3,
            })).unwrap();
            saved.retry_cause = Some(Some(cause));
            saved.store(&service.store, &id).await.unwrap();
            let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
            record_failure(
                &service,
                &checkpoint,
                &CancellationToken::new(),
                &Error::unavailable("Transport lost"),
                &[],
            )
            .await
            .unwrap();
            let state = service.store.run_state(&id).await.unwrap();
            let saved = RunCheckpoint::load(&service.store, &id)
                .await
                .unwrap()
                .unwrap();
            if same {
                assert_eq!(state.status, RunStatus::Queued);
                assert_eq!(state.retry.unwrap().cause.message, "Provider busy");
                assert!(saved.has_provider_retry_reservation());
            } else {
                assert_eq!(state.status, RunStatus::Failed);
                assert!(state.retry.is_none());
                assert!(!saved.has_provider_retry_reservation());
            }
            assert_eq!(saved.controller_recoveries(), 3);
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn capacity_refunds_only_paid_rejected_current_attempt() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        for (count, rejected, same_attempt, cancelled, expired, paid) in [
            (3, true, true, false, false, true),
            (4, true, true, false, false, true),
            (3, false, true, false, false, true),
            (3, true, false, false, false, true),
            (3, true, true, true, false, true),
            (3, true, true, false, true, true),
            (3, true, true, false, false, false),
        ] {
            let id = crate::config::id();
            let run = json!({ "id": id, "taskId": id, "status": "running" });
            service
                .store
                .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
                .await
                .unwrap();
            let mut saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true }, "runnerId": "current",
                "completed": false, "terminalCode": null, "remainingMs": if expired { Some(0) } else { None },
                "startRejected": rejected, "controllerRecoveries": count,
            })).unwrap();
            if paid {
                saved.pending_paid_admission = Some(super::super::checkpoint::PaidAdmission {
                    attempt: if same_attempt { "current" } else { "old" }.into(),
                    controller: true,
                    capacity_refused: false,
                    provider_cause: None,
                });
            }
            saved.store(&service.store, &id).await.unwrap();
            let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
            let cancel = CancellationToken::new();
            if cancelled {
                cancel.cancel();
            }
            let error = Error::unavailable(format!(
                "{} has no capacity",
                crate::nodes::placement::NO_CAPACITY
            ));
            record_failure(&service, &checkpoint, &cancel, &error, &[])
                .await
                .unwrap();
            let durable = RunCheckpoint::load(&service.store, &id)
                .await
                .unwrap()
                .unwrap();
            let expected = rejected && same_attempt && !cancelled && !expired && paid;
            assert_eq!(
                durable.has_controller_recovery_reservation(),
                expected,
                "count={count}, rejected={rejected}, same_attempt={same_attempt}, cancelled={cancelled}, expired={expired}, paid={paid}"
            );
            assert_eq!(durable.controller_recoveries(), count);
            if expected {
                let run = service.store.run_state(&id).await.unwrap();
                assert_eq!(run.status, RunStatus::Queued);
                let key = id.clone();
                assert!(
                    service
                        .store
                        .transaction(move |db| super::super::schedule::finish_recovery(
                            db,
                            &key,
                            Some(&durable)
                        ))
                        .await
                        .unwrap()
                );
            }
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn legacy_wait_one_paid_admission_refunds_confirmed_capacity_refusal() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let captured: Value =
            serde_json::from_str(include_str!("fixtures/legacy-provider-wait-one.json")).unwrap();
        let mut retry: crate::run_retry::Retry =
            serde_json::from_value(captured["retry"].clone()).unwrap();
        let shift = now() - retry.started_at.unwrap();
        retry.started_at = retry.started_at.map(|time| time + shift);
        retry.deadline_at = retry.deadline_at.map(|time| time + shift);
        retry.next_attempt_at = retry.next_attempt_at.map(|time| time + shift);
        let id = crate::config::id();
        let run = json!({"id":id,"taskId":id,"status":"queued"});
        service
            .store
            .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
            .await
            .unwrap();
        service
            .store
            .change_run(
                &id,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Queued),
                    session_id: Some(Some("session".into())),
                    retry: Some(Some(retry.clone())),
                    recovery_pending: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(captured["checkpoint"].clone()).unwrap();
        saved.store(&service.store, &id).await.unwrap();
        let key = id.clone();
        assert!(
            service
                .store
                .transaction(move |db| super::super::schedule::finish_recovery(
                    db,
                    &key,
                    Some(&saved)
                ))
                .await
                .unwrap()
        );
        let mut admitted = RunCheckpoint::load(&service.store, &id)
            .await
            .unwrap()
            .unwrap();
        assert!(admitted.has_provider_retry_reservation());
        let state = service.store.run_state(&id).await.unwrap();
        admitted.bind_paid_admission("paid-candidate", &state);
        assert!(!admitted.has_provider_retry_reservation());
        assert_eq!(
            admitted
                .pending_paid_admission
                .as_ref()
                .unwrap()
                .provider_cause
                .as_ref(),
            Some(&retry.cause)
        );
        admitted.provider_failed = None;
        admitted.start_rejected = Some(true);
        admitted.terminal_code = None;
        admitted.store(&service.store, &id).await.unwrap();
        let mut consumed = retry.clone();
        consumed.next_attempt_at = None;
        service
            .store
            .change_run(
                &id,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Running),
                    retry: Some(Some(consumed)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), admitted);
        let error = Error::unavailable(format!(
            "{} has no capacity",
            crate::nodes::placement::NO_CAPACITY
        ));
        record_failure(
            &service,
            &checkpoint,
            &CancellationToken::new(),
            &error,
            &[],
        )
        .await
        .unwrap();
        let after = RunCheckpoint::load(&service.store, &id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            after.provider_retry_reservation.as_deref(),
            Some("paid-candidate")
        );
        let restored = service.store.run_state(&id).await.unwrap().retry.unwrap();
        assert_eq!(restored.attempt, retry.attempt);
        assert_eq!(restored.started_at, retry.started_at);
        assert_eq!(restored.deadline_at, retry.deadline_at);
        assert_eq!(restored.cause, retry.cause);
        assert!(restored.next_attempt_at.is_some());
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn provider_capacity_refund_requires_same_attempt_and_retry_cause() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let cause = crate::run_retry::Cause::classify(
            &json!({ "code": "server_overloaded", "message": "busy" }),
        )
        .unwrap();
        for (same_attempt, same_cause, rejected, cancelled, expired) in [
            (true, true, true, false, false),
            (false, true, true, false, false),
            (true, false, true, false, false),
            (true, true, false, false, false),
            (true, true, true, true, false),
            (true, true, true, false, true),
        ] {
            let id = crate::config::id();
            let run = json!({ "id": id, "taskId": id, "status": "running" });
            service
                .store
                .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
                .await
                .unwrap();
            let retry_cause = if same_cause {
                cause.clone()
            } else {
                crate::run_retry::Cause::classify(
                    &json!({ "code": "request_timeout", "message": "different" }),
                )
                .unwrap()
            };
            let mut retry = crate::run_retry::Retry::next(None, retry_cause, now()).unwrap();
            retry.next_attempt_at = None;
            service
                .store
                .change_run(
                    &id,
                    crate::runs::RunChanges {
                        retry: Some(Some(retry)),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let mut saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "firecracker", "isolated": true }, "runnerId": "current",
                "completed": false, "terminalCode": null, "remainingMs": if expired { Some(0) } else { None },
                "startRejected": rejected, "controllerRecoveries": 3,
            })).unwrap();
            saved.pending_paid_admission = Some(super::super::checkpoint::PaidAdmission {
                attempt: if same_attempt { "current" } else { "old" }.into(),
                controller: false,
                capacity_refused: false,
                provider_cause: Some(cause.clone()),
            });
            saved.store(&service.store, &id).await.unwrap();
            let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
            let cancel = CancellationToken::new();
            if cancelled {
                cancel.cancel();
            }
            let error = Error::unavailable(format!(
                "{} has no capacity",
                crate::nodes::placement::NO_CAPACITY
            ));
            record_failure(&service, &checkpoint, &cancel, &error, &[])
                .await
                .unwrap();
            let durable = RunCheckpoint::load(&service.store, &id)
                .await
                .unwrap()
                .unwrap();
            let expected = same_attempt && same_cause && rejected && !cancelled && !expired;
            assert_eq!(durable.has_provider_retry_reservation(), expected);
            assert!(!durable.has_controller_recovery_reservation());
            assert_eq!(durable.controller_recoveries(), 3);
            if expected {
                assert!(
                    service
                        .store
                        .run_state(&id)
                        .await
                        .unwrap()
                        .retry
                        .unwrap()
                        .next_attempt_at
                        .is_some()
                );
                let key = id.clone();
                assert!(
                    service
                        .store
                        .transaction(move |db| super::super::schedule::finish_recovery(
                            db,
                            &key,
                            Some(&durable)
                        ))
                        .await
                        .unwrap()
                );
            }
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn completed_worker_release_keeps_explicit_idle_move_and_reservation() {
        use crate::{agent_fixture::StoreAgentFixture, node_fixture::StoreNodeFixture};
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let (id, agent, source, destination, attempt) = (
            crate::config::id(),
            crate::config::id(),
            crate::config::id(),
            crate::config::id(),
            crate::config::id(),
        );
        for node in [&source, &destination] {
            service.store.put_node_fixture(json!({
                "id": node, "accepting": true, "executionReady": true, "runtimeId": "development",
                "lastSeen": now(), "capabilities": { "kvm": true },
                "limits": { "cpu": 2, "memoryMiB": 1024, "diskMiB": 1024 },
            })).await.unwrap();
        }
        let profile = service.store.put_agent_fixture(json!({
            "id": agent, "provider": "codex", "access": { "backend": "firecracker", "nodes": [source, destination] }
        })).await.unwrap();
        let record = json!({
            "id": id, "taskId": id, "status": "succeeded", "trigger": "chat",
            "sessionId": "retained-session", "summary": "committed answer", "isolated": true,
            "nodeId": source, "resources": { "cpu": 1, "memoryMiB": 512, "diskMiB": 128 },
            "snapshot": { "agent": profile }, "nativeProvenanceUnknown": true,
        });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&record, None).map(|_| ()))
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "firecracker", "isolated": true }, "nodeId": source,
            "runnerId": attempt, "completed": true, "terminalCode": 0, "remainingMs": null,
        }))
        .unwrap();
        saved.store(&service.store, &id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        crate::nodes::moves::request(
            &service,
            &service.store.run(&id).await.unwrap(),
            &json!({"nodeId": destination}),
        )
        .await
        .unwrap();
        let pending = service.store.run(&id).await.unwrap();
        assert_eq!(pending["moveRequest"]["idle"], true);
        let reservation = pending["moveRequest"]["reservation"]
            .as_str()
            .unwrap()
            .to_owned();
        release(&service, &checkpoint, None, true).await.unwrap();
        let after = service.store.run(&id).await.unwrap();
        assert_eq!(after["moveRequest"]["reservation"], reservation);
        assert_eq!(after["status"], RunStatus::Queued);
        assert_eq!(after["recoveryPending"], true);
        assert_eq!(after["summary"], "committed answer");
        assert_eq!(
            service
                .store
                .get("node-attempts", &reservation)
                .await
                .unwrap()
                .unwrap()["released"],
            false
        );
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn pending_move_does_not_requeue_completed_failed_finalization_reconciliation() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let id = crate::config::id();
        let run = json!({ "id": id, "taskId": id, "status": "failed", "moveRequest": { "nodeId": "destination" } });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
            "completed": true, "terminalCode": 70, "remainingMs": null,
        }))
        .unwrap();
        saved.store(&service.store, &id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
        release(&service, &checkpoint, None, true).await.unwrap();
        let current = checkpoint.snapshot().await;
        assert!(current.completed());
        assert_eq!(current.terminal_code(), Some(70));
        assert_eq!(
            service.store.run_state(&id).await.unwrap().status,
            RunStatus::Failed
        );
        assert!(service.store.run(&id).await.unwrap()["moveRequest"].is_null());

        service
            .store
            .change_run(
                &id,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Queued),
                    boundary: Some(json!({ "moveRequest": { "nodeId": "destination" } })),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let pending = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
            "completed": true, "remainingMs": null,
        }))
        .unwrap();
        pending.store(&service.store, &id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), pending);
        release(&service, &checkpoint, None, true).await.unwrap();
        let current = checkpoint.snapshot().await;
        assert!(current.completed() && current.terminal_code.is_none());
        assert!(service.store.run(&id).await.unwrap()["moveRequest"].is_object());

        for status in [RunStatus::Succeeded, RunStatus::Waiting] {
            service
                .store
                .change_run(
                    &id,
                    crate::runs::RunChanges {
                        status: Some(status),
                        wakeup: Some((status == RunStatus::Waiting).then(|| {
                            serde_json::from_value(json!({
                                "id": "wake", "messageId": null, "reason": "Waiting", "seconds": 60,
                                "createdAt": now(), "wakeAt": now() + 60_000,
                            }))
                            .unwrap()
                        })),
                        boundary: Some(json!({ "moveRequest": { "nodeId": "destination" } })),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
                "completed": true, "terminalCode": 0, "remainingMs": null,
            }))
            .unwrap();
            saved.store(&service.store, &id).await.unwrap();
            let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
            release(&service, &checkpoint, None, true).await.unwrap();
            let current = checkpoint.snapshot().await;
            assert!(current.completed());
            assert_eq!(current.terminal_code(), Some(0));
            let run = service.store.run(&id).await.unwrap();
            assert_eq!(run["status"], status);
            assert_eq!(run["moveRequest"].is_object(), status == RunStatus::Waiting);
            if status == RunStatus::Waiting {
                assert_eq!(run["wakeup"]["id"], "wake");
            }
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn failure_settlement_rechecks_deadline_after_waiting_for_store_reconciliation() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let id = crate::config::id();
        let run = json!({ "id": id, "taskId": id, "status": "running" });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
            "completed": true, "terminalCode": 0, "remainingMs": 200,
        }))
        .unwrap();
        saved.store(&service.store, &id).await.unwrap();
        let checkpoint = Arc::new(Checkpoint::new(service.store.clone(), id.clone(), saved));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let store = service.store.clone();
        let blocker = tokio::spawn(async move {
            store
                .transaction(move |_| {
                    let _ = entered_tx.send(());
                    release_rx.recv().unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
        entered_rx.await.unwrap();
        let active_service = service.clone();
        let active_checkpoint = checkpoint.clone();
        let settlement = tokio::spawn(async move {
            record_failure(
                &active_service,
                &active_checkpoint,
                &CancellationToken::new(),
                &Error::unavailable("transport interrupted"),
                &[],
            )
            .await
            .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        settlement.await.unwrap();
        let state = service.store.run_state(&id).await.unwrap();
        assert_eq!(state.status, RunStatus::Failed);
        assert_eq!(state.error.as_deref(), Some("Run exceeded its time limit."));

        let resumed_id = crate::config::id();
        let run = json!({ "id": resumed_id, "taskId": resumed_id, "status": "running" });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
            "completed": true, "terminalCode": 0, "remainingMs": 60_000,
        }))
        .unwrap();
        saved.store(&service.store, &resumed_id).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), resumed_id.clone(), saved);
        checkpoint.account_pause("attempt", 30_000);
        record_failure(
            &service,
            &checkpoint,
            &CancellationToken::new(),
            &Error::unavailable("transport interrupted"),
            &[],
        )
        .await
        .unwrap();
        let persisted = RunCheckpoint::load(&service.store, &resumed_id)
            .await
            .unwrap()
            .unwrap();
        let observation = persisted.administrative_pause_observation.unwrap();
        assert_eq!(
            (observation.attempt.as_str(), observation.total_ms),
            ("attempt", 30_000)
        );
        assert!(persisted.remaining_ms.flatten().unwrap() > 89_000);
        assert_eq!(
            service.store.run_state(&resumed_id).await.unwrap().status,
            RunStatus::Succeeded
        );
        // If the durable attempt loses its terminal evidence while settlement
        // waits, finish_recovery must retain its newly paid reservation.
        for existing_reservation in [false, true] {
            let retry_id = crate::config::id();
            let run = json!({ "id": retry_id, "taskId": retry_id, "status": "running" });
            service
                .store
                .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
                .await
                .unwrap();
            let saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
                "completed": true, "terminalCode": 0, "remainingMs": 60_000,
            }))
            .unwrap();
            saved.store(&service.store, &retry_id).await.unwrap();
            let checkpoint = Arc::new(Checkpoint::new(
                service.store.clone(),
                retry_id.clone(),
                saved,
            ));
            checkpoint.account_pause("attempt", 30_000);
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let (store, changed_id) = (service.store.clone(), retry_id.clone());
            let blocker = tokio::spawn(async move {
                store
                    .transaction(move |db| {
                        let _ = entered_tx.send(());
                        release_rx.recv().unwrap();
                        let mut current = RunCheckpoint::load_in(db, &changed_id)?.unwrap();
                        current.completed = Some(false);
                        current.terminal_code = Some(None);
                        if existing_reservation {
                            current.controller_recoveries = Some(1);
                            current.controller_recovery_reservation = Some("attempt".into());
                        }
                        current.store_in(db, &changed_id)
                    })
                    .await
                    .unwrap();
            });
            entered_rx.await.unwrap();
            let (running_service, running_checkpoint) = (service.clone(), checkpoint.clone());
            let settlement = tokio::spawn(async move {
                record_failure(
                    &running_service,
                    &running_checkpoint,
                    &CancellationToken::new(),
                    &Error::unavailable("transport interrupted"),
                    &[],
                )
                .await
                .unwrap();
            });
            tokio::time::sleep(Duration::from_millis(100)).await;
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            settlement.await.unwrap();
            let paid = RunCheckpoint::load(&service.store, &retry_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(paid.controller_recoveries(), 1);
            assert!(paid.has_controller_recovery_reservation());
            assert_eq!(
                paid.administrative_pause_observation.unwrap().total_ms,
                30_000
            );
            assert!(paid.remaining_ms.flatten().unwrap() > 89_000);
        }

        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn generic_failure_settlement_rechecks_cancel_and_deadline_in_transaction_reconciliation()
    {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        for can_recover in [false, true] {
            for cancelled in [false, true] {
                let id = crate::config::id();
                let run = json!({ "id": id, "taskId": id, "status": "running", "sessionId": if can_recover { Some("session") } else { None } });
                service
                    .store
                    .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
                    .await
                    .unwrap();
                let saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "native", "isolated": true }, "runnerId": "attempt",
                    "completed": false, "terminalCode": null,
                    "remainingMs": if cancelled { None } else { Some(200) },
                }))
                .unwrap();
                saved.store(&service.store, &id).await.unwrap();
                let checkpoint =
                    Arc::new(Checkpoint::new(service.store.clone(), id.clone(), saved));
                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let store = service.store.clone();
                let blocked_id = id.clone();
                let blocker = tokio::spawn(async move {
                    store
                        .transaction(move |db| {
                            let _ = entered_tx.send(());
                            release_rx.recv().unwrap();
                            if cancelled {
                                db.change_run(
                                    &blocked_id,
                                    crate::runs::RunChanges {
                                        cancel_requested_at: Some(Some(now())),
                                        ..Default::default()
                                    },
                                )?;
                            }
                            Ok(())
                        })
                        .await
                        .unwrap();
                });
                entered_rx.await.unwrap();
                let active_service = service.clone();
                let active_checkpoint = checkpoint.clone();
                let settlement = tokio::spawn(async move {
                    record_failure(
                        &active_service,
                        &active_checkpoint,
                        &CancellationToken::new(),
                        &Error::unavailable("controller unavailable"),
                        &[],
                    )
                    .await
                    .unwrap();
                });
                tokio::time::sleep(Duration::from_millis(300)).await;
                release_tx.send(()).unwrap();
                blocker.await.unwrap();
                settlement.await.unwrap();
                let persisted = RunCheckpoint::load(&service.store, &id)
                    .await
                    .unwrap()
                    .unwrap();
                let settled = persisted.settled().unwrap();
                assert_eq!(
                    settled.status,
                    if cancelled {
                        RunStatus::Cancelled
                    } else {
                        RunStatus::Failed
                    }
                );
                if !cancelled {
                    assert!(
                        settled
                            .summary
                            .as_deref()
                            .unwrap_or_default()
                            .contains("time limit")
                    );
                }
                assert!(!persisted.has_controller_recovery_reservation());
                assert_eq!(
                    service.store.run_state(&id).await.unwrap().status,
                    RunStatus::Queued
                );
            }
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn remote_preparation_failures_stop_after_three_recoveries() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let run = crate::config::id();
        service
            .store
            .transaction({
                let run = run.clone();
                move |db| {
                    let record = json!({
                        "id": run,
                        "taskId": run,
                        "status": "running",
                        "sessionId": "saved-session",
                    });
                    db.add_run_fixture(&record, None)?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        let checkpoint = Checkpoint::new(
            service.store.clone(),
            run.clone(),
            RunCheckpoint::from_value(json!({
                "prepared": { "backend": "firecracker", "isolated": true },
                "nodeId": crate::config::id(),
                "runnerId": crate::config::id(),
                "remainingMs": null,
                "controllerRecoveries": MAX_CONTROLLER_RECOVERIES - 1,
            }))
            .unwrap(),
        );
        checkpoint.persist().await.unwrap();
        let cancel = CancellationToken::new();
        let error =
            Error::unavailable("Remote workspace preparation failed; source files are preserved.");
        record_failure(&service, &checkpoint, &cancel, &error, &[])
            .await
            .unwrap();
        assert_eq!(
            checkpoint.snapshot().await.controller_recoveries(),
            MAX_CONTROLLER_RECOVERIES
        );
        assert!(checkpoint.snapshot().await.settled().is_none());
        // The paid recovery is consumed by the replacement attempt. Its own
        // failure must settle at the ceiling even when a session exists.
        checkpoint
            .update(|saved| {
                saved.runner_id = Some(Some(crate::config::id()));
                saved.controller_recovery_reservation = None;
            })
            .await
            .unwrap();
        record_failure(&service, &checkpoint, &cancel, &error, &[])
            .await
            .unwrap();
        assert_eq!(
            checkpoint
                .snapshot()
                .await
                .settled()
                .unwrap()
                .run_changes()
                .status,
            Some(RunStatus::Failed)
        );
        assert_eq!(
            checkpoint.snapshot().await.controller_recoveries(),
            MAX_CONTROLLER_RECOVERIES
        );
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn native_mode_recovery_is_paid_and_admitted_after_fencing() {
        use axum::{Router, routing::any};
        let app = Router::new().fallback(any(|| async { axum::http::StatusCode::NO_CONTENT }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: format!("http://{address}"),
        })
        .await
        .unwrap();
        for (count, forbidden, cancelled) in [
            (3, false, false),
            (4, false, false),
            (3, true, false),
            (3, false, true),
        ] {
            let id = crate::config::id();
            let run = json!({ "id": id, "taskId": id, "status": "running", "createdAt": now(),
                "nativeModeInterruption": crate::config::id(),
                "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID, "provider": "codex" } } });
            service
                .store
                .transaction(move |db| db.add_run_fixture(&run, None).map(|_| ()))
                .await
                .unwrap();
            let saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true }, "runnerId": crate::config::id(),
                "completed": false, "terminalCode": null, "remainingMs": null,
                "controllerRecoveries": count,
            })).unwrap();
            saved.store(&service.store, &id).await.unwrap();
            let checkpoint = Checkpoint::new(service.store.clone(), id.clone(), saved);
            let cancel = CancellationToken::new();
            if cancelled {
                cancel.cancel();
            }
            let error = if forbidden {
                Error::forbidden("Native admission revoked")
            } else {
                Error::unavailable("Controlled native stop")
            };
            record_failure(&service, &checkpoint, &cancel, &error, &[])
                .await
                .unwrap();
            let reserved = RunCheckpoint::load(&service.store, &id)
                .await
                .unwrap()
                .unwrap();
            if forbidden || cancelled {
                assert!(!reserved.has_controller_recovery_reservation());
                assert!(reserved.settled().is_some());
            } else {
                assert_eq!(reserved.controller_recoveries(), count + 1);
                assert!(reserved.has_controller_recovery_reservation());
            }
            // Fencing uses only the disposable controller; tick then settles the
            // durable reservation before any new placement can be admitted.
            service.worker.tick(&service).await.unwrap();
            let current = service.store.run_state(&id).await.unwrap();
            if forbidden || cancelled {
                assert_eq!(
                    current.status,
                    if cancelled {
                        RunStatus::Cancelled
                    } else {
                        RunStatus::Failed
                    }
                );
            } else {
                assert_eq!(current.status, RunStatus::Queued);
                assert!(!current.recovery_pending);
                let saved = RunCheckpoint::load(&service.store, &id)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(saved.has_controller_recovery_reservation());
                assert_eq!(saved.controller_recoveries(), count + 1);
            }
        }
        service.shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn native_mode_recovery_retries_pre_session_interruptions_but_not_revocations() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let run = crate::config::id();
        let record = json!({
            "id": run, "taskId": crate::config::id(), "status": "running", "createdAt": now(),
            "nativeModeInterruption": crate::config::id(),
        });
        service
            .store
            .transaction(move |db| db.add_run_fixture(&record, None).map(|_| ()))
            .await
            .unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "nodeId": crate::config::id(), "runnerId": crate::config::id(),
            "controllerRecoveries": MAX_CONTROLLER_RECOVERIES + 1, "remainingMs": null,
        }))
        .unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), run, saved.clone());
        let cancel = CancellationToken::new();
        assert!(
            recovers_controller(
                &service,
                &checkpoint,
                &saved,
                &cancel,
                &Error::unavailable("Controlled native stop")
            )
            .await
            .unwrap()
        );
        for error in [
            Error::forbidden("Native admission revoked"),
            Error::conflict("Saved session unavailable"),
        ] {
            assert!(
                !recovers_controller(&service, &checkpoint, &saved, &cancel, &error)
                    .await
                    .unwrap()
            );
        }
        cancel.cancel();
        assert!(
            !recovers_controller(
                &service,
                &checkpoint,
                &saved,
                &cancel,
                &Error::unavailable("Controlled native stop")
            )
            .await
            .unwrap()
        );
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn stopped_native_turns_retain_final_publication_demand_for_every_outcome() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        for (backend, status, fenced, expected) in [
            ("native", "succeeded", true, true),
            ("native", "failed", true, true),
            ("native", "cancelled", true, true),
            ("native", "queued", true, true),
            ("firecracker", "waiting", true, true),
            ("native", "failed", false, false),
            ("firecracker", "failed", true, false),
            ("firecracker", "cancelled", true, false),
        ] {
            let run = crate::config::id();
            service
                .store
                .transaction({
                    let run = run.clone();
                    move |db| {
                        let record = json!({
                            "id": run,
                            "taskId": run,
                            "status": status,
                            "isolated": true,
                            "storage": { "mode": "on-demand" },
                        });
                        db.add_run_fixture(&record, None)?;
                        Ok(())
                    }
                })
                .await
                .unwrap();
            let checkpoint = Checkpoint::new(
                service.store.clone(),
                run.clone(),
                RunCheckpoint::from_value(json!({
                    "prepared": { "backend": backend, "isolated": true },
                    "runnerId": crate::config::id(),
                }))
                .unwrap(),
            );
            checkpoint.persist().await.unwrap();
            release(&service, &checkpoint, None, fenced).await.unwrap();
            let current = service.store.run(&run).await.unwrap();
            assert_eq!(
                crate::nodes::publication::requested(&current),
                expected,
                "{backend}/{status}/fenced={fenced}: {current}",
            );
            if expected {
                // Every current disk retains demand for the publication scheduler.
                assert_eq!(current["backup"]["requestedRevision"], 1);
                assert_eq!(current["backup"]["status"], "pending");
            } else {
                assert!(current["backup"].is_null());
            }
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn completed_turn_releases_admission_while_publication_is_busy() {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let run = crate::config::id();
        service.store.transaction({
            let run = run.clone();
            move |db| {
                let record = json!({ "id": run, "taskId": run, "status": "succeeded", "isolated": true });
                db.add_run_fixture(&record, None)?;
                Ok(())
            }
        }).await.unwrap();
        let checkpoint = Checkpoint::new(service.store.clone(), run.clone(), RunCheckpoint::from_value(json!({
            "prepared": { "backend": "firecracker", "isolated": true }, "runnerId": crate::config::id(),
        })).unwrap());
        // A real publication holds this operation across capture, every S3 PUT,
        // manifest verification and the node's acknowledgement.
        let publication = service.node_backup_operation.lock(&run).await;
        // This is a deadlock watchdog, not a storage-latency SLA: the publication
        // guard stays held until after release and all durable-demand assertions.
        tokio::time::timeout(
            Duration::from_secs(5),
            release(&service, &checkpoint, None, true),
        )
        .await
        .expect("A completed turn must release its scheduler slot before S3 acknowledgement")
        .unwrap();
        let current = service.store.run(&run).await.unwrap();
        assert_eq!(current["backup"]["status"], "pending");
        assert_eq!(current["backup"]["requestedRevision"], 1);
        // Repeated completed turns coalesce to one durable demand rather than
        // enqueueing one upload task for each response.
        release(&service, &checkpoint, None, true).await.unwrap();
        assert_eq!(
            service.store.run(&run).await.unwrap()["backup"]["requestedRevision"],
            2
        );
        drop(publication);
        service.shutdown.cancel();
    }
}
