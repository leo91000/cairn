//! A live environment can outlast its provider turn. Its original execution
//! future remains the sole owner of cleanup, cancellation and the node lease.
use futures_util::TryStreamExt;
use std::collections::HashSet;

tokio::task_local! {
    /// Stable JSON-RPC identity scoped to the authenticated request.
    pub(crate) static CALL_ID: serde_json::Value;
}

use super::{
    Worker,
    checkpoint::{HeldEnvironment, RunCheckpoint, Settled},
};
use crate::{
    config::now,
    error::{Error, Result},
    run_status::RunStatus,
    store::Db,
    validation::text,
};
use cairn_contracts::EventKind;

pub(super) const DEADLINE_MESSAGE: &str = "Environment background work reached its time limit.";
pub(super) const HELD_DEADLINE_CODE: &str = "environment_background_deadline";
const ENVIRONMENT_UNAVAILABLE: &str = "environment_observation_unavailable";

pub(super) fn observation_ended(error: &Error) -> bool {
    matches!(
        error.code.as_deref(),
        Some(HELD_DEADLINE_CODE | ENVIRONMENT_UNAVAILABLE)
    )
}

pub(super) fn reported_control_failure(error: &Error) -> bool {
    use crate::background_jobs::protocol::{CONTROL_INTERRUPTED, INVALID_CONTROL_FRAME};
    matches!(
        error.code.as_deref(),
        Some(CONTROL_INTERRUPTED | INVALID_CONTROL_FRAME)
    )
}

fn observation_reply_matches(
    request: &crate::background_jobs::protocol::Request,
    reply: &crate::background_jobs::protocol::Event,
) -> bool {
    use crate::background_jobs::protocol::{Event, Request};
    match (request, reply) {
        (
            Request::LookupJob { request_id, .. },
            Event::JobLookup {
                request_id: returned,
                ..
            }
            | Event::Refused {
                request_id: returned,
                ..
            },
        )
        | (
            Request::JobStatus { request_id, .. },
            Event::Refused {
                request_id: returned,
                ..
            },
        ) => request_id == returned,
        (
            Request::Activity { request_id },
            Event::Activity {
                request_id: returned,
                ..
            }
            | Event::Refused {
                request_id: returned,
                ..
            },
        ) => request_id == returned,
        (
            Request::JobStatus { request_id, job_id },
            Event::JobStatus {
                request_id: returned,
                job_id: observed,
                ..
            },
        ) => request_id == returned && job_id == observed,
        (Request::LookupJob { .. } | Request::JobStatus { .. } | Request::Activity { .. }, _) => {
            false
        }
        _ => true,
    }
}

pub(super) fn close_control_failure(
    reply: Result<crate::background_jobs::protocol::Event>,
    expected_request: &str,
) -> Option<Error> {
    use crate::background_jobs::protocol::{Event, invalid_control_frame};

    match reply {
        Ok(Event::Accepted { request_id }) if request_id == expected_request => None,
        Ok(_) => Some(invalid_control_frame()),
        Err(error) if reported_control_failure(&error) => Some(error),
        Err(_) => None,
    }
}

fn observation_unavailable(message: &str) -> Error {
    Error::coded(503, ENVIRONMENT_UNAVAILABLE, message)
}

fn control_failure_key(run: &str) -> String {
    format!("environment-control-failure:{run}")
}

pub(super) fn take_observed_control_failure(
    db: &Db<'_>,
    run: &str,
    saved: &RunCheckpoint,
) -> Result<Option<String>> {
    let key = control_failure_key(run);
    let Some(receipt) = db.kv(&key)? else {
        return Ok(None);
    };
    db.delete(&key)?;
    if saved.runner_id().is_some() && receipt["attemptId"].as_str() == saved.runner_id() {
        return Ok(receipt["message"].as_str().map(str::to_owned));
    }
    Ok(None)
}

