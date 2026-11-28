//! Commit an agent retry once; admission waits without retaining an account.
use super::checkpoint::Checkpoint;
use crate::{
    config::now,
    error::{Error, Result},
    run_retry::Retry,
    run_status::RunStatus,
    service::Service,
    store::Db,
};
use cairn_contracts::EventKind;
use serde_json::json;
use tokio_util::sync::CancellationToken;

pub(super) async fn schedule(
    s: &Service,
    checkpoint: &Checkpoint,
    cancel: &CancellationToken,
    error: &Error,
) -> Result<bool> {
    if cancel.is_cancelled() || s.shutdown.is_cancelled() || checkpoint.expired() {
        return Ok(false);
    }
    let saved = checkpoint.snapshot().await;
    let Some(cause) = saved.retry_cause.as_ref().and_then(Option::as_ref) else {
        return Ok(false);
    };
    if saved.completed()
        || (saved.managed()
            && saved.provider_failed()
            && !cause.startup
            && saved.confirmed_provider_failure() != Some(cause))
        || saved.finalization_error().is_some()
        || (cause.message != error.message && saved.confirmed_provider_failure() != Some(cause))
    {
        return Ok(false);
    }
    let id = checkpoint.id.clone();
    let cancel = cancel.clone();
    let shutdown = s.shutdown.clone();
    let cause = cause.clone();
    let error_message = error.message.clone();
    checkpoint
        .decide(move |current, db| {
            let run = db
                .run_state(&id)?
                .ok_or_else(|| Error::not_found("Run not found"))?;
            if cancel.is_cancelled()
                || shutdown.is_cancelled()
                || matches!(current.remaining_ms, Some(Some(i64::MIN..=0)))
                || current.completed()
                || current.finalization_error().is_some()
                || (current.managed()
                    && current.provider_failed()
                    && !cause.startup
                    && current.confirmed_provider_failure() != Some(&cause))
                || current.retry_cause.as_ref().and_then(Option::as_ref) != Some(&cause)
                || (cause.message != error_message
                    && current.confirmed_provider_failure() != Some(&cause))
                || run.cancel_requested_at.is_some()
                || (run.session_id.is_none() && !cause.startup)
                || !run.status.is_active()
            {
                return Ok(false);
            }
            schedule_in(db, &id, current, &run, cause)
        })
        .await
}

/// Commit a provider retry and its attempt-bound authorization together.
pub(super) fn schedule_in(
    db: &Db<'_>,
    id: &str,
    current: &mut super::checkpoint::RunCheckpoint,
    run: &cairn_contracts::RunState,
    cause: crate::run_retry::Cause,
) -> Result<bool> {
    let at = now();
    let Some(mut retry) = Retry::next(run.retry.as_ref(), cause, at) else {
        return Ok(false);
    };
    if let Some(not_before) = current.provider_retry_not_before
        && let Some(next) = &mut retry.next_attempt_at
    {
        *next = (*next).max(not_before);
        if retry.deadline_at.is_some_and(|deadline| *next >= deadline) {
            return Ok(false);
        }
    }
    let delay_ms = retry.next_attempt_at.unwrap().saturating_sub(at);
    let seconds = (delay_ms + 999) / 1000;
    let reason = format!(
        "Temporary agent error. Retrying in {seconds} seconds (attempt {}/{}).",
        retry.attempt, retry.limit
    );
    let patch = crate::runs::RunChanges {
        status: Some(RunStatus::Queued),
        retry: Some(Some(retry.clone())),
        recovery_pending: Some(true),
        resume_available: Some(true),
        finished_at: Some(None),
        error: Some(None),
        account_wait_reason: Some(Some(reason.clone())),
        account_required: Some(None),
        ..Default::default()
    };
    if current.managed() {
        current.provider_retry_reservation = current.runner_id().map(str::to_owned);
    }
    db.change_run(id, patch)?;
    db.event(
        id,
        &EventKind::RetryScheduled,
        &reason,
        Some(&json!({ "retry": retry })),
    )?;
    db.audit("run.retry.scheduled", &json!({ "id": id, "retry": retry }))?;
    Ok(true)
}

/// Expire a delayed recovery only after its old execution has been fenced.
pub(super) async fn expire(s: &Service, id: &str) -> Result<bool> {
    let id = id.to_owned();
    s.store.transaction(move |db| expire_in(db, &id)).await
}

/// Admission calls this again after asynchronous preparation. Both callers have
/// fenced the previous execution before entering the transaction.
pub(super) fn expire_in(db: &Db<'_>, id: &str) -> Result<bool> {
    let record = db
        .run_record(id)?
        .ok_or_else(|| Error::not_found("Run not found"))?;
    let run = record.state;
    if run.status != RunStatus::Queued
        || run.cancel_requested_at.is_some()
        || !Retry::expired(run.retry.as_ref(), now())
    {
        return Ok(false);
    }
    match crate::conversation_lifecycle::require_active_run(db, id) {
        Ok(()) => {}
        Err(error) if error.is_conflict() => return Ok(false),
        Err(error) => return Err(error),
    }
    let turn = super::checkpoint::antigravity_execution_id(&record.boundary).or(Some(id));
    let needs_input = super::checkpoint::RunCheckpoint::load_in(db, id)?
        .is_some_and(|saved| saved.antigravity_replay_refused(turn));
    let message = if needs_input {
        crate::antigravity::RETRY_UNCONFIRMED_MESSAGE
    } else if run.trigger == "chat" {
        crate::run_retry::CHAT_RECOVERY_EXPIRED
    } else {
        "Automatic recovery reached its 10-minute limit. Your conversation and files are preserved; resume when ready."
    };
    let mut retry = run.retry;
    if let Some(retry) = &mut retry {
        retry.next_attempt_at = None;
    }
    db.change_run(
        id,
        crate::runs::RunChanges {
            status: Some(RunStatus::Failed),
            finished_at: Some(Some(now())),
            error: Some(Some((message).to_owned())),
            summary: Some((message).to_owned()),
            account_wait_reason: Some(None),
            recovery_pending: Some(false),
            retry: Some(retry),
            resume_available: needs_input.then_some(false),
            ..Default::default()
        },
    )?;
    db.event(
        id,
        &EventKind::Status,
        RunStatus::Failed.as_str(),
        Some(&json!({ "error": { "message": message } })),
    )?;
    Ok(true)
}
