//! A live environment can outlast its provider turn. Its original execution
//! future remains the sole owner of cleanup, cancellation and the node lease.
use futures_util::TryStreamExt;
use std::collections::HashSet;

tokio::task_local! {
    /// Stable JSON-RPC identity scoped to the authenticated request.
    pub(crate) static CALL_ID: serde_json::Value;
}

use super::{Worker, checkpoint::RunCheckpoint};
use crate::{config::now, store::Db, validation::text};

/// Holding an environment is explicit and bounded; an ordinary completed run
/// must never renew a lease merely because its old checkpoint still exists.
pub(crate) fn authorized(
    db: &Db<'_>,
    run: &serde_json::Value,
    attempt: &str,
) -> crate::error::Result<bool> {
    let id = text(run, "id");
    let Some(saved) = RunCheckpoint::load_in(db, id)? else {
        return Ok(false);
    };
    if crate::conversation_lifecycle::require_active_run(db, id).is_err() {
        return Ok(false);
    }
    Ok(saved.held_environment.as_ref().is_some_and(|held| {
        held.attempt == attempt
            && held.expires_at > now()
            && saved.runner_id() == Some(attempt)
            && run["cancelRequestedAt"].is_null()
            && matches!(
                run["status"].as_str(),
                Some("succeeded" | "waiting" | "queued" | "running")
            )
    }))
}

impl Worker {
    pub(super) async fn provider_active(&self, active: &HashSet<String>) -> HashSet<String> {
        let held = self.held.lock().await;
        active
            .iter()
            .filter(|id| !held.contains(*id))
            .cloned()
            .collect()
    }
}

/// Minimal controller capability needed before emitting a multi-turn plan.
pub(crate) async fn supported(
    s: &crate::service::Service,
    run: &serde_json::Value,
) -> crate::error::Result<bool> {
    let url = crate::nodes::transport::url(s, text(run, "id")).await?;
    let response = s
        .http
        .get(format!("{url}/health"))
        .bearer_auth(crate::execution::secret(&s.config.data_dir, "runner-secret").await?)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;
    let Ok(response) = response else {
        return Ok(false);
    };
    if !response.status().is_success() {
        return Ok(false);
    }
    let value: serde_json::Value = response.json().await.unwrap_or_default();
    Ok(value["environmentProtocol"] == 1)
}

/// The controller owns each admission receipt; a late manager response must
/// never be routed through a newly selected attempt.
fn require_attempt(saved: &RunCheckpoint, expected: Option<&str>) -> crate::error::Result<()> {
    if expected.is_some_and(|expected| saved.runner_id() != Some(expected)) {
        return Err(crate::error::Error::conflict(
            "The job's environment was replaced.",
        ));
    }
    Ok(())
}

/// Calls only the already admitted controller. This path never prepares a disk
/// or launches a replacement environment after an ambiguous response.
pub(crate) async fn request(
    s: &crate::service::Service,
    run: &serde_json::Value,
    request: &crate::background_jobs::protocol::Request,
) -> crate::error::Result<crate::background_jobs::protocol::Event> {
    request_bound(s, run, None, request).await
}

pub(crate) async fn request_bound(
    s: &crate::service::Service,
    run: &serde_json::Value,
    expected_attempt: Option<&str>,
    request: &crate::background_jobs::protocol::Request,
) -> crate::error::Result<crate::background_jobs::protocol::Event> {
    use crate::error::Error;
    let id = text(run, "id");
    let saved = RunCheckpoint::load(&s.store, id)
        .await?
        .ok_or_else(|| Error::conflict("No active environment."))?;
    let attempt = saved
        .runner_id()
        .ok_or_else(|| Error::conflict("No active environment."))?;
    require_attempt(&saved, expected_attempt)?;
    let url = crate::nodes::transport::url_for_node(
        s,
        saved.node_id.as_ref().and_then(|value| value.as_deref()),
    );
    let response = s
        .http
        .post(format!("{url}/runs/{attempt}/environment"))
        .bearer_auth(crate::execution::secret(&s.config.data_dir, "runner-secret").await?)
        .json(request)
        .timeout(std::time::Duration::from_secs(
            if matches!(
                request,
                crate::background_jobs::protocol::Request::ResumeTurn { .. }
            ) {
                60
            } else {
                12
            },
        ))
        .send()
        .await
        .map_err(|_| {
            Error::unavailable("Environment request outcome is unknown; do not restart this job.")
        })?;
    if !response.status().is_success() {
        return Err(Error::unavailable(
            "Execution environment rejected the request.",
        ));
    }
    let bytes = crate::process::read_bounded(
        tokio_util::io::StreamReader::new(response.bytes_stream().map_err(std::io::Error::other)),
        256 * 1024,
    )
    .await?;
    serde_json::from_slice(&bytes).map_err(Error::from)
}