async fn remember_observed_control_failure(
    s: &crate::service::Service,
    run: &str,
    attempt: &str,
    message: &str,
) -> Result<()> {
    let (run, attempt, message) = (run.to_owned(), attempt.to_owned(), message.to_owned());
    s.store
        .transaction(move |db| {
            let Some(mut saved) = RunCheckpoint::load_in(db, &run)? else {
                return Ok(());
            };
            if saved.runner_id() != Some(attempt.as_str()) {
                return Ok(());
            }
            let Some(current) = db.run_state(&run)? else {
                return Ok(());
            };
            let retiring = saved.settled().is_some() && current.recovery_pending;
            if saved.held_environment.is_some() || retiring {
                db.set(
                    &control_failure_key(&run),
                    &serde_json::json!({ "attemptId": attempt, "message": message }),
                    None,
                )?;
            } else if current.cancel_requested_at.is_none()
                && saved.retired_environment_deadline.is_some()
                && saved.settled().is_some_and(|outcome| {
                    (outcome.status == RunStatus::Succeeded
                        && current.status == RunStatus::Succeeded)
                        || (outcome.error.as_deref() == Some(DEADLINE_MESSAGE)
                            && current.error.as_deref() == Some(DEADLINE_MESSAGE))
                })
            {
                // A reply already in flight may arrive after the physical fence.
                // Only amend the retired turn while it still owns the run result.
                let failure = Settled::failure(RunStatus::Failed, &message);
                crate::chats::continuation::cancel_for_run(db, &run)?;
                db.event(&run, &EventKind::Error, &message, None)?;
                db.change_run(&run, failure.run_changes())?;
                saved.settled = Some(Some(failure));
                saved.store_in(db, &run)?;
            }
            Ok(())
        })
        .await
}

#[derive(Clone, Copy)]
pub(super) enum Retirement<'a> {
    Close,
    Deadline,
    Interrupted(&'a str),
}

pub(super) fn completed_chat(db: &Db<'_>, run: &cairn_contracts::RunState) -> Result<bool> {
    Ok(run.status == RunStatus::Succeeded
        && run.trigger == "chat"
        && db
            .conversation_for_run(&run.id)?
            .is_some_and(|chat| chat.wakeup.is_none()))
}

/// Record who owns retirement and the outcome to publish after the outer fence.
pub(super) fn settle_retirement(
    db: &Db<'_>,
    id: &str,
    saved: &mut RunCheckpoint,
    reason: Retirement<'_>,
    held_deadline: i64,
    expired_at: Option<i64>,
) -> Result<()> {
    let run = db
        .run_state(id)?
        .ok_or_else(|| Error::not_found("Run not found"))?;
    if let Some(deadline) = expired_at.or_else(|| {
        run.cancel_requested_at
            .filter(|cancelled| *cancelled >= held_deadline)
            .map(|_| held_deadline)
    }) {
        saved.retired_environment_deadline = Some(deadline);
    }
    let observed_control_failure = take_observed_control_failure(db, id, saved)?;
    let interruption = match reason {
        Retirement::Close => observed_control_failure.as_deref(),
        Retirement::Deadline => observed_control_failure
            .as_deref()
            .or(Some(DEADLINE_MESSAGE)),
        Retirement::Interrupted(message) => Some(message),
    };
    let settled = if run.cancel_requested_at.is_some() {
        Settled::failure(RunStatus::Cancelled, "Background work was stopped.")
    } else if let Some(message) = interruption {
        // A wait or an accepted continuation is still unfinished even when
        // the last provider turn succeeded. The cutoff for new input also
        // applies when a separate error interrupted supervision.
        if matches!(reason, Retirement::Deadline)
            && observed_control_failure.is_none()
            && completed_chat(db, &run)?
        {
            db.event(id, &EventKind::Diagnostic, message, None)?;
            Settled::from_state(&run)
        } else {
            crate::chats::continuation::cancel_for_run(db, id)?;
            db.event(id, &EventKind::Error, message, None)?;
            Settled::failure(RunStatus::Failed, message)
        }
    } else if run.status == RunStatus::Succeeded && run.trigger == "chat" {
        saved.retired_environment_deadline = Some(held_deadline);
        Settled::from_state(&run)
    } else {
        return Ok(());
    };
    let unfinished = settled.status != RunStatus::Succeeded;
    saved.settled = Some(Some(settled));
    db.change_run(
        id,
        crate::runs::RunChanges {
            status: unfinished.then_some(RunStatus::Queued),
            finished_at: unfinished.then_some(None),
            recovery_pending: Some(true),
            ..Default::default()
        },
    )?;
    Ok(())
}

