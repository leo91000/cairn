use super::{
    Admission,
    checkpoint::{Checkpoint, RunCheckpoint, Settled},
    launch,
};
use crate::{
    accounts::{Lease, output_redactions::History},
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
    let admission_revision = run["environmentAdmissionRevision"].clone();
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
    let sensitive = History::default();
    s.worker
        .job_redactions
        .lock()
        .await
        .insert(text(&run, "id").to_owned(), sensitive.clone());
    let result = launch::execute(
        s,
        &mut run,
        &mut admission,
        &cancel,
        &checkpoint,
        existing,
        &sensitive,
    )
    .await;
    if let Err(error) = result {
        record_failure_for_revision(
            s,
            &checkpoint,
            &cancel,
            &error,
            &sensitive.snapshot(),
            Some(admission_revision),
        )
        .await?;
    }
    if s.shutdown.is_cancelled() {
        // An otherwise finished relay may have lost the race to shutdown
        // after emitting the provider failure but before reconciliation.
        let _ = confirm_antigravity_refusal(s, &checkpoint, &cancel).await;
    }
    let fenced = fence(s, &checkpoint).await?;
    stop_heartbeat.cancel();
    let _ = heartbeat.await;
    #[cfg(feature = "worker-fixture-hooks")]
    await_fixture_retirement(s, checkpoint.id.as_str()).await?;
    checkpoint.persist().await?;
    release(s, &checkpoint, admission.account.as_ref(), fenced).await
}

/// The disposable worker fixture can retain the actual final checkpoint write.
/// Default production builds contain neither the socket probe nor this barrier.
#[cfg(feature = "worker-fixture-hooks")]
async fn await_fixture_retirement(s: &Service, run_id: &str) -> Result<()> {
    use tokio::io::AsyncReadExt;

    let socket = s
        .config
        .data_dir
        .join("runs")
        .join(run_id)
        .join("fixture-retirement.sock");
    if !tokio::fs::try_exists(&socket).await? {
        return Ok(());
    }
    let state = s.store.run_state(run_id).await?;
    if !matches!(state.status, RunStatus::Failed | RunStatus::Cancelled) {
        return Ok(());
    }

    let mut gate = tokio::net::UnixStream::connect(socket).await?;
    let mut released = [0];
    // Match the fixture's existing observation budget, without changing any run deadline.
    tokio::select! {
        () = s.shutdown.cancelled() => return Ok(()),
        result = tokio::time::timeout(Duration::from_secs(60), gate.read_exact(&mut released)) => {
            result.map_err(|_| Error::timeout("Fixture retirement release was not received."))??;
        },
    }
    if released != [1] {
        return Err(Error::bad("Invalid fixture retirement release."));
    }
    Ok(())
}

