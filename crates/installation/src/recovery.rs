use crate::{
    config::now,
    error::{Error, Result},
    execution::secret,
    provider::Provider,
    run_status::RunStatus,
    service::Service,
    validation::text,
    worker::checkpoint::RunCheckpoint,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, time::Duration};

/// Identifies one process instance across PID reuse and host reboots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u64,
    pub start: String,
    pub boot: String,
}

pub async fn process_identity(pid: u32) -> Result<Option<ProcessIdentity>> {
    let stat = match tokio::fs::read_to_string(format!("/proc/{pid}/stat")).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let fields = stat
        .rsplit_once(") ")
        .map(|(_, s)| s.split(' ').collect::<Vec<_>>())
        .ok_or_else(|| Error::internal("Invalid process status"))?;
    if fields.first() == Some(&"Z") {
        return Ok(None);
    }
    let start = fields
        .get(19)
        .ok_or_else(|| Error::internal("Invalid process status"))?;
    let boot = tokio::fs::read_to_string("/proc/sys/kernel/random/boot_id").await?;
    Ok(Some(ProcessIdentity {
        pid: pid.into(),
        start: (*start).to_owned(),
        boot: boot.trim().to_owned(),
    }))
}

async fn is_running(pid: u32, identity: &ProcessIdentity) -> Result<bool> {
    Ok(process_identity(pid)
        .await?
        .is_some_and(|current| current.start == identity.start && current.boot == identity.boot))
}