pub(super) fn record_close_failure(
    db: &Db<'_>,
    id: &str,
    saved: &mut RunCheckpoint,
    held: &HeldEnvironment,
    expired_at: Option<i64>,
    message: &str,
) -> Result<()> {
    if saved.runner_id() == Some(held.attempt.as_str())
        && saved.settled().is_some_and(|outcome| {
            outcome.status == RunStatus::Succeeded
                || outcome.error.as_deref() == Some(DEADLINE_MESSAGE)
        })
    {
        settle_retirement(
            db,
            id,
            saved,
            Retirement::Interrupted(message),
            held.expires_at,
            expired_at,
        )?;
    }
    Ok(())
}

/// Read-only credential observation for one retained environment. This grants
/// no tool access and cannot survive replacement, expiry or revocation.
#[derive(Clone)]
pub(crate) struct Observation {
    run: String,
    execution: serde_json::Value,
    attempt: String,
    generation: String,
}

impl Observation {
    pub(crate) fn capture(run: &serde_json::Value, saved: &RunCheckpoint) -> Option<Self> {
        let held = saved.held_environment.as_ref()?;
        Some(Self {
            run: text(run, "id").into(),
            execution: crate::waits::execution_id(run).clone(),
            attempt: held.attempt.clone(),
            generation: held.generation.clone(),
        })
    }

    pub(crate) fn authorize(&self, db: &Db<'_>) -> crate::error::Result<()> {
        use crate::error::{Error, required};
        let run = required(db.run(&self.run)?, "Run not found")?;
        let saved = required(
            RunCheckpoint::load_in(db, &self.run)?,
            "Environment missing",
        )?;
        let current = required(
            db.agent_snapshot(text(&run["snapshot"]["agent"], "id"))?,
            "Agent not found",
        )?;
        let changed = || Error::forbidden("The observed environment ended or changed.");
        let held = owned_hold(db, &run, &saved, &self.attempt).ok_or_else(changed)?;
        if crate::waits::execution_id(&run) != &self.execution
            || held.generation != self.generation
            || crate::waits::Owner::for_run(db, &run)?.run_id() != self.run
            || !crate::service::covers(&current, &run["snapshot"]["agent"])
        {
            return Err(changed());
        }
        crate::delegation::authorize_run(db, &run)?;
        if now() >= held.expires_at {
            return Err(Error::coded(403, HELD_DEADLINE_CODE, DEADLINE_MESSAGE));
        }
        Ok(())
    }

    pub(crate) fn authority(&self) -> crate::capabilities::plugin_packages::Authority {
        let observation = self.clone();
        crate::capabilities::plugin_packages::Authority::default()
            .with_guard(move |db| observation.authorize(db))
    }
}

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
    Ok(owned_hold(db, run, &saved, attempt).is_some_and(|held| held.expires_at > now()))
}