pub(crate) async fn start_job(
    s: &crate::service::Service,
    bearer: &str,
    args: &serde_json::Value,
) -> crate::error::Result<serde_json::Value> {
    use crate::{
        background_jobs::protocol::{Event, Request},
        error::Error,
    };
    let run = crate::project_workspaces::authorize(s, bearer).await?;
    if crate::chats::planning(&run)
        || crate::execution::Sandbox::is_read_only(
            &crate::service::policy(&run["snapshot"]["agent"])["sandbox"],
        )
    {
        return Err(Error::forbidden(
            "Background jobs require a writable execution turn.",
        ));
    }
    let checkpoint = RunCheckpoint::load(&s.store, text(&run, "id"))
        .await?
        .ok_or_else(|| Error::conflict("No active environment."))?;
    let attempt = checkpoint
        .runner_id()
        .ok_or_else(|| Error::conflict("No active environment."))?;
    let plan: serde_json::Value = serde_json::from_slice(
        &tokio::fs::read(
            s.config
                .data_dir
                .join("runner-plans")
                .join(format!("{attempt}.json")),
        )
        .await?,
    )?;
    if plan["environmentProtocol"] != 1 {
        return Err(Error::conflict(
            "This environment predates managed background jobs. Start a new environment to use start_job.",
        ));
    }
    let call = CALL_ID
        .try_with(Clone::clone)
        .map_err(|_| Error::bad("A JSON-RPC request identity is required."))?;
    if !(call.is_string() || call.is_number()) {
        return Err(Error::bad("A JSON-RPC request identity is required."));
    }
    let name = args["name"]
        .as_str()
        .ok_or_else(|| Error::bad("Missing job name."))?;
    let command = args["command"]
        .as_str()
        .ok_or_else(|| Error::bad("Missing job command."))?;
    if args.as_object().is_none_or(|object| object.len() != 2) {
        return Err(Error::bad("Provide only name and command."));
    }
    let key = crate::auth::digest(&format!(
        "{}:{}:{}",
        crate::mcps::grant_key(bearer),
        crate::waits::execution_id(&run),
        call
    ));
    super::jobs::reserve(
        s,
        bearer,
        &run,
        attempt,
        &key,
        &crate::auth::digest(&args.to_string()),
    )
    .await?;
    let result = request_bound(
        s,
        &run,
        Some(attempt),
        &Request::StartJob {
            request_id: key.clone(),
            name: name.into(),
            command: command.into(),
        },
    )
    .await?;
    match result {
        Event::JobStarted { job_id, .. } => {
            super::jobs::track(s, &run, attempt, &key, &job_id).await?;
            Ok(serde_json::json!({ "jobId": job_id }))
        }
        Event::Refused { message, .. } => {
            super::jobs::refused(s, &run, attempt, &key).await?;
            Err(Error::conflict(message))
        }
        _ => Err(Error::unavailable("Invalid job acknowledgement.")),
    }
}

pub(crate) fn tool() -> serde_json::Value {
    serde_json::json!({
        "name": "start_job",
        "description": "Start background work owned by this run. Use job_finished with wait_for to end this agent turn while the job keeps running. Completion includes exit status, duration and a bounded log tail.",
        "inputSchema": { "type": "object", "properties": {
            "name": { "type": "string", "minLength": 1, "maxLength": 256 },
            "command": { "type": "string", "minLength": 1, "maxLength": 131072 }
        }, "required": ["name", "command"], "additionalProperties": false }
    })
}

pub(crate) async fn turn_receipt(
    s: &crate::service::Service,
    run: &serde_json::Value,
    turn: &str,
) -> crate::error::Result<crate::background_jobs::controller::TurnReceipt> {
    use crate::error::Error;
    let saved = RunCheckpoint::load(&s.store, text(run, "id"))
        .await?
        .ok_or_else(|| Error::conflict("No environment checkpoint."))?;
    let attempt = saved
        .runner_id()
        .ok_or_else(|| Error::conflict("No environment."))?;
    let url = crate::nodes::transport::url_for_node(
        s,
        saved.node_id.as_ref().and_then(|value| value.as_deref()),
    );
    let response = s
        .http
        .get(format!("{url}/runs/{attempt}/turn/{turn}"))
        .bearer_auth(crate::execution::secret(&s.config.data_dir, "runner-secret").await?)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| Error::unavailable("Provider completion could not be verified."))?;
    if !response.status().is_success() {
        return Err(Error::unavailable("Provider completion was not confirmed."));
    }
    let receipt: crate::background_jobs::controller::TurnReceipt =
        response.json().await.map_err(Error::internal)?;
    if receipt.attempt_id != attempt
        || receipt.turn_id != turn
        || receipt.generation.is_empty()
        || receipt.log_start > receipt.log_end
    {
        return Err(Error::bad_gateway("Invalid provider completion identity."));
    }
    if saved
        .held_environment
        .as_ref()
        .is_some_and(|held| held.generation != receipt.generation || held.attempt != attempt)
    {
        return Err(Error::conflict(
            "Execution environment changed between turns.",
        ));
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_request_cannot_follow_a_replacement_environment() {
        let mut saved = RunCheckpoint::fresh(None);
        saved.runner_id = Some(Some("original".into()));
        assert!(require_attempt(&saved, Some("original")).is_ok());
        saved.runner_id = Some(Some("replacement".into()));
        assert!(require_attempt(&saved, Some("original")).is_err());
        assert!(require_attempt(&saved, None).is_ok());
    }

    #[tokio::test]
    async fn a_held_environment_still_owns_cancellation_but_not_a_provider_slot() {
        let worker = Worker::default();
        let active = HashSet::from(["held".into(), "running".into()]);
        worker.held.lock().await.insert("held".into());
        assert_eq!(
            worker.provider_active(&active).await,
            HashSet::from(["running".into()])
        );
        assert!(active.contains("held"));
    }
}
