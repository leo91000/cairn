//! Commit an agent retry once; admission waits without retaining an account.
use super::checkpoint::{Checkpoint, RunCheckpoint};
use crate::{
    config::now,
    error::{Error, Result},
    run_retry::{Cause, Retry},
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
    if saved.completed() || cause.message != error.message {
        return Ok(false);
    }
    let id = checkpoint.id.clone();
    let expected = saved.clone();
    let deadline = checkpoint.deadline();
    let cancel = cancel.clone();
    let shutdown = s.shutdown.clone();
    let cause = cause.clone();
    s.store
        .transaction(move |db| {
            let current = RunCheckpoint::load_in(db, &id)?;
            if cancel.is_cancelled()
                || shutdown.is_cancelled()
                || deadline.is_some_and(|deadline| now() >= deadline)
                || current.as_ref().map_or(
                    expected.runner_id().is_some() || expected.process().is_some(),
                    |current| !crate::recovery::same_execution(Some(current), Some(&expected)),
                )
            {
                return Ok(false);
            }
            schedule_in(db, &id, cause)
        })
        .await
}

/// Active failure and restart settlement commit the same provider retry policy.
pub(super) fn schedule_in(db: &Db<'_>, id: &str, cause: Cause) -> Result<bool> {
    let run = db
        .run_state(id)?
        .ok_or_else(|| Error::not_found("Run not found"))?;
    if run.cancel_requested_at.is_some()
        || (run.session_id.is_none() && !cause.startup)
        || !run.status.is_active()
    {
        return Ok(false);
    }
    let Some(retry) = Retry::next(run.retry.as_ref(), cause, now()) else {
        return Ok(false);
    };
    let delay_ms = retry.next_attempt_at.unwrap().saturating_sub(now());
    let seconds = (delay_ms + 999) / 1000;
    let reason = format!(
        "Temporary agent error. Retrying in {seconds} seconds (attempt {}/{}).",
        retry.attempt, retry.limit
    );
    db.change_run(
        id,
        crate::runs::RunChanges {
            status: Some(RunStatus::Queued),
            retry: Some(Some(retry.clone())),
            recovery_pending: Some(true),
            resume_available: Some(true),
            finished_at: Some(None),
            error: Some(None),
            account_wait_reason: Some(Some(reason.clone())),
            account_required: Some(None),
            ..Default::default()
        },
    )?;
    db.event(
        id,
        &EventKind::RetryScheduled,
        &reason,
        Some(&json!({"retry":retry})),
    )?;
    db.audit("run.retry.scheduled", &json!({"id":id,"retry":retry}))?;
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
    let run = db
        .run_state(id)?
        .ok_or_else(|| Error::not_found("Run not found"))?;
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
    crate::nodes::moves::release_terminal_destination_in(db, id)?;
    let message = "Automatic recovery reached its 10-minute limit. Your conversation and files are preserved; resume when ready.";
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
            ..Default::default()
        },
    )?;
    db.event(id, &EventKind::Status, RunStatus::Failed.as_str(), None)?;
    Ok(true)
}