fn owned_hold<'a>(
    db: &Db<'_>,
    run: &serde_json::Value,
    saved: &'a RunCheckpoint,
    attempt: &str,
) -> Option<&'a HeldEnvironment> {
    if crate::conversation_lifecycle::require_active_run(db, text(run, "id")).is_err() {
        return None;
    }
    saved.held_environment.as_ref().filter(|held| {
        held.attempt == attempt
            && saved.runner_id() == Some(attempt)
            && run["cancelRequestedAt"].is_null()
            && matches!(
                run["status"].as_str(),
                Some("succeeded" | "waiting" | "queued" | "running")
            )
    })
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
    use crate::background_jobs::{
        Outcome,
        protocol::{Event, Request},
    };
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
            if matches!(request, Request::ResumeTurn { .. }) {
                60
            } else {
                12
            },
        ))
        .send()
        .await
        .map_err(|_| {
            observation_unavailable(
                "Environment request outcome is unknown; do not restart this job.",
            )
        })?;
    let status = response.status();
    if !status.is_success() && status != reqwest::StatusCode::BAD_GATEWAY {
        if matches!(
            status,
            reqwest::StatusCode::SERVICE_UNAVAILABLE
                | reqwest::StatusCode::REQUEST_TIMEOUT
                | reqwest::StatusCode::GATEWAY_TIMEOUT
        ) {
            return Err(observation_unavailable(
                "Execution environment rejected the request.",
            ));
        }
        return Err(Error::unavailable(
            "Execution environment rejected the request.",
        ));
    }
    let result = crate::process::read_bounded(
        tokio_util::io::StreamReader::new(response.bytes_stream().map_err(std::io::Error::other)),
        256 * 1024,
    )
    .await
    .map_err(|error| {
        if error.is_internal() || !status.is_success() {
            observation_unavailable("Execution environment response was interrupted.")
        } else {
            crate::background_jobs::protocol::invalid_control_frame()
        }
    })
    .and_then(|bytes| {
        if status == reqwest::StatusCode::BAD_GATEWAY {
            use crate::background_jobs::protocol::{CONTROL_INTERRUPTED, INVALID_CONTROL_FRAME};

            let body = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default();
            let message = "Execution controller failed to complete the control request.";
            if let Some(code @ (CONTROL_INTERRUPTED | INVALID_CONTROL_FRAME)) =
                body["code"].as_str()
            {
                return Err(Error::coded(502, code, message));
            }
            // A gateway-generated failure carries no proof of a controller fault.
            return Err(observation_unavailable(message));
        }
        serde_json::from_slice(&bytes)
            .ok()
            .filter(|reply| observation_reply_matches(request, reply))
            .ok_or_else(crate::background_jobs::protocol::invalid_control_frame)
    });
    if matches!(
        request,
        Request::LookupJob { .. } | Request::JobStatus { .. }
    ) {
        let failure = match &result {
            Err(error) if reported_control_failure(error) => Some(error.message.as_str()),
            Ok(Event::JobStatus {
                outcome: Outcome::Unknown { .. },
                ..
            }) => {
                // Only the controller can report an unknown workload here. The
                // manager's expiry marker is stored locally and cannot be returned.
                Some("Workload ownership is unknown; environment fencing is required.")
            }
            _ => None,
        };
        if let Some(message) = failure
            && let Err(store_error) =
                remember_observed_control_failure(s, id, attempt, message).await
        {
            tracing::warn!(run_id = id, %attempt, error = %store_error, "Could not save an observed background control failure");
        }
    }
    result
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
    fn a_job_observation_requires_the_requested_event_and_identity() {
        use crate::background_jobs::{
            Outcome,
            protocol::{Event, Request},
        };
        let request = Request::JobStatus {
            request_id: "request".into(),
            job_id: "job".into(),
        };
        for (request_id, job_id, valid) in [
            ("request", "job", true),
            ("other", "job", false),
            ("request", "other", false),
        ] {
            let reply = Event::JobStatus {
                request_id: request_id.into(),
                job_id: job_id.into(),
                outcome: Outcome::Unknown {
                    message: "No result".into(),
                },
            };
            assert_eq!(observation_reply_matches(&request, &reply), valid);
        }
        assert!(!observation_reply_matches(
            &request,
            &Event::Accepted {
                request_id: "request".into()
            }
        ));
        assert!(!observation_reply_matches(
            &Request::Activity {
                request_id: "request".into()
            },
            &Event::Activity {
                request_id: "other".into(),
                jobs: false,
                containers: false
            },
        ));
    }

    #[tokio::test]
    async fn interruption_keeps_its_cause_when_input_cutoff_has_elapsed() {
        use crate::run_fixture::DbRunFixture;
        use serde_json::json;

        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let deadline = now() - 1;
                let message = "New credential paths require a fresh environment.";
                for (id, status) in [("active", "running"), ("completed", "succeeded")] {
                    let chat = format!("chat-{id}");
                    db.add_run_fixture(
                        &json!({"id":id, "taskId":chat, "status":status, "trigger":"chat"}),
                        None,
                    )?;
                    db.save_conversation_boundary(&json!({"id": chat, "runId": id}))?;
                    let mut saved = RunCheckpoint::fresh(None);
                    settle_retirement(
                        db,
                        id,
                        &mut saved,
                        Retirement::Interrupted(message),
                        deadline,
                        Some(deadline),
                    )?;
                    assert_eq!(saved.retired_environment_deadline, Some(deadline));
                    let settled = saved.settled().unwrap();
                    assert_eq!(settled.status, RunStatus::Failed);
                    assert_eq!(settled.error.as_deref(), Some(message));
                }
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn observation_requires_the_same_live_environment_after_provider_completion() {
        use crate::{run_fixture::DbRunFixture, run_status::RunStatus};
        use serde_json::json;
        let root = tempfile::tempdir().unwrap();
        let config = serde_json::from_value(json!({
            "dataDir":root.path().join("data"),"home":root.path().join("home"),
            "workspaceRoots":[root.path()],"publicUrl":"http://localhost:4310",
            "host":"127.0.0.1","port":4310,"codexBin":"codex","concurrency":1,
            "logger":false,"workerEnabled":false,"runnerUrl":"http://runner.invalid"
        }))
        .unwrap();
        let service = crate::service::Service::new(config).await.unwrap();
        let agent = service
            .agent_snapshot(crate::config::MAIN_AGENT_ID)
            .await
            .unwrap();
        service.store.transaction(move |db| {
            let record = db.add_run_fixture(&json!({"id":"observed","taskId":"observed","status":"succeeded","snapshot":{"agent":agent,"projects":[]}}),None)?;
            db.save_conversation_boundary(&json!({ "id": "observed", "runId": "observed" }))?;
            let mut saved = RunCheckpoint::fresh(None);
            saved.runner_id = Some(Some("attempt".into()));
            saved.completed = Some(true);
            saved.held_environment = Some(super::super::checkpoint::HeldEnvironment { attempt:"attempt".into(),generation:"generation".into(),expires_at:now()+60_000 });
            saved.store_in(db,"observed")?;
            let observation = Observation::capture(&record,&saved).unwrap();
            observation.authorize(db)?;
            let mut replaced = saved.clone();
            replaced.held_environment.as_mut().unwrap().generation = "replacement".into();
            replaced.store_in(db,"observed")?;
            assert!(observation.authorize(db).is_err());
            saved.store_in(db,"observed")?;
            db.change_run("observed",crate::runs::RunChanges { cancel_requested_at:Some(Some(now())), ..Default::default() })?;
            assert!(observation.authorize(db).is_err());
            db.change_run("observed",crate::runs::RunChanges { cancel_requested_at:Some(None),status:Some(RunStatus::Failed), ..Default::default() })?;
            assert!(observation.authorize(db).is_err());
            db.change_run("observed",crate::runs::RunChanges { status:Some(RunStatus::Succeeded), ..Default::default() })?;
            saved.held_environment.as_mut().unwrap().expires_at = now()-1;
            saved.store_in(db,"observed")?;
            let error = observation.authorize(db).unwrap_err();
            assert_eq!(error.code.as_deref(), Some(HELD_DEADLINE_CODE));
            Ok(())
        }).await.unwrap();
    }

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
