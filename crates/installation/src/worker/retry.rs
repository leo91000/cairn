//! Commit an agent retry once; admission waits without retaining an account.
use super::checkpoint::Checkpoint;
use crate::{
    config::now,
    error::{Error, Result},
    run_retry::Retry,
    run_status::RunStatus,
    service::Service,
};
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
    let cause = cause.clone();
    s.store
        .transaction(move |db| {
            let run = db
                .run(&id)?
                .ok_or_else(|| Error::not_found("Run not found"))?;
            if !run["cancelRequestedAt"].is_null()
                || !run["sessionId"].is_string()
                || !RunStatus::of(&run).is_some_and(RunStatus::is_active)
            {
                return Ok(false);
            }
            let previous = run.get("retry").filter(|retry| retry.is_object());
            let Some(retry) = Retry::next(previous, cause, now()) else {
                return Ok(false);
            };
            let delay_ms = retry.next_attempt_at.unwrap().saturating_sub(now());
            let seconds = (delay_ms + 999) / 1000;
            let reason = format!(
                "Temporary agent error. Retrying in {seconds} seconds (attempt {}/{}).",
                retry.attempt, retry.limit
            );
            let patch = json!({
                "status": RunStatus::Queued,
                "retry": retry,
                "recoveryPending": true,
                "resumeAvailable": true,
                "finishedAt": null,
                "error": null,
                "accountWaitReason": reason,
                "accountRequired": null,
            });
            db.patch_run(&id, &patch)?;
            db.event(
                &id,
                "retry.scheduled",
                &reason,
                Some(&json!({ "retry": retry })),
            )?;
            db.audit("run.retry.scheduled", &json!({ "id": id, "retry": retry }))?;
            Ok(true)
        })
        .await
}