/// Keeps the checkpoint and node lease current while the run executes.
async fn heartbeat(s: Arc<Service>, checkpoint: Arc<Checkpoint>, stop: CancellationToken) {
    let renewal_stop = stop.clone();
    let renewal_service = s.clone();
    let renewal_checkpoint = checkpoint.clone();
    let renewal = tokio::spawn(async move {
        let mut attempt = None;
        let mut timer = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                () = renewal_stop.cancelled() => break,
                _ = timer.tick() => {
                    if let Some(saved) = renewal_checkpoint.try_snapshot() {
                        attempt = saved.runner_id().map(str::to_owned);
                    }
                    let run_id = renewal_checkpoint.id.as_str();
                    if let Some(attempt) = &attempt {
                        let _ = crate::nodes::placement::renew_preparation(&renewal_service, run_id, attempt).await;
                    }
                    let _ = crate::nodes::placement::renew_local(&renewal_service, run_id).await;
                }
            }
        }
    });
    let mut timer = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            _ = timer.tick() => {
                let _ = checkpoint.refresh_pause(&s).await;
                if checkpoint.read(|state| state.held_environment.is_some() && state.completed()).await
                    || s.worker.held.lock().await.contains(&checkpoint.id) {
                    // A queued continuation updates this same durable row. The
                    // live owner adopts it before its next provider turn.
                    continue;
                }
                if let Err(error) = checkpoint.persist().await {
                    tracing::warn!(run_id = checkpoint.id, error = %error, "Could not save the run checkpoint");
                }
            }
        }
    }
    let _ = renewal.await;
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
    if saved.confirmed_provider_failure().is_some() {
        return Ok(());
    }
    if tokio::select! {
        () = s.shutdown.cancelled() => return Ok(()),
        confirmed = confirm_antigravity_refusal(s, checkpoint, cancel) => confirmed?,
    } {
        // A provider receipt confirms the end of the turn, independently of
        // physical retirement. Admission still fences that environment.
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
        false,
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
                                    current
                                        .record_controller_observation(_db, &id, &attempt, code)?;
                                }
                            }
                            recovery::ObservationAction::PublishCleanup(result) => {
                                if !shutdown.is_cancelled() {
                                    current
                                        .publish_cleanup_observation(_db, &id, &attempt, result)?;
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

pub(super) async fn confirm_antigravity_refusal(
    s: &Service,
    checkpoint: &Checkpoint,
    cancel: &CancellationToken,
) -> Result<bool> {
    let saved = checkpoint.snapshot().await;
    let remaining_ms = checkpoint.deadline().map_or(5000, |deadline| {
        deadline.saturating_sub(now()).clamp(0, 5000)
    });
    let receipt = tokio::select! {
        () = cancel.cancelled() => return Ok(false),
        receipt = read_antigravity_refusal(s, &checkpoint.id, &saved, Duration::from_millis(remaining_ms as u64)) => receipt,
    };
    let Some(receipt) = receipt else {
        return Ok(false);
    };
    checkpoint
        .decide(move |current, _| Ok(current.record_antigravity_retry_receipt(&receipt)))
        .await
}

pub(super) async fn read_antigravity_refusal(
    s: &Service,
    run_id: &str,
    saved: &RunCheckpoint,
    budget: Duration,
) -> Option<crate::background_jobs::controller::TurnReceipt> {
    let turn = saved.antigravity_receipt_to_observe()?;
    let receipt = tokio::time::timeout(budget, super::held::turn_receipt(s, run_id, turn))
        .await
        .ok()?
        .ok()?;
    (receipt.code == 1).then_some(receipt)
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

#[cfg(test)]
async fn record_failure(
    s: &Service,
    checkpoint: &Checkpoint,
    cancel: &CancellationToken,
    error: &Error,
    sensitive: &[String],
) -> Result<()> {
    record_failure_for_revision(s, checkpoint, cancel, error, sensitive, None).await
}

async fn record_failure_for_revision(
    s: &Service,
    checkpoint: &Checkpoint,
    cancel: &CancellationToken,
    error: &Error,
    sensitive: &[String],
    admission_revision: Option<Value>,
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
    let observation_deferred = s.shutdown.is_cancelled() && saved.terminal_observation_pending();
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
            let restart_cleanup = shutdown.is_cancelled() && saved.cleanup_observation_pending();
            let expired = matches!(saved.remaining_ms, Some(Some(i64::MIN..=0))) && !restart_cleanup;
            let selection_replaced = admission_revision.as_ref().is_some_and(|revision| run.boundary["environmentAdmissionRevision"] != *revision)
                && run.boundary["environmentState"] == "expired"
                && !crate::environment_expiration::pending(db, &id)?;
            if (capacity || selection_replaced) && !cancelled && !expired && !shutdown.is_cancelled() {
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
                let detail = if selection_replaced { "The destination or backend changed during preparation. Retrying its first admission.".to_owned() } else { capacity_detail };
                return Ok(Some(detail));
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
            let turn = super::checkpoint::antigravity_execution_id(&run.boundary).or(Some(&id));
            let replay_refused = saved.antigravity_replay_refused(turn);
            let message = if cancelled && replay_refused {
                "Run stopped. Send a new message to continue."
            } else if cancelled {
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
            if replay_refused {
                patch.resume_available = Some(false);
            }
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
            Ok(None)
        })
        .await?;
    if let Some(detail) = capacity_wait {
        let run = s.store.run_state(run_id).await?;
        super::schedule::report_capacity_wait(s, &run, &detail).await?;
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
    let mode_change = run.placement.native_mode_interruption.is_some();
    let may_retry = mode_change
        || saved.controller_recoveries() < MAX_CONTROLLER_RECOVERIES
        || saved.has_controller_recovery_reservation();
    unavailable
        && saved.managed()
        && !saved.completed()
        && !saved.provider_failed()
        && may_retry
        && (mode_change || run.state.session_id.is_some())
        && !(run.state.session_id.is_none() && saved.antigravity_start_unconfirmed())
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

/// Returns whether the execution is confirmed stopped. Otherwise recovery
/// still owes the fence, with the outcome saved in the checkpoint.
async fn fence(s: &Service, checkpoint: &Checkpoint) -> Result<bool> {
    let run_id = checkpoint.id.as_str();
    let has_runner = checkpoint
        .read(|c| c.isolated() && c.runner_id().is_some())
        .await;
    if !has_runner
        || checkpoint
            .fence(s, &s.store.run(run_id).await?)
            .await
            .is_ok()
    {
        return Ok(true);
    }
    let current = s.store.run_state(run_id).await?;
    let preserves_success = current.status == RunStatus::Succeeded && current.recovery_pending;
    if current.status != RunStatus::Queued {
        let settled = Settled::from_state(&current);
        checkpoint
            .update(|c| c.settled = Some(Some(settled)))
            .await?;
    }
    let patch = crate::runs::RunChanges {
        status: (!preserves_success).then_some(RunStatus::Queued),
        recovery_pending: Some(true),
        finished_at: (!preserves_success).then_some(None),
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
    let restart_cleanup = s.shutdown.is_cancelled()
        && saved.cleanup_observation_pending()
        && saved.settled().is_none();
    if fenced
        && !restart_cleanup
        && (saved.settled().is_some() || saved.finalization_failure_attempt.is_some())
    {
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
#[path = "execution_tests.rs"]
mod tests;