pub async fn fence_process(identity: &ProcessIdentity) -> Result<()> {
    let pid = i32::try_from(identity.pid)
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| Error::bad("Invalid saved process identity."))?;
    let unsigned_pid = pid.unsigned_abs();
    if !is_running(unsigned_pid, identity).await? {
        return Ok(());
    }
    let result = unsafe { libc::kill(pid, libc::SIGTERM) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    let deadline = now() + 5000;
    while is_running(unsigned_pid, identity).await? {
        if now() >= deadline {
            return Err(Error::unavailable(
                "Waiting for the previous run process to stop.",
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

/// Observe the existing attempt before stopping it or deciding whether to replay.
/// This never acquires a workspace, starts an agent, or replays retained output.
#[derive(Clone)]
pub(crate) enum ObservationAction {
    Reserve,
    Release,
    Exhausted,
    Record(Option<crate::runner::ControllerResult>),
    PublishCleanup(Option<crate::runner::ControllerResult>),
}

pub(crate) async fn terminal_code<F, Fut>(
    s: &Service,
    attempt: &str,
    node: Option<&str>,
    cancel: &tokio_util::sync::CancellationToken,
    limit: Duration,
    cleanup_observation_pending: bool,
    mut persist: F,
) -> Result<()>
where
    F: FnMut(ObservationAction) -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let runner_url = crate::nodes::transport::url_for_node(s, node);
    if runner_url.is_empty() {
        let action = if cleanup_observation_pending {
            ObservationAction::PublishCleanup(None)
        } else {
            ObservationAction::Exhausted
        };
        persist(action).await?;
        return Ok(());
    }
    let token = match secret(&s.config.data_dir, "runner-secret").await {
        Ok(token) => token,
        Err(error) if cleanup_observation_pending => {
            tracing::warn!(attempt, %error, "Could not authenticate optional cleanup observation");
            persist(ObservationAction::PublishCleanup(None)).await?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let url = format!("{runner_url}/runs/{attempt}");
    let max_observation = Duration::from_secs(5);
    // An interrupted cleanup observation already has a durable workload fence.
    // On restart its read budget is independent of the expired workload budget.
    let budget = if cleanup_observation_pending {
        max_observation
    } else {
        limit.min(max_observation)
    };
    let deadline = tokio::time::Instant::now() + budget;
    if cleanup_observation_pending {
        let result = observe_cleanup_retirement(s, &url, &token, deadline, cancel).await;
        persist(ObservationAction::PublishCleanup(result)).await?;
        return Ok(());
    }
    loop {
        if s.shutdown.is_cancelled() || cancel.is_cancelled() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            persist(ObservationAction::Exhausted).await?;
            return Ok(());
        }
        // Do not drop an in-flight transaction: it may still commit after its
        // caller is cancelled, leaving an active checkpoint's memory stale.
        if !persist(ObservationAction::Reserve).await? {
            persist(ObservationAction::Exhausted).await?;
            return Ok(());
        }
        if s.shutdown.is_cancelled()
            || cancel.is_cancelled()
            || tokio::time::Instant::now() >= deadline
        {
            persist(ObservationAction::Release).await?;
            if !s.shutdown.is_cancelled() && !cancel.is_cancelled() {
                persist(ObservationAction::Record(None)).await?;
            }
            return Ok(());
        }
        let wait = crate::runner::wait_for_exit(&s.http, &url, &token);
        let result = tokio::select! {
            () = s.shutdown.cancelled() => None,
            () = cancel.cancelled() => None,
            result = tokio::time::timeout_at(deadline, wait) => Some(result),
        };
        match result {
            None => {
                persist(ObservationAction::Release).await?;
                return Ok(());
            }
            Some(Ok(Ok(code))) => {
                let pending_cleanup = code.finalization_failed && code.native_cleanup.is_some();
                persist(ObservationAction::Record(Some(code))).await?;
                // The existing deadline applies to the original observation.
                // Persist the known failure before trying optional retirement.
                if pending_cleanup {
                    let result =
                        observe_cleanup_retirement(s, &url, &token, deadline, cancel).await;
                    persist(ObservationAction::PublishCleanup(result)).await?;
                }
                return Ok(());
            }
            Some(Err(_)) => {
                persist(ObservationAction::Record(None)).await?;
                return Ok(());
            }
            Some(Ok(Err(_))) => {
                persist(ObservationAction::Release).await?;
            }
        }
        let next_read = (tokio::time::Instant::now() + Duration::from_millis(200)).min(deadline);
        tokio::select! {
            () = s.shutdown.cancelled() => return Ok(()),
            () = cancel.cancelled() => return Ok(()),
            () = tokio::time::sleep_until(next_read) => {}
        }
    }
}

/// Optional retirement evidence never replaces the already durable failure.
async fn observe_cleanup_retirement(
    s: &Service,
    url: &str,
    token: &str,
    deadline: tokio::time::Instant,
    cancel: &tokio_util::sync::CancellationToken,
) -> Option<crate::runner::ControllerResult> {
    if tokio::time::Instant::now() >= deadline || s.shutdown.is_cancelled() || cancel.is_cancelled()
    {
        return None;
    }
    let retirement = crate::runner::observe_cleanup_retirement(&s.http, url, token);
    tokio::select! {
        () = s.shutdown.cancelled() => None,
        () = cancel.cancelled() => None,
        result = tokio::time::timeout_at(deadline, retirement) => result.ok().and_then(Result::ok),
    }
    .filter(|result| result.physical_finalized && result.native_cleanup.is_some())
}

/// Stops every trace of the run's previous execution before it may be relaunched.
pub async fn fence(s: &Service, run: &Value) -> Result<()> {
    fence_authorized(s, run, crate::authorization::Authorization::default()).await
}

pub(crate) async fn fence_authorized(
    s: &Service,
    run: &Value,
    authorize: crate::authorization::Authorization,
) -> Result<()> {
    let run_id = text(run, "id");
    let checkpoint = RunCheckpoint::load(&s.store, run_id).await?;
    fence_attempt_authorized(s, run, checkpoint.as_ref(), authorize).await
}

/// Compare the physical owner, not the checkpoint's mutable evidence.
pub(crate) fn same_execution(left: Option<&RunCheckpoint>, right: Option<&RunCheckpoint>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.runner_id() == right.runner_id()
                && left.node_id == right.node_id
                && left.process() == right.process()
        }
        _ => false,
    }
}

/// Fence only the attempt selected by the caller, including its process and node.
pub(crate) async fn fence_attempt(
    s: &Service,
    run: &Value,
    expected: Option<&RunCheckpoint>,
) -> Result<()> {
    fence_attempt_authorized(
        s,
        run,
        expected,
        crate::authorization::Authorization::default(),
    )
    .await
}

async fn fence_attempt_authorized(
    s: &Service,
    run: &Value,
    expected: Option<&RunCheckpoint>,
    authorize: crate::authorization::Authorization,
) -> Result<()> {
    let run_id = text(run, "id");
    let checkpoint = RunCheckpoint::load(&s.store, run_id).await?;
    if !same_execution(checkpoint.as_ref(), expected) {
        return Err(Error::conflict(
            "Execution attempt changed during recovery.",
        ));
    }
    let process = checkpoint
        .as_ref()
        .and_then(RunCheckpoint::process)
        .cloned();
    let node = checkpoint
        .as_ref()
        .and_then(|saved| saved.node_id.as_ref())
        .and_then(|node| node.as_deref())
        .map(str::to_owned);
    authorize.recheck().await?;
    if let Some(process) = &process {
        fence_process(process).await?;
    }
    let isolated =
        run["isolated"] == true || checkpoint.as_ref().is_some_and(RunCheckpoint::isolated);
    if isolated {
        let attempt = expected
            .and_then(RunCheckpoint::runner_id)
            .unwrap_or(run_id);
        let runner_url = crate::nodes::transport::url_for_node(s, node.as_deref());
        let settled_status = checkpoint
            .as_ref()
            .filter(|saved| {
                saved.managed()
                    && saved.finalization_failure_attempt.as_deref() != Some(attempt)
                    && saved.acknowledged_finalization_failure_attempt.as_deref() != Some(attempt)
            })
            .and_then(RunCheckpoint::settled)
            .map(|outcome| outcome.status)
            .filter(|status| matches!(status, RunStatus::Succeeded | RunStatus::Failed));
        let may_defer = settled_status == Some(RunStatus::Succeeded)
            && run["trigger"] == "chat"
            && run["status"] == RunStatus::Succeeded
            && run["recoveryPending"] == true
            && run["cancelRequestedAt"].is_null()
            && checkpoint
                .as_ref()
                .and_then(RunCheckpoint::prepared)
                .is_some_and(|plan| plan["backend"] == "native");
        let pending = checkpoint
            .as_ref()
            .and_then(RunCheckpoint::pending_native_cleanup);
        authorize.recheck().await?;
        let cleanup = match pending {
            Some(pending) => FencedCleanup::Deferred(if may_defer {
                observe_deferred_cleanup(s, &runner_url, attempt, &authorize, pending).await
            } else {
                pending.clone()
            }),
            None => {
                fence_runner(
                    s,
                    &runner_url,
                    attempt,
                    &authorize,
                    settled_status,
                    may_defer,
                )
                .await?
            }
        };
        if !matches!(cleanup, FencedCleanup::Clear) {
            let id = run_id.to_owned();
            let attempt = attempt.to_owned();
            let expected = expected.cloned();
            let observed_pending = pending.cloned();
            let cleanup_authorize = authorize.clone();
            s.store
                .transaction(move |db| {
                    cleanup_authorize.check_local(db)?;
                    let mut current = RunCheckpoint::load_in(db, &id)?;
                    if !same_execution(current.as_ref(), expected.as_ref()) {
                        return Err(Error::conflict(
                            "Execution attempt changed during recovery.",
                        ));
                    }
                    let Some(saved) = &mut current else {
                        return Ok(());
                    };
                    if saved.runner_id() != Some(attempt.as_str())
                        || saved.pending_native_cleanup() != observed_pending.as_ref()
                    {
                        return Ok(());
                    }
                    let cleanup = match cleanup {
                        FencedCleanup::Clear => return Ok(()),
                        FencedCleanup::Failed(result) => result,
                        FencedCleanup::Deferred(pending) => {
                            let eligible = observed_pending.is_some()
                                || (saved
                                    .settled()
                                    .is_some_and(|turn| turn.status == RunStatus::Succeeded)
                                    && db.run_state(&id)?.is_some_and(|state| {
                                        state.status == RunStatus::Succeeded
                                            && state.recovery_pending
                                            && state.cancel_requested_at.is_none()
                                    }));
                            if eligible {
                                if observed_pending.as_ref() == Some(&pending) {
                                    return Ok(());
                                }
                                saved.pending_native_cleanup = Some(pending);
                                return saved.store_in(db, &id);
                            }
                            pending.failure()
                        }
                    };
                    if !(cleanup.finalization_failed
                        && cleanup.native_cleanup.is_none()
                        && saved.acknowledged_finalization_failure_attempt.as_deref()
                            == Some(attempt.as_str()))
                        && (settled_status.is_some()
                            || (saved.managed()
                                && cleanup.finalization_failed
                                && cleanup.native_cleanup.is_none())
                            || saved
                                .prepared()
                                .is_some_and(|plan| plan["backend"] == "native"))
                    {
                        saved.record_controller_result(db, &id, &attempt, Some(cleanup))?;
                        saved.store_in(db, &id)?;
                    }
                    Ok(())
                })
                .await?;
        }
    }
    let id = run_id.to_owned();
    let expected = expected
        .and_then(RunCheckpoint::runner_id)
        .map(str::to_owned);
    let release = isolated.then(|| expected.clone().unwrap_or_else(|| id.clone()));
    authorize.recheck().await?;
    s.store
        .transaction(move |db| {
            authorize.check_local(db)?;
            let mut current = RunCheckpoint::load_in(db, &id)?;
            let current_attempt = current.as_ref().and_then(RunCheckpoint::runner_id);
            let current_node = current
                .as_ref()
                .and_then(|saved| saved.node_id.as_ref())
                .and_then(|node| node.as_deref());
            let current_process = current.as_ref().and_then(RunCheckpoint::process);
            if current_attempt != expected.as_deref()
                || current_node != node.as_deref()
                || current_process != process.as_ref()
            {
                return Err(Error::conflict(
                    "Execution attempt changed during recovery.",
                ));
            }
            if let Some(saved) = &mut current {
                saved.process = None;
                saved.store_in(db, &id)?;
            }
            if let Some(attempt) = release {
                crate::nodes::placement::release_in(db, &attempt)?;
            }
            Ok(())
        })
        .await
}

/// Fences the last execution of a detached environment. Its checkpoint has been reset,
/// so the retirement keeps the identity it captured.
pub(crate) async fn fence_retired(
    s: &Service,
    attempt: Option<&str>,
    node: Option<&str>,
    process: Option<&ProcessIdentity>,
    isolated: bool,
) -> Result<()> {
    if let Some(process) = process {
        fence_process(process).await?;
    }
    let Some(attempt) = attempt else {
        return Ok(());
    };
    let owned = s.store.attempt(attempt).await?;
    // A released attempt was confirmed stopped before its capacity was returned.
    if isolated && owned.is_some_and(|owned| crate::nodes::is_active_attempt(&owned)) {
        let runner_url = crate::nodes::transport::url_for_node(s, node);
        fence_runner(
            s,
            &runner_url,
            attempt,
            &crate::authorization::Authorization::default(),
            None,
            false,
        )
        .await?;
    }
    let attempt = attempt.to_owned();
    s.store
        .transaction(move |db| crate::nodes::placement::release_in(db, &attempt))
        .await
}

// Once the workload is fenced, observe delayed disk retirement on the normal
// completed-environment recovery loop without retaining the execution slot.
const NATIVE_CLEANUP_FOLLOW_UP_MS: i64 = 5 * 60 * 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DeferredCleanupState {
    Observing,
    Retired,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PendingNativeCleanup {
    pub(crate) attempt: String,
    pub(crate) state: DeferredCleanupState,
    until: i64,
    message: String,
}

impl PendingNativeCleanup {
    pub(crate) fn failure(self) -> crate::runner::ControllerResult {
        crate::runner::ControllerResult {
            code: crate::runner::CONTROLLER_FAILED,
            finalization_failed: true,
            physical_finalized: false,
            terminal_reason: None,
            native_cleanup: Some(self.message),
        }
    }
}

enum FencedCleanup {
    Clear,
    Failed(crate::runner::ControllerResult),
    Deferred(PendingNativeCleanup),
}

async fn observe_deferred_cleanup(
    s: &Service,
    runner_url: &str,
    attempt: &str,
    authorize: &crate::authorization::Authorization,
    pending: &PendingNativeCleanup,
) -> PendingNativeCleanup {
    let mut pending = pending.clone();
    // A persisted verdict is sufficient after restart, including when the
    // controller has disconnected between observation and publication.
    if pending.state != DeferredCleanupState::Observing || s.shutdown.is_cancelled() {
        return pending;
    }
    let remaining = pending.until.saturating_sub(now());
    if remaining <= 0 {
        pending.state = DeferredCleanupState::Expired;
        return pending;
    }
    let budget = Duration::from_millis(remaining.unsigned_abs().min(2_000));
    let observe = async {
        let credential = secret(&s.config.data_dir, "runner-secret").await?;
        let url = format!("{runner_url}/runs/{attempt}");
        let result = crate::runner::observe_cleanup_retirement(&s.http, &url, &credential).await?;
        if !result.physical_finalized || result.native_cleanup.is_none() {
            return Ok(false);
        }
        confirm_native_retirement(s, &url, attempt, &credential, authorize).await
    };
    let retired = tokio::select! {
        () = s.shutdown.cancelled() => return pending,
        result = tokio::time::timeout(budget, observe) => matches!(result, Ok(Ok(true))),
    };
    if now() >= pending.until {
        pending.state = DeferredCleanupState::Expired;
    } else if retired {
        pending.state = DeferredCleanupState::Retired;
    }
    pending
}

async fn fence_runner(
    s: &Service,
    runner_url: &str,
    attempt: &str,
    authorize: &crate::authorization::Authorization,
    settled_status: Option<RunStatus>,
    may_defer: bool,
) -> Result<FencedCleanup> {
    authorize.recheck().await?;
    if runner_url.is_empty() {
        return Err(Error::unavailable(
            "Waiting for the isolated runner before recovering this run.",
        ));
    }
    let credential = secret(&s.config.data_dir, "runner-secret").await?;
    authorize.recheck().await?;
    let url = format!("{runner_url}/runs/{attempt}");
    let response = s
        .http
        .delete(&url)
        .bearer_auth(&credential)
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    let mut cleanup = None;
    let confirmed = match response {
        Ok(response) if response.status().is_success() => {
            if let Ok(value) = response.json::<Value>().await {
                cleanup = crate::runner::client_cleanup_result(&value, attempt);
                if cleanup.is_none() {
                    cleanup = crate::runner::plain_finalization_failure(&value);
                }
            }
            if let Some(message) = cleanup
                .as_ref()
                .and_then(|result| result.native_cleanup.as_ref())
                && settled_status == Some(RunStatus::Succeeded)
            {
                match confirm_late_native_cleanup(s, &url, attempt, &credential, authorize).await {
                    Some(true) => return Ok(FencedCleanup::Clear),
                    None if may_defer => {
                        return Ok(FencedCleanup::Deferred(PendingNativeCleanup {
                            attempt: attempt.to_owned(),
                            state: DeferredCleanupState::Observing,
                            until: now() + NATIVE_CLEANUP_FOLLOW_UP_MS,
                            message: message.clone(),
                        }));
                    }
                    _ => {}
                }
            } else if cleanup.is_none() && settled_status.is_some() {
                // Both controllers expose explicit flush failures through the
                // terminal result. DELETE may only acknowledge that they stopped.
                authorize.recheck().await?;
                cleanup = tokio::time::timeout(
                    Duration::from_secs(2),
                    crate::runner::observe_plain_finalization_failure(&s.http, &url, &credential),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .flatten();
            }
            true
        }
        Ok(response) if response.status() == 404 => true,
        _ => false,
    };
    if !confirmed && !node_lease_expired(s, attempt).await? {
        return Err(Error::unavailable(
            "Waiting for the previous VM execution lease to expire.",
        ));
    }
    Ok(cleanup.map_or(FencedCleanup::Clear, FencedCleanup::Failed))
}

/// Keep an acknowledged fence while giving a successful turn's close a short
/// grace. The historical wait proves this attempt flushed. DELETE rechecks
/// the conversation's current quarantine before we discard the pending result.
async fn confirm_late_native_cleanup(
    s: &Service,
    url: &str,
    attempt: &str,
    credential: &str,
    authorize: &crate::authorization::Authorization,
) -> Option<bool> {
    let observe = async {
        loop {
            let result =
                crate::runner::observe_cleanup_retirement(&s.http, url, credential).await?;
            if result.native_cleanup.is_none() {
                return Ok(false);
            }
            if result.physical_finalized {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        confirm_native_retirement(s, url, attempt, credential, authorize).await
    };
    tokio::select! {
        () = s.shutdown.cancelled() => None,
        result = tokio::time::timeout(Duration::from_secs(7), observe) => {
            result.ok().map(|result| matches!(result, Ok(true)))
        }
    }
}

async fn confirm_native_retirement(
    s: &Service,
    url: &str,
    attempt: &str,
    credential: &str,
    authorize: &crate::authorization::Authorization,
) -> Result<bool> {
    authorize.recheck().await?;
    let response = s
        .http
        .delete(url)
        .bearer_auth(credential)
        .send()
        .await
        .map_err(Error::internal)?;
    if !response.status().is_success() {
        return Ok(false);
    }
    let result = response.json::<Value>().await.map_err(Error::internal)?;
    Ok(result["NativeCleanupRetired"]["attemptId"].as_str() == Some(attempt))
}

/// An unreachable runner is only presumed stopped once its node lease has lapsed.
async fn node_lease_expired(s: &Service, attempt: &str) -> Result<bool> {
    let owned = s
        .store
        .attempt(attempt)
        .await?
        .ok_or_else(|| Error::conflict("Attempt removed."))?;
    let leased = owned.lease_required || owned.node_id != crate::nodes::LOCAL_NODE_ID;
    if !leased {
        return Ok(false);
    }
    let lease_ms = owned.lease_duration_ms.unwrap_or(60000).min(300000);
    let deadline = s
        .node_lease_deadlines
        .lock()
        .await
        .get(attempt)
        .copied()
        .unwrap_or_else(|| s.started + Duration::from_millis(lease_ms + 3000));
    Ok(tokio::time::Instant::now() >= deadline + Duration::from_secs(20))
}

pub async fn session(s: &Service, run: &Value, home: &Path, cwd: &Path) -> Result<String> {
    Provider::of_run(run)?
        .adapter()
        .resume_session(s, run, home, cwd)
        .await
}
