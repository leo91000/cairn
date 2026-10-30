use super::{
    Admission, Worker, audit,
    checkpoint::{RunCheckpoint, Settled},
};
use crate::{
    accounts::Lease,
    config::now,
    error::{Error, Result},
    execution,
    provider::Provider,
    recovery,
    run_status::RunStatus,
    service::{Service, run_projects},
    store::{Db, Store},
    validation::text,
};
use cairn_contracts::EventKind;
use cairn_contracts::RunState;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MAINTENANCE_INTERVAL_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
const RECOVERY_WAIT_REASON: &str = "Waiting for the previous execution to stop before recovery.";
/// Starts the reason of a run that stays queued until a node has room for it.
pub(super) const CAPACITY_WAIT_REASON: &str = "Waiting for capacity.";

impl Worker {
    pub async fn tick(self: &Arc<Self>, s: &Arc<Service>) -> Result<()> {
        let Ok(_tick) = self.tick_lock.try_lock() else {
            return Ok(());
        };
        if s.shutdown.is_cancelled() {
            return Ok(());
        }
        self.initialize(s).await?;
        self.maintain(s).await?;
        let active = self
            .active
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        crate::chats::coding_agent::tick(s, &active).await?;
        // Parked missions must release any pre-crash execution independently
        // of their wakeup and of admission capacity for new processes.
        for state in s.store.read(|db| db.active_run_states()).await? {
            let run_id = state.id.as_str();
            if state.status == RunStatus::Waiting
                && state.recovery_pending
                && !active.contains(run_id)
            {
                let preparation = self.run_preparation_lock(run_id).await;
                let _preparation = preparation.lock().await;
                recover_or_wait(s, &s.store.run(run_id).await?).await?;
            }
        }
        crate::waits::tick_groups(s, &active).await?;
        s.chat_tick(&active).await?;
        crate::waits::tick_missions(s, &active).await?;
        if s.shutdown.is_cancelled() || s.store.kv("deployment-lease").await?.is_some() {
            return Ok(());
        }
        let mut projects = HashSet::new();
        for id in &active {
            projects.extend(locked_projects(s, &s.store.run(id).await?));
        }
        for record in s.store.read(|db| db.queued_run_records()).await? {
            let state = record.state.clone();
            let run = record.wire()?;
            if !execution::uses_vm(&run, &s.config) {
                let active_ids = self.active.lock().await.keys().cloned().collect::<Vec<_>>();
                let mut host_runs = 0;
                for id in active_ids {
                    if !execution::uses_vm(&s.store.run(&id).await?, &s.config) {
                        host_runs += 1;
                    }
                }
                if host_runs >= s.config.concurrency {
                    continue;
                }
            }
            self.try_launch(s, run, state, &mut projects).await?;
        }
        Ok(())
    }

    /// Hourly pruning of expired keys and old audit/event history.
    async fn maintain(&self, s: &Service) -> Result<()> {
        if now() - self.maintenance.load(Ordering::Relaxed) <= MAINTENANCE_INTERVAL_MS {
            return Ok(());
        }
        s.store
            .write(|db| {
                db.0.execute("DELETE FROM kv WHERE expires IS NOT NULL AND expires<=?", [now()])?;
                db.0.execute("DELETE FROM audit WHERE created_at<?", [now() - 90 * DAY_MS])?;
                db.0.execute("DELETE FROM records WHERE kind='node-exclusive-requests' AND json_extract(data,'$.state') IN ('released','failed','expired') AND json_extract(data,'$.createdAt')<?", [now() - 30 * DAY_MS])?;
                db.0.execute("DELETE FROM events WHERE created_at<? AND run_id IN (SELECT id FROM runs WHERE status NOT IN ('queued','running','waiting') AND trigger!='chat')", [now() - 30 * DAY_MS])?;
                Ok(())
            })
            .await?;
        self.maintenance.store(now(), Ordering::Relaxed);
        Ok(())
    }

    /// Starts `run` when it is queued, its projects are free and an account is available.
    async fn try_launch(
        self: &Arc<Self>,
        s: &Arc<Service>,
        run: Value,
        state: RunState,
        projects: &mut HashSet<String>,
    ) -> Result<()> {
        let run_id = text(&run, "id").to_owned();
        if state.status != RunStatus::Queued
            || locked_projects(s, &run).any(|project| projects.contains(&project))
        {
            return Ok(());
        }

        // Preparation can leave the durable status queued until the launch completes.
        // Its existing reservation must keep owning the execution and its stop token.
        if self.active.lock().await.contains_key(&run_id) {
            return Ok(());
        }

        if state.cancel_requested_at.is_some() {
            let pending_id = run_id.clone();
            if s.store
                .read(move |db| crate::chats::coding_agent::pending_for_run(db, &pending_id))
                .await?
            {
                return Ok(());
            }
        }
        let run_preparation = self.run_preparation_lock(&run_id).await;
        let _run_preparation = run_preparation.lock_owned().await;
        let current_id = run_id.clone();
        let Some(record) = s.store.read(move |db| db.run_record(&current_id)).await? else {
            return Ok(());
        };

        let state = record.state.clone();
        let run = record.wire()?;
        let state = &state;
        let recovering = state.recovery_pending;
        let launched = RunCheckpoint::load(&s.store, &run_id)
            .await?
            .is_some_and(|checkpoint| checkpoint.launched());
        // A recovering run may still own its workspace, so keep it reserved even
        // when it cannot start in this pass.
        if recovering || launched {
            projects.extend(locked_projects(s, &run));
        }
        if recovering && !recover_or_wait(s, &run).await? {
            return Ok(());
        }
        let eligibility_id = run_id.clone();
        if !s
            .store
            .read(move |db| eligible_run(db, &eligibility_id))
            .await?
        {
            return Ok(());
        }
        let current = s.store.run_state(&run_id).await?;
        if current.status != RunStatus::Queued
            || crate::run_retry::Retry::waiting(current.retry.as_ref(), now())
        {
            return Ok(());
        }
        if super::retry::expire(s, &run_id).await? {
            return Ok(());
        }
        // Serialize only the preparation window, until placement has reserved
        // the node slot. Running VMs use node slots without a global ceiling.
        let preparation = if execution::uses_vm(&run, &s.config) {
            let Ok(guard) = self.preparation.clone().try_lock_owned() else {
                tracing::info!(
                    target: "cairn_performance",
                    operation = "manager_admission",
                    id = crate::performance::identity(&run_id),
                    phase = "preparation_lock",
                    event = "deferred"
                );
                return Ok(());
            };
            Some(guard)
        } else {
            None
        };
        let mut timing =
            crate::performance::Operation::new("manager_admission", &run_id, "capacity_check");
        // Waiting here keeps the account free and retries on every tick.
        if execution::uses_vm(&run, &s.config) {
            let current = s.store.run(&run_id).await?;
            if let Err(error) = crate::nodes::placement::check(s, &current).await
                && error.is_unavailable()
            {
                if crate::nodes::placement::is_no_capacity(&error)
                    && crate::nodes::moves::queue_capacity_move(s, &current).await?
                {
                    return Ok(());
                }
                return report_capacity_wait(s, &s.store.run_state(&run_id).await?, &error.message)
                    .await;
            }
        }
        let provider = Provider::of_run(&run)?;
        timing.next("account_acquire");
        let model = text(&run["snapshot"]["agent"], "model");
        let speed = text(&run["snapshot"]["agent"], "speed");
        let account = match s
            .accounts
            .acquire_for(s, &run_id, provider, model, speed)
            .await
        {
            Ok(account) => account,
            Err(error) => return report_account_wait(s, state, provider, &error).await,
        };
        let cancel = CancellationToken::new();
        match self.register(s, &run, cancel.clone()).await {
            Ok(true) => {}
            result => {
                if let Some(account) = account {
                    s.accounts.release(s, &account).await?;
                }
                return result.map(|_| ());
            }
        }
        projects.extend(locked_projects(s, &run));
        timing.next("launch");
        let worker = self.clone();
        let s = s.clone();
        self.tasks.spawn(async move {
            run_to_completion(
                &s,
                run,
                Admission {
                    account,
                    preparation,
                },
                cancel,
            )
            .await;
            worker.active.lock().await.remove(&run_id);
            worker.notify();
        });
        timing.finish();
        Ok(())
    }

    /// Rechecks persisted eligibility under the cancellation/cleanup lock before admission.
    async fn register(
        &self,
        s: &Service,
        prepared: &Value,
        cancel: CancellationToken,
    ) -> Result<bool> {
        let run_id = text(prepared, "id");
        let mut active = self.active.lock().await;
        if s.shutdown.is_cancelled() || active.contains_key(run_id) {
            return Ok(false);
        }
        let id = run_id.to_owned();
        let expected_agent = prepared["snapshot"]["agent"].clone();
        let expected_message = crate::waits::execution_id(prepared).clone();
        let eligible = s
            .store
            .transaction(move |db| {
                if !eligible_run(db, &id)? || super::retry::expire_in(db, &id)? {
                    return Ok(false);
                }
                let current = db
                    .run(&id)?
                    .ok_or_else(|| Error::not_found("Run not found"))?;
                // Account preparation can outlive cancellation, merge and resume.
                // Its account and captured execution belong to this exact selection.
                Ok(current["snapshot"]["agent"] == expected_agent
                    && *crate::waits::execution_id(&current) == expected_message)
            })
            .await?;
        if !eligible {
            return Ok(false);
        }
        active.insert(run_id.to_owned(), cancel);
        Ok(true)
    }
}

fn eligible_run(db: &Db<'_>, id: &str) -> Result<bool> {
    let Some(record) = db.run_record(id)? else {
        return Ok(false);
    };
    if record.boundary["backendMigration"].is_object()
        || record.state.status != RunStatus::Queued
        || record.state.cancel_requested_at.is_some()
    {
        return Ok(false);
    }
    if RunCheckpoint::load_in(db, id)?
        .and_then(|checkpoint| checkpoint.capacity_retry_at)
        .is_some_and(|deadline| deadline > now())
    {
        return Ok(false);
    }
    match crate::conversation_lifecycle::require_active_run(db, id) {
        Ok(()) => Ok(true),
        Err(error) if error.is_conflict() => Ok(false),
        Err(error) => Err(error),
    }
}

async fn run_to_completion(
    s: &Arc<Service>,
    run: Value,
    admission: Admission,
    cancel: CancellationToken,
) {
    let run_id = text(&run, "id").to_owned();
    let Err(error) = super::execution::execute(s, run, admission, cancel).await else {
        return;
    };
    // Keep the slot occupied until recovery is durable. A transient
    // disk/database failure must not strand a running record or
    // launch a second process before the previous one is fenced.
    loop {
        let result = match s.accounts.lease(s, &run_id).await {
            Ok(lease) => requeue_after_error(&s.store, &run_id, lease).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => break,
            Err(error) => {
                tracing::warn!(run_id = %run_id, error = %error, "Could not requeue run after a worker error");
            }
        }
        tokio::select! {
            () = s.shutdown.cancelled() => break,
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
    let detail = json!({ "runId": run_id, "message": error.message });
    audit(&s.store, "worker.execution_failed", detail).await;
}

/// Projects whose checkouts a run holds on this host; VM runs work on their own disk.
fn locked_projects(s: &Service, run: &Value) -> impl Iterator<Item = String> {
    let projects = if execution::uses_vm(run, &s.config) {
        Vec::new()
    } else {
        run_projects(run)
    };
    projects
        .into_iter()
        .map(|project| text(&project, "id").to_owned())
}

async fn report_account_wait(
    s: &Service,
    run: &RunState,
    provider: Provider,
    error: &Error,
) -> Result<()> {
    if run.account_wait_reason.as_deref() == Some(error.message.as_str()) {
        return Ok(());
    }
    let run_id = &run.id;
    // Clients offer Connections when the wait needs the user.
    let required = s
        .accounts
        .needs_attention(s, provider)
        .await?
        .then_some(provider);
    let patch = crate::runs::RunChanges {
        account_wait_reason: Some(Some((error.message).to_owned())),
        account_required: Some(serde_json::from_value(serde_json::json!(required))?),
        ..Default::default()
    };
    let (run_id, message) = (run_id.to_owned(), error.message.clone());
    s.store
        .transaction(move |db| {
            db.change_run(&run_id, patch)?;
            db.event(&run_id, &EventKind::Status, &message, None)
        })
        .await
}

/// Shows why `run` waits for a node. Updated figures replace the reason
/// without adding another status event.
pub(super) async fn report_capacity_wait(s: &Service, run: &RunState, detail: &str) -> Result<()> {
    let reason = format!("{CAPACITY_WAIT_REASON} {detail}");
    let previous = run.account_wait_reason.as_deref().unwrap_or_default();
    if previous == reason {
        return Ok(());
    }
    let run_id = &run.id;
    let patch = crate::runs::RunChanges {
        account_wait_reason: Some(Some((reason).to_owned())),
        account_required: Some(None),
        ..Default::default()
    };
    let changed_figures = previous.starts_with(CAPACITY_WAIT_REASON);
    let run_id = run_id.to_owned();
    s.store
        .transaction(move |db| {
            let current = db
                .run_state(&run_id)?
                .ok_or_else(|| Error::not_found("Run not found"))?;
            if current.cancel_requested_at.is_some() || current.status != RunStatus::Queued {
                return Ok(());
            }
            db.change_run(&run_id, patch)?;
            if !changed_figures {
                db.event(&run_id, &EventKind::Status, &reason, None)?;
            }
            Ok(())
        })
        .await
}

/// Returns whether a recovering run may launch again now.
async fn recover_or_wait(s: &Service, run: &Value) -> Result<bool> {
    if let Ok(ready) = recover(s, run).await {
        return Ok(ready);
    }
    if run["accountWaitReason"] != RECOVERY_WAIT_REASON
        && !text(run, "accountWaitReason").starts_with(CAPACITY_WAIT_REASON)
    {
        let patch = crate::runs::RunChanges {
            account_wait_reason: Some(Some((RECOVERY_WAIT_REASON).to_owned())),
            ..Default::default()
        };
        s.store.change_run(text(run, "id"), patch).await?;
    }
    Ok(false)
}

// A provider retry is paid by Retry::next, not by the controller recovery
// budget. Bind that authorization to the attempt and its confirmed cause.
pub(super) fn provider_retry_paid(saved: &RunCheckpoint, state: &RunState) -> bool {
    saved.turn_pending != Some(true)
        && saved.has_provider_retry_reservation()
        && state.retry.as_ref().is_some_and(|retry| {
            retry.next_attempt_at.is_some()
                && saved.retry_cause.as_ref().and_then(Option::as_ref) == Some(&retry.cause)
        })
}

fn controller_budget_exhausted(saved: &RunCheckpoint, state: &RunState, run: &Value) -> bool {
    if !saved.managed() || saved.terminal_code() == Some(0) || provider_retry_paid(saved, state) {
        return false;
    }
    // A confirmed provider ending has its own retry budget. This only permits
    // fencing and settlement; finish_recovery still reserves the retry before admission.
    if saved.finalization_error().is_none()
        && let Some(cause) = saved.confirmed_provider_failure()
        && (state.session_id.is_some() || cause.startup)
        && crate::run_retry::Retry::next(state.retry.as_ref(), cause.clone(), now()).is_some()
    {
        return false;
    }
    let count = saved.controller_recoveries();
    if count < super::execution::MAX_CONTROLLER_RECOVERIES {
        return false;
    }
    let refunded_capacity = saved.start_rejected == Some(true)
        && saved.has_controller_recovery_reservation()
        && saved.pending_paid_admission.as_ref().is_some_and(|paid| {
            paid.controller
                && paid.capacity_refused
                && saved.runner_id() == Some(paid.attempt.as_str())
        });
    if refunded_capacity {
        return false;
    }
    if saved.has_controller_recovery_reservation()
        && saved.runner_id() == saved.move_recovery_reservation.as_deref()
    {
        return false;
    }
    let exception = (saved
        .prepared()
        .is_some_and(|prepared| prepared["backend"] == "native")
        && run["nativeModeInterruption"].is_string())
        || run["moveRequest"].is_object();
    if exception && saved.has_controller_recovery_reservation() {
        return false;
    }
    !(count == super::execution::MAX_CONTROLLER_RECOVERIES
        && saved.has_controller_recovery_reservation())
}

fn recovery_cannot_replay(
    checkpoint: Option<&RunCheckpoint>,
    recovery_deadline: Option<i64>,
    state: &RunState,
    run: &Value,
) -> bool {
    checkpoint.is_some_and(|saved| {
        recovery_deadline.is_some_and(|deadline| now() >= deadline)
            || matches!(saved.remaining_ms, Some(Some(i64::MIN..=0)))
            || saved.finalization_error().is_some()
            || saved.output_error().is_some()
            || (saved.managed() && saved.completed() && saved.terminal_code() == Some(0))
            || saved.settled().is_some_and(|settled| {
                matches!(settled.status, RunStatus::Failed | RunStatus::Cancelled)
            })
            || controller_budget_exhausted(saved, state, run)
    })
}

// Persist the controller's cumulative pause counter with the deadline it
// extends. Both writes are atomic, so a restart cannot credit the same pause
// twice. Never apply a response to a replacement execution.
async fn refresh_recovery_pause(
    s: &Service,
    run_id: &str,
    expected: Option<&RunCheckpoint>,
) -> Result<Option<i64>> {
    let Some(saved) = expected.filter(|saved| saved.managed()) else {
        return Ok(None);
    };
    let Some(attempt) = saved.runner_id() else {
        return Ok(None);
    };
    let total = crate::nodes::exclusive::administrative_pause_for_node(
        s,
        saved.node_id.as_ref().and_then(|node| node.as_deref()),
        attempt,
    )
    .await;
    let id = run_id.to_owned();
    let expected = expected.cloned();
    let attempt = attempt.to_owned();
    s.store
        .transaction(move |db| {
            let Some(mut current) = RunCheckpoint::load_in(db, &id)? else {
                return Ok(None);
            };
            if !recovery::same_execution(Some(&current), expected.as_ref()) {
                return Ok(None);
            }
            if let (Ok(total), Some(deadline)) = (total, current.recovery_deadline_ms) {
                let previous = current
                    .administrative_pause_observation
                    .as_ref()
                    .filter(|observation| observation.attempt == attempt)
                    .map_or(0, |observation| observation.total_ms);
                if total > previous {
                    let credit = total.saturating_sub(previous).min(i64::MAX as u64) as i64;
                    current.recovery_deadline_ms = Some(deadline.saturating_add(credit));
                    current.administrative_pause_observation =
                        Some(super::checkpoint::AdministrativePauseObservation {
                            attempt,
                            total_ms: total,
                        });
                }
            }
            if let Some(deadline) = current.recovery_deadline_ms {
                current.remaining_ms = Some(Some(deadline.saturating_sub(now()).max(0)));
                current.store_in(db, &id)?;
            }
            Ok(current.recovery_deadline_ms)
        })
        .await
}

/// Fences the previous execution, then either settles the run from its
/// checkpoint or clears it for relaunch (`true`).
async fn recover(s: &Service, run: &Value) -> Result<bool> {
    let run_id = text(run, "id");
    let mut checkpoint = RunCheckpoint::load(&s.store, run_id).await?;
    let initial_state = s.store.run_state(run_id).await?;
    let parked = initial_state.status == RunStatus::Waiting
        || (initial_state.trigger != "chat"
            && run["wakeup"].is_object()
            && checkpoint.as_ref().is_some_and(RunCheckpoint::completed));
    // A newly queued turn has not spent any of its active execution budget yet.
    let new_turn = checkpoint
        .as_ref()
        .is_some_and(|saved| saved.turn_pending == Some(true));
    let expected_checkpoint = checkpoint.clone();
    let mut recovery_deadline = if !parked && !new_turn {
        let id = run_id.to_owned();
        let expected = expected_checkpoint.clone();
        s.store
            .transaction(move |db| {
                let Some(mut saved) = RunCheckpoint::load_in(db, &id)? else {
                    return Ok(None);
                };
                if !recovery::same_execution(Some(&saved), expected.as_ref()) {
                    return Ok(None);
                }
                let deadline = saved
                    .recovery_deadline_ms
                    .or_else(|| match saved.remaining_ms {
                        Some(Some(remaining)) => Some(now().saturating_add(remaining)),
                        Some(None) | None => None,
                    });
                if saved.recovery_deadline_ms != deadline {
                    saved.recovery_deadline_ms = deadline;
                    saved.store_in(db, &id)?;
                }
                Ok(deadline)
            })
            .await?
    } else {
        None
    };
    if recovery_deadline.is_some() {
        recovery_deadline = refresh_recovery_pause(s, run_id, expected_checkpoint.as_ref()).await?;
        checkpoint = RunCheckpoint::load(&s.store, run_id).await?;
    }
    let state = s.store.run_state(run_id).await?;
    if state.cancel_requested_at.is_none()
        && let Some(saved) = checkpoint.as_mut()
        && saved.managed()
        && saved.start_rejected != Some(true)
        && saved.settled().is_none()
        && saved.terminal_code.is_none()
        && !matches!(saved.remaining_ms, Some(Some(i64::MIN..=0)))
        && let Some(attempt) = saved.runner_id().map(str::to_owned)
    {
        let observation_limit = recovery_deadline.map_or(Duration::from_secs(5), |deadline| {
            Duration::from_millis(deadline.saturating_sub(now()).max(0) as u64)
                .min(Duration::from_secs(5))
        });
        recovery::terminal_code(
            s,
            &attempt,
            saved.node_id.as_ref().and_then(|node| node.as_deref()),
            &CancellationToken::new(),
            observation_limit,
            |action| {
                let id = run_id.to_owned();
                let attempt = attempt.clone();
                let expected = expected_checkpoint.clone();
                let shutdown = s.shutdown.clone();
                async move {
                    s.store
                        .transaction(move |db| {
                            let state = db
                                .run_state(&id)?
                                .ok_or_else(|| Error::not_found("Run not found"))?;
                            if state.cancel_requested_at.is_some()
                                && matches!(action, recovery::ObservationAction::Reserve)
                            {
                                return Ok(false);
                            }
                            let Some(mut current) = RunCheckpoint::load_in(db, &id)? else {
                                return Ok(false);
                            };
                            if !recovery::same_execution(Some(&current), expected.as_ref()) {
                                return Ok(false);
                            }
                            let mut reserved = false;
                            match action {
                                recovery::ObservationAction::Reserve => {
                                    if recovery_deadline.is_none_or(|deadline| now() < deadline) {
                                        reserved = current.reserve_observation(&attempt);
                                    }
                                }
                                recovery::ObservationAction::Release => {
                                    current.release_observation(&attempt);
                                }
                                recovery::ObservationAction::Exhausted => {
                                    if !shutdown.is_cancelled()
                                        && current.runner_id() == Some(attempt.as_str())
                                        && current.settled().is_none()
                                        && current.terminal_code.is_none()
                                    {
                                        current.release_observation(&attempt);
                                        current.terminal_code = Some(None);
                                    }
                                }
                                recovery::ObservationAction::Record(code) => {
                                    current.release_observation(&attempt);
                                    if current.runner_id() == Some(attempt.as_str())
                                        && current.settled().is_none()
                                        && current.terminal_code.is_none()
                                        && (!shutdown.is_cancelled() || code.is_some())
                                    {
                                        current
                                            .record_controller_result(db, &id, &attempt, code)?;
                                    }
                                }
                            }
                            if let Some(deadline) = recovery_deadline {
                                current.remaining_ms =
                                    Some(Some(deadline.saturating_sub(now()).max(0)));
                            }
                            current.store_in(db, &id)?;
                            Ok(reserved)
                        })
                        .await
                }
            },
        )
        .await?;
        if s.shutdown.is_cancelled() {
            return Err(Error::unavailable("Worker is restarting."));
        }
        checkpoint = RunCheckpoint::load(&s.store, run_id).await?;
    }
    if recovery_deadline.is_some() {
        recovery_deadline = refresh_recovery_pause(s, run_id, expected_checkpoint.as_ref()).await?;
        checkpoint = RunCheckpoint::load(&s.store, run_id).await?;
    }
    if !recovery::same_execution(checkpoint.as_ref(), expected_checkpoint.as_ref()) {
        return Ok(false);
    }
    let parked = state.status == RunStatus::Waiting
        || (state.trigger != "chat"
            && run["wakeup"].is_object()
            && checkpoint.as_ref().is_some_and(RunCheckpoint::completed));
    // A parked turn only needs fencing/release, including a completed turn
    // requeued because its first fencing attempt failed. Placement and possible
    // disk relocation belong to admission after its wakeup.
    let cannot_replay = recovery_cannot_replay(checkpoint.as_ref(), recovery_deadline, &state, run);
    // A move requested after a conversation has already succeeded is a disk
    // transfer, not admission of another turn. Its idle marker is recorded by
    // the public request while the run is succeeded; active-turn moves lack it.
    let finished_idle_move = run["moveRequest"]["idle"] == true
        && run["moveRequest"]["automatic"] == false
        && state.cancel_requested_at.is_none()
        && checkpoint.as_ref().is_some_and(|saved| {
            saved.managed()
                && saved.completed()
                && saved.terminal_code() == Some(0)
                && saved.finalization_error().is_none()
                && saved.output_error().is_none()
                && !saved.settled().is_some_and(|settled| {
                    matches!(settled.status, RunStatus::Failed | RunStatus::Cancelled)
                })
        });
    if !parked
        && (!cannot_replay || finished_idle_move)
        && !crate::nodes::moves::advance_for_attempt(s, run, expected_checkpoint.as_ref()).await?
    {
        return Ok(false);
    }
    let fenced = recovery::fence_attempt(s, run, expected_checkpoint.as_ref()).await;
    let mut expected_fenced = expected_checkpoint.clone();
    if fenced.is_ok()
        && let Some(saved) = &mut expected_fenced
    {
        saved.process = None;
    }
    if recovery_deadline.is_some() {
        let expected = if fenced.is_ok() {
            expected_fenced.as_ref()
        } else {
            expected_checkpoint.as_ref()
        };
        recovery_deadline = refresh_recovery_pause(s, run_id, expected).await?;
    }
    if let Some(deadline) = recovery_deadline {
        let id = run_id.to_owned();
        let expected = if fenced.is_ok() {
            expected_fenced.clone()
        } else {
            expected_checkpoint.clone()
        };
        s.store
            .transaction(move |db| {
                let mut current = RunCheckpoint::load_in(db, &id)?;
                if recovery::same_execution(current.as_ref(), expected.as_ref())
                    && let Some(saved) = &mut current
                {
                    saved.remaining_ms = Some(Some(deadline.saturating_sub(now()).max(0)));
                    saved.store_in(db, &id)?;
                }
                Ok(())
            })
            .await?;
    }
    fenced?;
    let current = RunCheckpoint::load(&s.store, run_id).await?;
    if !recovery::same_execution(current.as_ref(), expected_fenced.as_ref()) {
        return Ok(false);
    }
    let current_state = s.store.run_state(run_id).await?;
    let current_run = s.store.run(run_id).await?;
    let cannot_replay_after_fence = recovery_cannot_replay(
        current.as_ref(),
        recovery_deadline,
        &current_state,
        &current_run,
    );
    if cannot_replay_after_fence && !parked {
        crate::nodes::moves::abort_terminal_recovery(s, run_id, expected_fenced.as_ref()).await?;
    } else if cannot_replay
        && !parked
        && !crate::nodes::moves::advance_for_attempt(s, run, expected_fenced.as_ref()).await?
    {
        return Ok(false);
    }
    let id = run_id.to_owned();
    let expected = expected_fenced.clone();
    let revoked = s
        .store
        .transaction(move |db| {
            let current = RunCheckpoint::load_in(db, &id)?;
            if !recovery::same_execution(current.as_ref(), expected.as_ref()) {
                return Ok(false);
            }
            db.revoke_mcp_run(&id)?;
            Ok(true)
        })
        .await?;
    if !revoked {
        return Ok(false);
    }
    let current = RunCheckpoint::load(&s.store, run_id).await?;
    if !recovery::same_execution(current.as_ref(), expected_fenced.as_ref()) {
        return Ok(false);
    }
    s.accounts.recover_run(s, run).await?;
    let id = run_id.to_owned();
    s.store
        .transaction(move |db| {
            let mut checkpoint = RunCheckpoint::load_in(db, &id)?;
            if !recovery::same_execution(checkpoint.as_ref(), expected_fenced.as_ref()) {
                return Ok(false);
            }
            if let (Some(saved), Some(deadline)) = (&mut checkpoint, recovery_deadline) {
                saved.remaining_ms = Some(Some(deadline.saturating_sub(now()).max(0)));
                saved.store_in(db, &id)?;
            }
            finish_recovery(db, &id, checkpoint.as_ref())
        })
        .await
}

/// Cancellation and the final recovery state commit together. In particular,
/// a completed provider turn must never restore a wait cancellation removed.
pub(super) fn finish_recovery(
    db: &Db<'_>,
    id: &str,
    checkpoint: Option<&RunCheckpoint>,
) -> Result<bool> {
    let record = db
        .run_record(id)?
        .ok_or_else(|| Error::not_found("Run not found"))?;
    let current = &record.state;
    if current.cancel_requested_at.is_some() {
        cancel_recovery(db, id, current)?;
        return Ok(false);
    }
    if let Some(checkpoint) = checkpoint {
        if let Some(settled) = checkpoint.settled() {
            let mut patch = settled.run_changes();
            if settled.status == RunStatus::Waiting {
                patch.resume_available = Some(true);
                patch.boundary = Some(json!({"outcome": null}));
            }
            db.change_run(id, patch)?;
            return Ok(false);
        }
        if matches!(checkpoint.remaining_ms, Some(Some(i64::MIN..=0))) {
            let settled = Settled::failure(RunStatus::Failed, "Run exceeded its time limit.");
            let mut patch = settled.run_changes();
            patch.wakeup = Some(None);
            db.change_run(id, patch)?;
            return Ok(false);
        }
        if let Some(error) = checkpoint
            .finalization_error()
            .or_else(|| checkpoint.output_error())
        {
            let settled = Settled::failure(RunStatus::Failed, error);
            let mut patch = settled.run_changes();
            patch.wakeup = Some(None);
            db.change_run(id, patch)?;
            return Ok(false);
        }
        if let Some(cause) = checkpoint.confirmed_provider_failure()
            && !provider_retry_paid(checkpoint, current)
        {
            let mut paid = checkpoint.clone();
            if (current.session_id.is_some() || cause.startup)
                && super::retry::schedule_in(db, id, &mut paid, current, cause.clone())?
            {
                paid.store_in(db, id)?;
            } else {
                let settled = Settled::failure(RunStatus::Failed, &cause.message);
                db.change_run(id, settled.run_changes())?;
            }
            return Ok(false);
        }
        if checkpoint.terminal_code() == Some(0) || checkpoint.completed() {
            let summary = match checkpoint.last_message() {
                "" => {
                    "Conversation completed before worker restart. See Activity for the recorded result."
                }
                message => message,
            };
            let waiting = current.trigger != "chat" && record.wakeup.is_some();
            if waiting && current.session_id.is_none() {
                db.change_run(id,crate::runs::RunChanges {
                    status: Some(RunStatus::Failed),
                    finished_at: Some(Some(now())),
                    recovery_pending: Some(false),
                    account_wait_reason: Some(None),
                    error: Some(Some("Cannot wait without a saved agent session. Working files were preserved.".into())),
                    wakeup: Some(None),
                    ..Default::default()
                })?;
                return Ok(false);
            }
            let mut patch = crate::runs::RunChanges {
                status: Some(if waiting {
                    RunStatus::Waiting
                } else {
                    RunStatus::Succeeded
                }),
                finished_at: Some(if waiting { None } else { Some(now()) }),
                resume_available: Some(waiting),
                account_wait_reason: Some(None),
                recovery_pending: Some(false),
                error: Some(None),
                retry: Some(None),
                summary: Some((summary).to_owned()),
                boundary: Some(json!({})),
                ..Default::default()
            };
            if waiting {
                patch.boundary.as_mut().unwrap()["outcome"] = Value::Null;
            } else {
                // A compaction completed before the restart settles like one that did not.
                crate::chats::keep_reply_after_compaction(&record.boundary, &mut patch)?;
            }
            db.change_run(id, patch)?;
            return Ok(false);
        }
        if controller_budget_exhausted(checkpoint, current, &record.boundary) {
            let expired = matches!(checkpoint.remaining_ms, None | Some(Some(i64::MIN..=0)));
            let message = if expired {
                "Run exceeded its time limit."
            } else {
                "Execution recovery budget exhausted. Working files were preserved."
            };
            let settled = Settled::failure(RunStatus::Failed, message);
            let mut patch = settled.run_changes();
            patch.wakeup = Some(None);
            db.change_run(id, patch)?;
            return Ok(false);
        }
    }
    if let Some(saved) = checkpoint
        && saved.managed()
        && saved.start_rejected != Some(true)
        && saved.runner_id().is_some()
        && saved.turn_pending != Some(true)
        && !saved.has_controller_recovery_reservation()
        && !provider_retry_paid(saved, current)
    {
        let mut paid = saved.clone();
        paid.controller_recoveries = Some(saved.controller_recoveries() + 1);
        paid.controller_recovery_reservation = saved.runner_id().map(str::to_owned);
        paid.store_in(db, id)?;
    }
    db.change_run(
        id,
        crate::runs::RunChanges {
            recovery_pending: Some(false),
            ..Default::default()
        },
    )?;
    Ok(true)
}

/// Settles a recovering run whose cancellation was requested. One waiting for an automatic
/// retry shows the stop in its conversation; a wait's own card already shows its cancellation.
fn cancel_recovery(db: &Db<'_>, id: &str, run: &RunState) -> Result<()> {
    db.change_run(
        id,
        crate::runs::RunChanges {
            status: Some(RunStatus::Cancelled),
            finished_at: Some(Some(now())),
            account_wait_reason: Some(None),
            recovery_pending: Some(false),
            retry: Some(None),
            wakeup: Some(None),
            ..Default::default()
        },
    )?;
    if run
        .retry
        .as_ref()
        .and_then(|retry| retry.next_attempt_at)
        .is_some()
    {
        db.event(id, &EventKind::Status, RunStatus::Cancelled.as_str(), None)?;
    }
    Ok(())
}

/// Hands a run whose execution failed unexpectedly back to recovery.
async fn requeue_after_error(store: &Store, run_id: &str, lease: Option<Lease>) -> Result<()> {
    let id = run_id.to_owned();
    store
        .transaction(move |db| {
            let Some(current) = db.run_state(&id)? else {
                return Ok(());
            };
            let mut checkpoint =
                RunCheckpoint::load_in(db, &id)?.unwrap_or_else(RunCheckpoint::exhausted);
            if !current.status.is_active() {
                checkpoint.settled = Some(Some(Settled::from_state(&current)));
            }
            checkpoint.store_in(db, &id)?;
            let mut patch = crate::runs::RunChanges {
                status: Some(RunStatus::Queued),
                recovery_pending: Some(true),
                finished_at: Some(None),
                account_wait_reason: Some(Some("Recovering after a worker error.".into())),
                ..Default::default()
            };
            if let Some(lease) = lease {
                patch.account_id = Some(Some((lease.account_id).to_owned()));
            }
            db.change_run(&id, patch)?;
            Ok(())
        })
        .await
}

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests {
    use crate::account_fixture::AccountFixtures;

    use super::*;
    use crate::agent_fixture::AgentFixture;
    use std::{future::Future, task::Poll};

    async fn fixture() -> (
        tempfile::TempDir,
        Arc<Service>,
        String,
        String,
        std::path::PathBuf,
    ) {
        fixture_with_runner(String::new()).await
    }

    async fn fixture_with_runner(
        runner_url: String,
    ) -> (
        tempfile::TempDir,
        Arc<Service>,
        String,
        String,
        std::path::PathBuf,
    ) {
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
            runner_url,
        })
        .await
        .unwrap();
        let mut chat = service.chat_create(json!({})).await.unwrap();
        let chat_id = text(&chat, "id").to_owned();
        let run_id = crate::config::id();
        service.store.transaction({
            let run_id = run_id.clone();
            move |db| {
                let run = json!({
                    "id": run_id,
                    "taskId": run_id,
                    "status": RunStatus::Queued,
                    "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID, "provider": "codex" } },
                });
                db.add_run_fixture(&run, None)?;
                Ok(())
            }
        }).await.unwrap();
        chat["runId"] = run_id.clone().into();
        service
            .store
            .save_conversation_boundary(chat)
            .await
            .unwrap();
        let workspace = service.config.data_dir.join("runs").join(&run_id);
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("work.txt"), "Private work")
            .await
            .unwrap();

        (root, service, chat_id, run_id, workspace)
    }

    #[tokio::test]
    async fn shutdown_during_active_observation_keeps_pending_checkpoint() {
        use axum::{Router, routing::any};
        let entered = Arc::new(tokio::sync::Notify::new());
        let seen = entered.clone();
        let app = Router::new().fallback(any(move || {
            let seen = seen.clone();
            async move {
                seen.notify_one();
                std::future::pending::<axum::http::StatusCode>().await
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "old-attempt", "completed": true, "controllerRecoveries": 3,
            "remainingMs": null,
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        let checkpoint = Arc::new(super::super::checkpoint::Checkpoint::new(
            service.store.clone(),
            run.clone(),
            saved,
        ));
        let (s, cp) = (service.clone(), checkpoint.clone());
        let task = tokio::spawn(async move {
            super::super::execution::reconcile(&s, &cp, &CancellationToken::new()).await
        });
        entered.notified().await;
        service.shutdown.cancel();
        task.await.unwrap().unwrap();
        let saved = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert!(saved.terminal_code.is_none());
        let observation = saved.controller_observation.unwrap();
        assert_eq!(observation.requests, 1);
        assert!(!observation.in_flight);
        server.abort();
    }

    #[tokio::test]
    async fn active_observation_cannot_overwrite_replacement() {
        use axum::{Router, routing::any};
        let entered = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let app = Router::new().fallback(any({
            let (entered, gate) = (entered.clone(), gate.clone());
            move || {
                let (entered, gate) = (entered.clone(), gate.clone());
                async move {
                    entered.notify_one();
                    gate.acquire().await.unwrap().forget();
                    axum::Json(json!({ "StatusCode": 0 }))
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let original = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "old-attempt", "remainingMs": null,
        }))
        .unwrap();
        original.store(&service.store, &run).await.unwrap();
        let checkpoint = Arc::new(super::super::checkpoint::Checkpoint::new(
            service.store.clone(),
            run.clone(),
            original.clone(),
        ));
        let (s, cp) = (service.clone(), checkpoint.clone());
        let task = tokio::spawn(async move {
            super::super::execution::reconcile(&s, &cp, &CancellationToken::new()).await
        });
        entered.notified().await;
        let mut replacement = original;
        replacement.runner_id = Some(Some("new-attempt".into()));
        replacement.controller_observation = None;
        replacement.store(&service.store, &run).await.unwrap();
        gate.add_permits(1);
        assert!(task.await.unwrap().is_err());
        let current = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.runner_id(), Some("new-attempt"));
        assert!(current.terminal_code.is_none());
        assert!(current.controller_observation.is_none());
        service.shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn interrupted_observations_keep_a_durable_per_attempt_limit() {
        use axum::{Router, extract::Request, response::IntoResponse, routing::any};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let entered = Arc::new(tokio::sync::Notify::new());
        let reads = Arc::new(AtomicUsize::new(0));
        let (seen, count) = (entered.clone(), reads.clone());
        let app = Router::new().fallback(any(move |request: Request| {
            let (seen, count) = (seen.clone(), count.clone());
            async move {
                if request.method() == "DELETE" {
                    return axum::http::StatusCode::NO_CONTENT.into_response();
                }
                count.fetch_add(1, Ordering::SeqCst);
                seen.notify_one();
                std::future::pending::<axum::response::Response>().await
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "old-attempt", "completed": true, "controllerRecoveries": 3,
            "remainingMs": null,
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        let (s, id) = (service.clone(), run.clone());
        let task = tokio::spawn(async move { recover(&s, &s.store.run(&id).await.unwrap()).await });
        entered.notified().await;
        task.abort();
        let _ = task.await;
        let saved = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert!(saved.terminal_code.is_none());
        let observation = saved.controller_observation.unwrap();
        assert_eq!(observation.requests, 1);
        assert!(observation.in_flight);
        assert!(
            !recover(&service, &service.store.run(&run).await.unwrap())
                .await
                .unwrap()
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(
            service.store.run_state(&run).await.unwrap().status,
            RunStatus::Failed
        );
        service.shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn recovery_deadline_stops_further_controller_observations() {
        use axum::{Router, extract::Request, response::IntoResponse, routing::any};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        let app = Router::new().fallback(any(move |request: Request| {
            let count = count.clone();
            async move {
                if request.method() == "DELETE" {
                    return axum::http::StatusCode::NO_CONTENT.into_response();
                }
                if request.uri().path().ends_with("/administrative-pause") {
                    return axum::http::StatusCode::NOT_FOUND.into_response();
                }
                count.fetch_add(1, Ordering::SeqCst);
                std::future::pending::<axum::response::Response>().await
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
            "completed": false, "remainingMs": 200,
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        assert!(
            !recover(&service, &service.store.run(&run).await.unwrap())
                .await
                .unwrap()
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        let state = service.store.run_state(&run).await.unwrap();
        assert_eq!(state.status, RunStatus::Failed);
        assert_eq!(state.error.as_deref(), Some("Run exceeded its time limit."));
        service.shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn recovery_pause_counter_is_durable_monotone_and_attempt_scoped() {
        use axum::{Router, response::IntoResponse, routing::any};
        use std::sync::atomic::{AtomicU64, Ordering};
        let counter = Arc::new(AtomicU64::new(100));
        let value = counter.clone();
        let app = Router::new().fallback(any(move || {
            let value = value.clone();
            async move {
                let total = value.load(Ordering::SeqCst);
                if total == u64::MAX {
                    return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                axum::Json(json!({ "administrativePausedMs": total })).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let deadline = now() + 10_000;
        let mut saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "old-attempt", "remainingMs": 10000,
            "recoveryDeadlineMs": deadline,
            "administrativePauseObservation": { "attempt": "old-attempt", "totalMs": 40 }
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        for (total, expected) in [
            (100, deadline + 60),
            (100, deadline + 60),
            (50, deadline + 60),
            (u64::MAX, deadline + 60),
            (130, deadline + 90),
        ] {
            counter.store(total, Ordering::SeqCst);
            assert_eq!(
                refresh_recovery_pause(&service, &run, Some(&saved))
                    .await
                    .unwrap(),
                Some(expected)
            );
            saved = RunCheckpoint::load(&service.store, &run)
                .await
                .unwrap()
                .unwrap();
        }
        let stale = saved.clone();
        saved.runner_id = Some(Some("new-attempt".into()));
        saved.store(&service.store, &run).await.unwrap();
        counter.store(20, Ordering::SeqCst);
        assert_eq!(
            refresh_recovery_pause(&service, &run, Some(&stale))
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            refresh_recovery_pause(&service, &run, Some(&saved))
                .await
                .unwrap(),
            Some(deadline + 110)
        );
        let reloaded = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            refresh_recovery_pause(&service, &run, Some(&reloaded))
                .await
                .unwrap(),
            Some(deadline + 110)
        );
        service.shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn admin_pause_during_recovery_does_not_spend_budget() {
        use axum::{Router, extract::Request, response::IntoResponse, routing::any};
        let started = std::time::Instant::now();
        let app = Router::new().fallback(any(move |request: Request| async move {
            if request.uri().path().ends_with("/administrative-pause") {
                let paused_ms = started.elapsed().as_millis() as u64;
                return axum::Json(json!({ "administrativePausedMs": paused_ms })).into_response();
            }
            if request.uri().path().ends_with("/wait") {
                return std::future::pending::<axum::response::Response>().await;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            axum::http::StatusCode::NO_CONTENT.into_response()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
            "completed": false, "remainingMs": 200,
            "administrativePauseObservation": { "attempt": "old-attempt", "totalMs": 0 },
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        service
            .store
            .change_run(
                &run,
                crate::runs::RunChanges {
                    session_id: Some(Some("saved-session".into())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let ready = recover(&service, &service.store.run(&run).await.unwrap())
            .await
            .unwrap();
        let state = service.store.run_state(&run).await.unwrap();
        let final_checkpoint = RunCheckpoint::load(&service.store, &run).await.unwrap();
        assert!(ready, "state={state:?} checkpoint={final_checkpoint:?}");
        assert_eq!(state.status, RunStatus::Queued);
        service.shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn failed_fences_spend_the_same_durable_deadline() {
        use axum::{Router, routing::any};
        use std::sync::atomic::{AtomicBool, Ordering};
        let allow = Arc::new(AtomicBool::new(false));
        let enabled = allow.clone();
        let app = Router::new().fallback(any(move || {
            let enabled = enabled.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                if enabled.load(Ordering::SeqCst) {
                    axum::http::StatusCode::NO_CONTENT
                } else {
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "old-attempt", "completed": true, "terminalCode": 0,
            "remainingMs": 350,
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        let mut previous = 350;
        for index in 0..2 {
            assert!(
                recover(&service, &service.store.run(&run).await.unwrap())
                    .await
                    .is_err()
            );
            let saved = RunCheckpoint::load(&service.store, &run)
                .await
                .unwrap()
                .unwrap();
            let remaining = saved.remaining_ms.flatten().unwrap();
            assert!(remaining <= previous && (index != 0 || remaining < previous));
            assert!(saved.recovery_deadline_ms.is_some());
            assert_eq!(
                service.store.run_state(&run).await.unwrap().status,
                RunStatus::Queued
            );
            previous = remaining;
        }
        assert_eq!(previous, 0);
        allow.store(true, Ordering::SeqCst);
        assert!(
            !recover(&service, &service.store.run(&run).await.unwrap())
                .await
                .unwrap()
        );
        let state = service.store.run_state(&run).await.unwrap();
        assert_eq!(state.status, RunStatus::Failed);
        assert_eq!(state.error.as_deref(), Some("Run exceeded its time limit."));
        service.shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn recovery_fences_completed_failed_turn_before_pending_move() {
        use axum::{Router, routing::any};
        let app = Router::new().fallback(any(|| async { axum::http::StatusCode::NO_CONTENT }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        for (terminal_code, idle, cancelled) in [
            (70, false, false),
            (70, true, false),
            (0, false, false),
            (70, true, true),
            (0, true, true),
        ] {
            let saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true },
                "runnerId": "old-attempt", "completed": true, "terminalCode": terminal_code,
                "controllerRecoveries": 3, "remainingMs": null,
            }))
            .unwrap();
            saved.store(&service.store, &run).await.unwrap();
            service
                .store
                .change_run(
                    &run,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        cancel_requested_at: Some(cancelled.then(now)),
                        boundary: Some(json!({ "moveRequest": { "nodeId": "destination", "idle": idle, "automatic": false } })),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert!(
                !recover(&service, &service.store.run(&run).await.unwrap())
                    .await
                    .unwrap()
            );
            let state = service.store.run_state(&run).await.unwrap();
            assert_eq!(
                state.status,
                if cancelled {
                    RunStatus::Cancelled
                } else if terminal_code == 0 {
                    RunStatus::Succeeded
                } else {
                    RunStatus::Failed
                }
            );
            let current = RunCheckpoint::load(&service.store, &run)
                .await
                .unwrap()
                .unwrap();
            assert!(current.completed());
            assert_eq!(current.terminal_code(), Some(terminal_code));
        }
        service.shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn failed_observation_commit_does_not_repeat_network_reads() {
        use axum::{Router, extract::Request, response::IntoResponse, routing::any};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = Arc::new(AtomicUsize::new(0));
        let seen = reads.clone();
        let app = Router::new().fallback(any(move |request: Request| {
            let seen = seen.clone();
            async move {
                if request.method() == "DELETE" {
                    return axum::http::StatusCode::NO_CONTENT.into_response();
                }
                seen.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({ "StatusCode": 0 })).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "old-attempt", "completed": true, "controllerRecoveries": 3,
            "remainingMs": null,
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        service.store.write(|db| {
            db.0.execute_batch("CREATE TRIGGER refuse_checkpoint BEFORE UPDATE ON run_checkpoints WHEN json_extract(OLD.state, '$.controllerObservation.requests') >= 1 BEGIN SELECT RAISE(ABORT, 'fixture write refused'); END")?;
            Ok(())
        }).await.unwrap();
        for _ in 0..2 {
            let _ = recover(&service, &service.store.run(&run).await.unwrap()).await;
        }
        service
            .store
            .write(|db| {
                db.0.execute_batch("DROP TRIGGER refuse_checkpoint")?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            !recover(&service, &service.store.run(&run).await.unwrap())
                .await
                .unwrap()
        );
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "a refused write must stop further observations"
        );
        service.shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn shutdown_during_recovery_observation_preserves_pending_result() {
        use axum::{Router, routing::any};
        let entered = Arc::new(tokio::sync::Notify::new());
        let seen = entered.clone();
        let app = Router::new().fallback(any(move || {
            let seen = seen.clone();
            async move {
                seen.notify_one();
                std::future::pending::<()>().await;
                axum::http::StatusCode::OK
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "old-attempt", "completed": true, "controllerRecoveries": 3,
            "remainingMs": null,
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        let (s, id) = (service.clone(), run.clone());
        let task = tokio::spawn(async move { recover(&s, &s.store.run(&id).await.unwrap()).await });
        entered.notified().await;
        service.shutdown.cancel();
        let _ = task.await.unwrap();
        let saved = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert!(
            saved.terminal_code.is_none(),
            "shutdown must leave observation pending"
        );
        let observation = saved.controller_observation.unwrap();
        assert_eq!(observation.attempt, "old-attempt");
        assert_eq!(observation.requests, 1);
        assert!(!observation.in_flight);
        server.abort();
    }

    #[tokio::test]
    async fn recovered_interruption_respects_controller_recovery_ceiling() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        for recoveries in [2, 3] {
            for code in [75, 143] {
                for (cancel, remaining) in [(false, None), (true, None), (false, Some(0))] {
                    let id = run.clone();
                    service
                        .store
                        .transaction(move |db| {
                            db.change_run(
                                &id,
                                crate::runs::RunChanges {
                                    status: Some(RunStatus::Queued),
                                    cancel_requested_at: Some(cancel.then(now)),
                                    wakeup: Some(Some(serde_json::from_value(json!({
                                        "id": "old",
                                        "messageId": null,
                                        "reason": "Waiting",
                                        "seconds": 3600,
                                        "createdAt": now(),
                                        "wakeAt": now() + 3_600_000,
                                    }))?)),
                                    ..Default::default()
                                },
                            )?;
                            let saved = RunCheckpoint::from_value(json!({
                                "prepared": { "backend": "native", "isolated": true },
                                "runnerId": "old-attempt", "completed": false,
                                "terminalCode": code, "controllerRecoveries": recoveries,
                                "remainingMs": remaining,
                            }))?;
                            saved.store_in(db, &id)?;
                            let may_launch = finish_recovery(db, &id, Some(&saved))?;
                            let persisted = RunCheckpoint::load_in(db, &id)?.unwrap();
                            if cancel || remaining == Some(0) || recoveries == 3 {
                                assert_eq!(persisted.controller_recoveries(), recoveries);
                                assert!(!persisted.has_controller_recovery_reservation());
                            } else {
                                assert_eq!(persisted.controller_recoveries(), recoveries + 1);
                                assert!(persisted.has_controller_recovery_reservation());
                            }
                            assert_eq!(
                                may_launch,
                                recoveries < 3 && !cancel && remaining.is_none(),
                                "code {code}, recoveries {recoveries}"
                            );
                            let current = db.run_state(&id)?.unwrap();
                            if cancel {
                                assert_eq!(current.status, RunStatus::Cancelled);
                            } else if remaining == Some(0) {
                                assert_eq!(current.status, RunStatus::Failed);
                                assert_eq!(
                                    current.error.as_deref(),
                                    Some("Run exceeded its time limit.")
                                );
                            }
                            if !may_launch {
                                assert!(db.run(&id)?.unwrap()["wakeup"].is_null());
                            }
                            Ok(())
                        })
                        .await
                        .unwrap();
                }
            }
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn clean_controller_exit_without_provider_completion_fails_on_restart() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        for backend in ["native", "firecracker", "shared"] {
            for completed in [false, true] {
                let id = run.clone();
                service
                    .store
                    .transaction(move |db| {
                        db.change_run(
                            &id,
                            crate::runs::RunChanges {
                                status: Some(RunStatus::Queued),
                                cancel_requested_at: Some(None),
                                ..Default::default()
                            },
                        )?;
                        let saved = RunCheckpoint::from_value(json!({
                            "prepared": { "backend": backend, "isolated": true },
                            "runnerId": "attempt", "completed": completed,
                            "terminalCode": 0, "remainingMs": null,
                        }))?;
                        assert!(!finish_recovery(db, &id, Some(&saved))?);
                        let state = db.run_state(&id)?.unwrap();
                        assert_eq!(
                            state.status,
                            if completed || backend == "shared" {
                                RunStatus::Succeeded
                            } else {
                                RunStatus::Failed
                            }
                        );
                        if !completed && backend != "shared" {
                            assert!(state.error.unwrap().contains("completion was recorded"));
                        }
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn provider_retry_authorization_is_bound_to_attempt_and_confirmed_cause() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        let cause = crate::run_retry::Cause::classify(&json!({
            "code": "server_overloaded", "message": "Provider busy"
        }))
        .unwrap();
        for (same_attempt, same_cause, pending_turn, cancelled, expired) in [
            (true, true, false, false, false),
            (false, true, false, false, false),
            (true, false, false, false, false),
            (true, true, true, false, false),
            (true, true, false, true, false),
            (true, true, false, false, true),
        ] {
            let (id, cause) = (run.clone(), cause.clone());
            service
                .store
                .transaction(move |db| {
                    let retry = crate::run_retry::Retry::next(None, cause.clone(), now()).unwrap();
                    db.change_run(
                        &id,
                        crate::runs::RunChanges {
                            status: Some(RunStatus::Queued),
                            retry: Some(Some(retry)),
                            cancel_requested_at: Some(cancelled.then(now)),
                            boundary: Some(
                                json!({ "nativeModeInterruption": null, "moveRequest": null }),
                            ),
                            ..Default::default()
                        },
                    )?;
                    let mut saved = RunCheckpoint::from_value(json!({
                        "prepared": { "backend": "firecracker", "isolated": true },
                        "runnerId": if same_attempt { "old" } else { "replacement" },
                        "completed": false, "terminalCode": 1,
                        "remainingMs": if expired { Some(0) } else { None },
                        "controllerRecoveries": 3, "providerRetryReservation": "old",
                        "turnPending": pending_turn,
                    }))?;
                    saved.retry_cause = Some(if same_cause { Some(cause) } else { None });
                    saved.store_in(db, &id)?;
                    let may_launch = finish_recovery(db, &id, Some(&saved))?;
                    assert_eq!(
                        may_launch,
                        same_attempt && same_cause && !pending_turn && !cancelled && !expired
                    );
                    if may_launch {
                        let durable = RunCheckpoint::load_in(db, &id)?.unwrap();
                        assert_eq!(durable.controller_recoveries(), 3);
                        assert!(durable.has_provider_retry_reservation());
                        assert!(finish_recovery(db, &id, Some(&durable))?);
                        assert_eq!(
                            RunCheckpoint::load_in(db, &id)?
                                .unwrap()
                                .controller_recoveries(),
                            3
                        );
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn paid_move_recovery_at_ceiling_is_not_a_free_controller_retry() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        for (movement, same_attempt) in [(true, true), (false, true), (true, false)] {
            let id = run.clone();
            service.store.transaction(move |db| {
                db.change_run(&id, crate::runs::RunChanges {
                    status: Some(RunStatus::Queued), cancel_requested_at: Some(None),
                    boundary: Some(json!({ "moveRequest": movement.then(|| json!({ "nodeId": "destination" })) })),
                    ..Default::default()
                })?;
                let saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "firecracker", "isolated": true },
                    "runnerId": if same_attempt { "attempt" } else { "replacement" },
                    "completed": false, "terminalCode": 75, "remainingMs": null,
                    "controllerRecoveries": 4, "controllerRecoveryReservation": "attempt",
                }))?;
                saved.store_in(db, &id)?;
                assert_eq!(finish_recovery(db, &id, Some(&saved))?, movement && same_attempt);
                let durable = RunCheckpoint::load_in(db, &id)?.unwrap();
                assert_eq!(durable.controller_recoveries(), 4);
                Ok(())
            }).await.unwrap();
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn transferred_move_authorization_is_attempt_scoped_and_consumed() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        for (replace, consume, cancelled, expired) in [
            (false, false, false, false),
            (true, false, false, false),
            (false, true, false, false),
            (false, false, true, false),
            (false, false, false, true),
        ] {
            let id = run.clone();
            service
                .store
                .transaction(move |db| {
                    db.change_run(
                        &id,
                        crate::runs::RunChanges {
                            status: Some(RunStatus::Queued),
                            cancel_requested_at: Some(cancelled.then(now)),
                            boundary: Some(json!({ "moveRequest": null })),
                            ..Default::default()
                        },
                    )?;
                    let mut saved = RunCheckpoint::from_value(json!({
                        "prepared": { "backend": "firecracker", "isolated": true },
                        "runnerId": "attempt", "completed": false, "terminalCode": 75,
                        "remainingMs": if expired { Some(0) } else { None },
                        "controllerRecoveries": 4, "controllerRecoveryReservation": "attempt",
                        "moveRecoveryReservation": "attempt",
                    }))?;
                    if replace {
                        saved.runner_id = Some(Some("replacement".into()));
                    }
                    if consume {
                        saved.bind_paid_admission("new-attempt", &db.run_state(&id)?.unwrap());
                    }
                    saved.store_in(db, &id)?;
                    assert_eq!(
                        finish_recovery(db, &id, Some(&saved))?,
                        !replace && !consume && !cancelled && !expired
                    );
                    assert_eq!(
                        RunCheckpoint::load_in(db, &id)?
                            .unwrap()
                            .controller_recoveries(),
                        4
                    );
                    Ok(())
                })
                .await
                .unwrap();
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn paid_native_mode_recovery_can_settle_at_and_above_controller_ceiling() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        for count in [3, 4, 5] {
            for (mode, cancelled, expired) in [
                (true, false, false),
                (false, false, false),
                (true, true, false),
                (true, false, true),
            ] {
                let id = run.clone();
                service.store.transaction(move |db| {
                    db.change_run(&id, crate::runs::RunChanges {
                        status: Some(RunStatus::Queued), session_id: Some(None),
                        cancel_requested_at: Some(cancelled.then(now)),
                        boundary: Some(json!({ "nativeModeInterruption": mode.then(crate::config::id) })),
                        ..Default::default()
                    })?;
                    let saved = RunCheckpoint::from_value(json!({
                        "prepared": { "backend": "native", "isolated": true },
                        "runnerId": "attempt", "completed": false, "terminalCode": 75,
                        "remainingMs": if expired { Some(0) } else { None },
                        "controllerRecoveries": count, "controllerRecoveryReservation": "attempt",
                    }))?;
                    saved.store_in(db, &id)?;
                    let admitted = finish_recovery(db, &id, Some(&saved))?;
                    assert_eq!(admitted, mode && !cancelled && !expired || count == 3 && !cancelled && !expired,
                               "count={count}, mode={mode}, cancelled={cancelled}, expired={expired}");
                    let current = db.run_state(&id)?.unwrap();
                    if cancelled { assert_eq!(current.status, RunStatus::Cancelled); }
                    if expired && !cancelled { assert_eq!(current.status, RunStatus::Failed); }
                    if admitted { assert!(RunCheckpoint::load_in(db, &id)?.unwrap().has_controller_recovery_reservation()); }
                    Ok(())
                }).await.unwrap();
            }
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn recovery_reservation_is_paid_once_and_bound_to_attempt() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        for count in 0..=3 {
            let id = run.clone();
            service
                .store
                .transaction(move |db| {
                    db.change_run(
                        &id,
                        crate::runs::RunChanges {
                            status: Some(RunStatus::Queued),
                            cancel_requested_at: Some(None),
                            ..Default::default()
                        },
                    )?;
                    let saved = RunCheckpoint::from_value(json!({
                        "prepared": { "backend": "native", "isolated": true },
                        "runnerId": "attempt", "completed": false,
                        "terminalCode": 75, "remainingMs": null,
                        "controllerRecoveries": count,
                    }))?;
                    saved.store_in(db, &id)?;
                    let allowed = finish_recovery(db, &id, Some(&saved))?;
                    assert_eq!(allowed, count < 3);
                    if allowed {
                        let paid = RunCheckpoint::load_in(db, &id)?.unwrap();
                        assert_eq!(paid.controller_recoveries(), count + 1);
                        assert!(paid.has_controller_recovery_reservation());
                        assert!(finish_recovery(db, &id, Some(&paid))?);
                        let paid_again = RunCheckpoint::load_in(db, &id)?.unwrap();
                        assert_eq!(paid_again.controller_recoveries(), count + 1);
                        let mut replacement = paid_again;
                        replacement.runner_id = Some(Some("replacement".into()));
                        assert!(!replacement.has_controller_recovery_reservation());
                        if count == 2 {
                            assert!(!finish_recovery(db, &id, Some(&replacement))?);
                        }
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn failed_flush_recovery_is_terminal_only_for_its_attempt() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        for (same, completed, cancelled) in [
            (true, false, false),
            (true, true, false),
            (false, false, false),
            (true, false, true),
        ] {
            let id = run.clone();
            service.store.transaction(move |db| {
                db.change_run(&id, crate::runs::RunChanges {
                    status: Some(RunStatus::Queued), cancel_requested_at: Some(cancelled.then(now)),
                    retry: Some(None), ..Default::default()
                })?;
                let saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "native", "isolated": true }, "runnerId": "current",
                    "finalizationFailureAttempt": if same { "current" } else { "old" },
                    "completed": completed, "terminalCode": 70, "remainingMs": null,
                    "controllerRecoveries": 0,
                }))?;
                saved.store_in(db, &id)?;
                let admitted = finish_recovery(db, &id, Some(&saved))?;
                assert_eq!(admitted, !same && !completed && !cancelled);
                let state = db.run_state(&id)?.unwrap();
                if same { assert_eq!(state.status, if cancelled { RunStatus::Cancelled } else { RunStatus::Failed }); }
                Ok(())
            }).await.unwrap();
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn recovered_provider_failure_is_paid_once_and_attempt_scoped() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        for (interrupted, same, code, cancelled, expired, flush) in [
            (true, true, 1, false, false, false),
            (false, true, 1, false, false, false),
            (false, false, 1, false, false, false),
            (false, true, 70, false, false, false),
            (false, true, 1, true, false, false),
            (false, true, 1, false, true, false),
            (false, true, 1, false, false, true),
        ] {
            let id = run.clone();
            service.store.transaction(move |db| {
                db.change_run(&id, crate::runs::RunChanges {
                    status: Some(RunStatus::Queued), session_id: Some(Some("session".into())),
                    cancel_requested_at: Some(cancelled.then(now)), retry: Some(None),
                    ..Default::default()
                })?;
                let cause = crate::run_retry::Cause::classify(&json!({ "code": "server_overloaded", "message": "Provider busy" })).unwrap();
                let mut saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "firecracker", "isolated": true },
                    "runnerId": "attempt", "providerFailureAttempt": if same { "attempt" } else { "old" },
                    "completed": false, "terminalCode": code, "outputInterrupted": interrupted,
                    "finalizationFailureAttempt": if flush { Some("attempt") } else { None },
                    "remainingMs": if expired { Some(0) } else { None }, "controllerRecoveries": 3,
                }))?;
                saved.retry_cause = Some(Some(cause));
                saved.store_in(db, &id)?;
                assert!(!finish_recovery(db, &id, Some(&saved))?);
                let state = db.run_state(&id)?.unwrap();
                if same && code == 1 && !cancelled && !expired && !flush {
                    assert_eq!(state.status, RunStatus::Queued);
                    assert_eq!(state.retry.as_ref().unwrap().attempt, 1);
                    let paid = RunCheckpoint::load_in(db, &id)?.unwrap();
                    assert!(paid.has_provider_retry_reservation());
                    assert_eq!(paid.controller_recoveries(), 3);
                    assert!(finish_recovery(db, &id, Some(&paid))?);
                    assert_eq!(db.run_state(&id)?.unwrap().retry.unwrap().attempt, 1);
                } else {
                    assert_eq!(state.status, if cancelled { RunStatus::Cancelled } else { RunStatus::Failed });
                    assert!(state.retry.is_none());
                }
                Ok(())
            }).await.unwrap();
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn intact_provider_failure_is_paid_after_restart_without_controller_charge() {
        use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::any};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let waits = Arc::new(AtomicUsize::new(0));
        let fences = Arc::new(AtomicUsize::new(0));
        let app = Router::new().fallback(any({
            let (waits, fences) = (waits.clone(), fences.clone());
            move |request: axum::http::Request<axum::body::Body>| {
                let (waits, fences) = (waits.clone(), fences.clone());
                async move {
                    if request.uri().path().ends_with("/wait") {
                        waits.fetch_add(1, Ordering::SeqCst);
                        return Json(json!({ "StatusCode": 1 })).into_response();
                    }
                    if request.method() == axum::http::Method::DELETE {
                        fences.fetch_add(1, Ordering::SeqCst);
                    }
                    StatusCode::NO_CONTENT.into_response()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for count in [0, 3] {
            let (_root, service, _chat, run, _workspace) =
                fixture_with_runner(format!("http://{address}")).await;
            service
                .store
                .change_run(
                    &run,
                    crate::runs::RunChanges {
                        session_id: Some(Some("provider-session".into())),
                        recovery_pending: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "firecracker", "isolated": true },
                "runnerId": "attempt", "completed": false, "remainingMs": null,
                "controllerRecoveries": count,
            }))
            .unwrap();
            saved.store(&service.store, &run).await.unwrap();
            let checkpoint = super::super::checkpoint::Checkpoint::new(
                service.store.clone(),
                run.clone(),
                saved,
            );
            let cause = crate::run_retry::Cause::classify(
                &json!({ "code": "server_overloaded", "message": "Provider busy" }),
            )
            .unwrap();
            checkpoint
                .record_event(
                    cairn_contracts::EventKind::TurnFailed,
                    "Provider busy".into(),
                    json!({ "type": "turn.failed" }),
                    super::super::checkpoint::EventChanges {
                        run: None,
                        failure: Some(("Provider busy".into(), Some(cause), true)),
                        completed: false,
                        last_message: None,
                    },
                )
                .await
                .unwrap();
            let before = RunCheckpoint::load(&service.store, &run)
                .await
                .unwrap()
                .unwrap();
            assert!(before.terminal_code.is_none());
            assert!(before.output_interrupted.is_none());
            assert!(before.provider_retry_reservation.is_none());
            assert!(
                !recover(&service, &service.store.run(&run).await.unwrap())
                    .await
                    .unwrap()
            );
            let waiting = service.store.run_state(&run).await.unwrap();
            assert_eq!(waiting.status, RunStatus::Queued, "count={count}");
            let retry = waiting.retry.unwrap();
            assert_eq!(retry.attempt, 1);
            assert!(retry.next_attempt_at.is_some_and(|at| at > now()));
            let paid = RunCheckpoint::load(&service.store, &run)
                .await
                .unwrap()
                .unwrap();
            assert!(paid.has_provider_retry_reservation());
            assert_eq!(paid.controller_recoveries(), count);
            assert!(
                recover(&service, &service.store.run(&run).await.unwrap())
                    .await
                    .unwrap()
            );
            let again = service.store.run_state(&run).await.unwrap().retry.unwrap();
            assert_eq!(again.attempt, 1);
            assert_eq!(again.next_attempt_at, retry.next_attempt_at);
            assert_eq!(
                RunCheckpoint::load(&service.store, &run)
                    .await
                    .unwrap()
                    .unwrap()
                    .controller_recoveries(),
                count
            );
            service.shutdown.cancel();
        }
        assert_eq!(waits.load(Ordering::SeqCst), 2);
        assert!(fences.load(Ordering::SeqCst) >= 2);
        server.abort();
    }

    #[tokio::test]
    async fn recovered_output_loss_fails_only_when_completion_is_missing() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        for completed in [false, true] {
            let id = run.clone();
            service
                .store
                .transaction(move |db| {
                    db.change_run(
                        &id,
                        crate::runs::RunChanges {
                            status: Some(RunStatus::Queued),
                            cancel_requested_at: Some(None),
                            ..Default::default()
                        },
                    )?;
                    let saved = RunCheckpoint::from_value(json!({
                        "prepared": { "backend": "native", "isolated": true },
                        "runnerId": "attempt", "completed": completed,
                        "outputInterrupted": true, "terminalCode": 0, "remainingMs": null,
                    }))?;
                    assert!(!finish_recovery(db, &id, Some(&saved))?);
                    let state = db.run_state(&id)?.unwrap();
                    assert_eq!(
                        state.status,
                        if completed {
                            RunStatus::Succeeded
                        } else {
                            RunStatus::Failed
                        }
                    );
                    if !completed {
                        assert!(state.error.unwrap().contains("output was interrupted"));
                    }
                    Ok(())
                })
                .await
                .unwrap();
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn recovery_may_spend_a_reservation_already_charged_to_the_attempt() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        service
            .store
            .transaction(move |db| {
                let mut saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "native", "isolated": true },
                    "runnerId": "old-attempt", "completed": false, "terminalCode": 143,
                    "controllerRecoveries": 3, "controllerRecoveryReservation": "old-attempt",
                    "remainingMs": null,
                }))?;
                assert!(finish_recovery(db, &run, Some(&saved))?);
                saved.runner_id = Some(Some("replacement".into()));
                assert!(!finish_recovery(db, &run, Some(&saved))?);
                assert_eq!(db.run_state(&run)?.unwrap().status, RunStatus::Failed);
                Ok(())
            })
            .await
            .unwrap();
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn failed_finalization_removes_typed_mission_wakeup() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        service
            .store
            .transaction(move |db| {
                let wakeup = serde_json::from_value(json!({
                    "id": "wake",
                    "messageId": null,
                    "reason": "Waiting",
                    "seconds": 3600,
                    "createdAt": now(),
                    "wakeAt": now() + 3_600_000,
                }))?;
                db.change_run(
                    &run,
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        trigger: Some("manual".into()),
                        wakeup: Some(Some(wakeup)),
                        ..Default::default()
                    },
                )?;
                let saved = RunCheckpoint::from_value(json!({
                    "prepared": { "backend": "native", "isolated": true },
                    "completed": true,
                    "terminalCode": 70,
                    "remainingMs": null,
                }))?;
                assert!(!finish_recovery(db, &run, Some(&saved))?);
                let record = db.run_record(&run)?.unwrap();
                assert_eq!(record.state.status, RunStatus::Failed);
                assert!(record.wakeup.is_none());
                Ok(())
            })
            .await
            .unwrap();
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn managed_completed_recovery_requires_healthy_finalization_and_cancellation_wins() {
        let (_root, service, _chat, run, _workspace) = fixture().await;
        for backend in ["native", "firecracker"] {
            for code in [None, Some(0), Some(70), Some(75)] {
                for cancel in [false, true] {
                    let run = run.clone();
                    service.store.transaction(move |db| {
                        let saved = RunCheckpoint::from_value(json!({
                            "prepared": { "backend": backend, "isolated": true },
                            "completed": true, "terminalCode": code,
                            "lastError": if code == Some(70) { "Finalization failed: EIO/EROFS" } else { "" },
                            "controllerRecoveries": 3,
                        }))?;
                        db.change_run(&run, crate::runs::RunChanges {
                            status: Some(RunStatus::Queued), cancel_requested_at: Some(cancel.then(now)),
                            ..Default::default()
                        })?;
                        assert!(!finish_recovery(db, &run, Some(&saved))?);
                        let current = db.run_state(&run)?.unwrap();
                        let expected = if cancel { RunStatus::Cancelled }
                            else if code == Some(0) { RunStatus::Succeeded } else { RunStatus::Failed };
                        assert_eq!(current.status, expected, "{backend} {code:?}");
                        assert_eq!(saved.controller_recoveries(), 3);
                        Ok(())
                    }).await.unwrap();
                }
            }
        }
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn fencing_rejects_replacement_before_and_during_delete() {
        use axum::{Router, extract::Request, response::IntoResponse, routing::any};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let deletes = Arc::new(AtomicUsize::new(0));
        let app = Router::new().fallback(any({
            let (entered, release, deletes) = (entered.clone(), release.clone(), deletes.clone());
            move |request: Request| {
                let (entered, release, deletes) =
                    (entered.clone(), release.clone(), deletes.clone());
                async move {
                    assert_eq!(request.uri().path(), "/runs/old-attempt");
                    deletes.fetch_add(1, Ordering::SeqCst);
                    entered.notify_one();
                    release.acquire().await.unwrap().forget();
                    axum::http::StatusCode::NO_CONTENT.into_response()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let original = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true }, "runnerId": "old-attempt",
            "completed": true, "terminalCode": 0, "remainingMs": null,
        }))
        .unwrap();
        let mut replacement = original.clone();
        replacement.runner_id = Some(Some("new-attempt".into()));
        replacement.process = Some(Some(crate::recovery::ProcessIdentity {
            pid: 99,
            start: "new".into(),
            boot: "new".into(),
        }));
        replacement.store(&service.store, &run).await.unwrap();
        let run_value = service.store.run(&run).await.unwrap();
        assert!(
            recovery::fence_attempt(&service, &run_value, Some(&original))
                .await
                .is_err()
        );
        assert_eq!(deletes.load(Ordering::SeqCst), 0);
        let mut changed_process = replacement.clone();
        changed_process.runner_id = original.runner_id.clone();
        changed_process.store(&service.store, &run).await.unwrap();
        assert!(
            recovery::fence_attempt(&service, &run_value, Some(&original))
                .await
                .is_err()
        );
        assert_eq!(deletes.load(Ordering::SeqCst), 0);
        original.store(&service.store, &run).await.unwrap();
        let s = service.clone();
        let task = tokio::spawn(async move { recover(&s, &run_value).await });
        entered.notified().await;
        replacement.store(&service.store, &run).await.unwrap();
        release.add_permits(1);
        assert!(task.await.unwrap().is_err());
        let current = RunCheckpoint::load(&service.store, &run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.runner_id(), Some("new-attempt"));
        assert_eq!(current.process(), replacement.process());
        assert_eq!(deletes.load(Ordering::SeqCst), 1);
        service.shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn restart_reconciliation_cannot_fence_a_replacement_and_honors_concurrent_cancel() {
        use axum::{Router, extract::Request, response::IntoResponse, routing::any};
        use std::sync::atomic::{AtomicUsize, Ordering};
        for replace in [true, false] {
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let deletes = Arc::new(AtomicUsize::new(0));
            let (read, reply, stops) = (entered.clone(), release.clone(), deletes.clone());
            let app = Router::new().fallback(any(move |request: Request| {
                let (read, reply, stops) = (read.clone(), reply.clone(), stops.clone());
                async move {
                    assert!(request.uri().path().starts_with("/runs/old-attempt"));
                    if request.method() == "DELETE" {
                        stops.fetch_add(1, Ordering::SeqCst);
                        return axum::http::StatusCode::NO_CONTENT.into_response();
                    }
                    assert_eq!(request.uri().path(), "/runs/old-attempt/wait");
                    read.notify_one();
                    reply.notified().await;
                    axum::Json(json!({ "StatusCode": 0 })).into_response()
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let (_root, service, _chat, run, _workspace) =
                fixture_with_runner(format!("http://{address}")).await;
            let saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true },
                "runnerId": "old-attempt", "completed": true, "controllerRecoveries": 3,
            }))
            .unwrap();
            saved.store(&service.store, &run).await.unwrap();
            let (s, id) = (service.clone(), run.clone());
            let recovering =
                tokio::spawn(async move { recover(&s, &s.store.run(&id).await.unwrap()).await });
            entered.notified().await;
            if replace {
                let mut replacement = saved.clone();
                replacement.runner_id = Some(Some("new-attempt".into()));
                replacement.completed = Some(false);
                replacement.store(&service.store, &run).await.unwrap();
            } else {
                service
                    .store
                    .change_run(
                        &run,
                        crate::runs::RunChanges {
                            cancel_requested_at: Some(Some(now())),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
            }
            release.notify_one();
            assert!(!recovering.await.unwrap().unwrap());
            if replace {
                let current = RunCheckpoint::load(&service.store, &run)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(current.runner_id(), Some("new-attempt"));
                assert_eq!(current.terminal_code(), None);
                assert_eq!(deletes.load(Ordering::SeqCst), 0);
            } else {
                assert_eq!(
                    service.store.run_state(&run).await.unwrap().status,
                    RunStatus::Cancelled
                );
                assert_eq!(deletes.load(Ordering::SeqCst), 1);
            }
            server.abort();
            service.shutdown.cancel();
        }
    }

    #[tokio::test]
    async fn a_new_turn_cannot_reuse_the_previous_attempts_healthy_exit() {
        use axum::{Router, extract::Request, response::IntoResponse, routing::any};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let waits = Arc::new(AtomicUsize::new(0));
        let count = waits.clone();
        let app = Router::new().fallback(any(move |request: Request| {
            let count = count.clone();
            async move {
                if request.method() == "DELETE" {
                    return axum::http::StatusCode::NO_CONTENT.into_response();
                }
                count.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({ "StatusCode": 0 })).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (_root, service, _chat, run, _workspace) =
            fixture_with_runner(format!("http://{address}")).await;
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "old-attempt", "completed": false, "terminalCode": null,
        }))
        .unwrap();
        saved.store(&service.store, &run).await.unwrap();
        assert!(
            recover(&service, &service.store.run(&run).await.unwrap())
                .await
                .unwrap()
        );
        assert_eq!(waits.load(Ordering::SeqCst), 0);
        assert_eq!(
            service.store.run_state(&run).await.unwrap().status,
            RunStatus::Queued
        );
        server.abort();
        service.shutdown.cancel();
    }

    #[tokio::test]
    async fn waiting_mission_restart_releases_attempt_before_deadline_even_when_cancelled() {
        for (cancel, remote, queued) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (true, true, false),
            (false, false, true),
            (true, false, true),
            (false, true, true),
            (true, true, true),
        ] {
            let (_root, original, chat_id, run_id, _workspace) = fixture().await;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = requests.clone();
            let remote_observed = requests.clone();
            let app = axum::Router::new()
                .route(
                    "/runs/{attempt}",
                    axum::routing::delete(move || async move {
                        observed.fetch_add(1, Ordering::Relaxed);
                        axum::http::StatusCode::NOT_FOUND
                    }),
                )
                .route(
                    "/internal/execution/{node}/runs/{attempt}",
                    axum::routing::delete(move || async move {
                        remote_observed.fetch_add(1, Ordering::Relaxed);
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    }),
                );
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut config = original.config.clone();
            config.runner_url = format!("http://{address}");
            config.public_url = config.runner_url.clone();
            let node = if remote {
                "offline-node"
            } else {
                crate::nodes::LOCAL_NODE_ID
            };
            original
                .store
                .transaction(move |db| db.remove_conversation(&chat_id))
                .await
                .unwrap();
            drop(original);
            let service = Service::new(config).await.unwrap();
            let wakeup = json!({
                "id": "wake", "messageId": null, "reason": "Waiting", "seconds": 3600,
                "createdAt": now(), "wakeAt": now() + 3_600_000,
            });
            service
                .store
                .change_run(
                    &run_id,
                    crate::runs::RunChanges {
                        status: Some(if queued {
                            RunStatus::Queued
                        } else {
                            RunStatus::Waiting
                        }),
                        recovery_pending: Some(queued),
                        trigger: Some(("manual").to_owned()),
                        session_id: Some(Some("saved".into())),
                        wakeup: Some(Some(serde_json::from_value(wakeup.clone()).unwrap())),
                        boundary: Some(json!({"isolated": true})),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let checkpoint = RunCheckpoint::from_value(json!({
                "completed": true, "terminalCode": 0, "launched": true, "runnerId": "old-attempt",
                "nodeId": node,
                "prepared": { "isolated": true, "backend": "native" },
            }))
            .unwrap();
            checkpoint.store(&service.store, &run_id).await.unwrap();
            service
                .store
                .put(
                    "node-attempts",
                    json!({
                        "id": "old-attempt", "runId": run_id,
                        "nodeId": node, "released": false,
                    }),
                )
                .await
                .unwrap();

            if remote {
                // The previous remote execution lease has expired; its node
                // cannot serve requests and has no available placement record.
                service.node_lease_deadlines.lock().await.insert(
                    "old-attempt".into(),
                    tokio::time::Instant::now() - Duration::from_secs(30),
                );
            }
            service.worker.initialize(&service).await.unwrap();
            if cancel {
                service.worker.cancel(&service, &run_id).await.unwrap();
            }
            service.worker.tick(&service).await.unwrap();

            assert_eq!(requests.load(Ordering::Relaxed), 1);
            let attempt = service
                .store
                .get("node-attempts", "old-attempt")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(attempt["released"], true);
            let current = service.store.run(&run_id).await.unwrap();
            assert_eq!(current["recoveryPending"], false);
            assert_eq!(
                current["status"],
                if cancel { "cancelled" } else { "waiting" }
            );
            if cancel {
                assert!(current["wakeup"].is_null());
            } else {
                assert_eq!(current["wakeup"], wakeup);
                assert!(current["missionTurnId"].is_null());
            }
            assert!(service.worker.active.lock().await.is_empty());
            server.abort();
        }
    }

    #[tokio::test]
    async fn completed_mission_recovery_keeps_resume_and_cancellation_wins_atomically() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let record = json!({
                    "id": "mission", "taskId": "task", "status": "queued", "createdAt": 0,
                    "recoveryPending": true, "sessionId": "saved", "resumeAvailable": false,
                    "wakeup": { "id": "wake" },
                    "outcome": { "status": "completed" },
                });
                db.add_run_fixture(&record, None)?;
                let checkpoint = RunCheckpoint::from_value(json!({ "completed": true }))?;
                assert!(!finish_recovery(db, "mission", Some(&checkpoint))?);
                let waiting = db.run("mission")?.unwrap();
                assert_eq!(waiting["status"], "waiting");
                assert_eq!(waiting["resumeAvailable"], true);
                assert!(waiting["finishedAt"].is_null());
                assert!(waiting["outcome"].is_null());
                for settled in [false, true] {
                    // A stale recovery snapshot preceded this cancellation. Completion
                    // must read it in the same transaction that writes the next state.
                    db.change_run(
                        "mission",
                        crate::runs::RunChanges {
                            status: Some(cairn_contracts::RunStatus::Queued),
                            recovery_pending: Some(true),
                            cancel_requested_at: Some(Some(now())),
                            wakeup: Some(None),
                            ..Default::default()
                        },
                    )?;
                    let mut checkpoint = checkpoint.clone();
                    if settled {
                        checkpoint.settled = Some(Some(Settled::from_state(
                            &db.run_state("mission")?.unwrap(),
                        )));
                    }
                    assert!(!finish_recovery(db, "mission", Some(&checkpoint))?);
                    let cancelled = db.run("mission")?.unwrap();
                    assert_eq!(cancelled["status"], "cancelled");
                    assert!(cancelled["wakeup"].is_null());
                    assert_eq!(cancelled["recoveryPending"], false);
                    assert!(db.active()?.is_empty());
                }
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn compaction_completed_before_restart_keeps_the_last_reply_and_outcome() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let outcome = json!({ "status": "completed", "messageId": "first" });
                let record = json!({
                    "id": "chat-run",
                    "taskId": "chat",
                    "trigger": "chat",
                    "status": "queued",
                    "createdAt": 0,
                    "recoveryPending": true,
                    "sessionId": "saved",
                    "summary": "Last reply",
                    "finishedAt": 500,
                    // Resuming the interrupted compaction cleared the run's outcome.
                    "outcome": null,
                    "chatExecution": {
                        "messageId": "compact:fixture",
                        "text": "",
                        "attachments": [],
                        "recovery": false,
                        "operation": "compact",
                        "previous": { "summary": "Last reply", "finishedAt": 500, "outcome": outcome },
                    },
                });
                db.add_run_fixture(&record, None)?;

                // The compaction's `turn.completed` was saved; the manager stopped before
                // settling the run, and a compaction leaves no reply in the checkpoint.
                let checkpoint = RunCheckpoint::from_value(json!({ "completed": true }))?;
                assert_eq!(checkpoint.last_message(), "");
                assert!(!finish_recovery(db, "chat-run", Some(&checkpoint))?);

                let recovered = db.run("chat-run")?.unwrap();
                assert_eq!(recovered["status"], "succeeded");
                assert_eq!(recovered["recoveryPending"], false);
                assert_eq!(recovered["summary"], "Last reply");
                assert_eq!(recovered["finishedAt"], 500);
                assert_eq!(recovered["outcome"], outcome);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn completed_turn_with_late_controller_failure_settles_without_relaunch() {
        let (_root, service, _chat_id, run_id, _workspace) = fixture().await;
        let checkpoint = super::super::checkpoint::Checkpoint::new(
            service.store.clone(),
            run_id.clone(),
            RunCheckpoint::fresh(None),
        );
        let mut activity =
            crate::performance::Activity::new(&run_id, "fixture", "host", Provider::Codex);
        let mut log_total = 500_000;
        for event in [
            json!({ "type": "turn.completed" }),
            json!({ "type": "runner.failed", "error": { "message": "Workspace sealing failed" } }),
        ] {
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
                &mut log_total,
                &mut activity,
            )
            .await
            .unwrap();
        }
        let saved = RunCheckpoint::load(&service.store, &run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.last_error(), "Workspace sealing failed");
        let run = service.store.run(&run_id).await.unwrap();
        assert!(!recover(&service, &run).await.unwrap());
        assert_eq!(
            service.store.run(&run_id).await.unwrap()["status"],
            "succeeded"
        );
    }

    #[tokio::test]
    async fn deletion_during_account_preparation_leaves_no_orphan_files() {
        let (_root, service, chat_id, run_id, workspace) = fixture().await;
        service.accounts.initialize(&service).await.unwrap();
        let mut account = serde_json::to_value(
            service
                .accounts
                .create(&service, Provider::Codex, "Ready")
                .await
                .unwrap(),
        )
        .unwrap();
        account["state"] = "ready".into();
        account["usage"] = json!({
            "allowed": true,
            "checkedAt": now(),
            "windows": [{ "id": "w", "usedPercent": 20, "models": [] }],
            "resets": null
        });
        service
            .store
            .save_account_fixture(account.clone())
            .await
            .unwrap();
        let account_guard = service.accounts.lock(text(&account, "id")).await;
        let run = service.store.run(&run_id).await.unwrap();
        let mut projects = HashSet::new();
        let state = service.store.run_state(&run_id).await.unwrap();
        let admission = service
            .worker
            .try_launch(&service, run, state, &mut projects);
        tokio::pin!(admission);
        let account_mutex = tokio::sync::OwnedMutexGuard::mutex(&account_guard);
        let owners = Arc::strong_count(account_mutex);
        tokio::time::timeout(
            Duration::from_secs(2),
            std::future::poll_fn(|cx| {
                assert!(admission.as_mut().poll(cx).is_pending());
                if Arc::strong_count(account_mutex) > owners {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }),
        )
        .await
        .unwrap();

        service.chat_trash(&chat_id, true).await.unwrap();
        let other = service.chat_create(json!({})).await.unwrap();
        let other_id = text(&other, "id");
        service.chat_trash(other_id, true).await.unwrap();
        service.cleanup_conversations().await.unwrap();
        service.cleanup_conversations().await.unwrap();
        assert!(
            service
                .store
                .conversation_boundary(other_id)
                .await
                .unwrap()
                .is_none(),
            "Blocked preparation must not stall unrelated deletion"
        );

        drop(account_guard);
        admission.await.unwrap();
        assert!(service.worker.active.lock().await.is_empty());
        assert!(
            service
                .accounts
                .lease(&service, &run_id)
                .await
                .unwrap()
                .is_none()
        );
        service.cleanup_conversations().await.unwrap();
        assert!(
            service
                .store
                .conversation_boundary(&chat_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(service.store.run(&run_id).await.unwrap_err().status, 404);
        assert!(
            service
                .store
                .checkpoint_boundary(&run_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !workspace.exists(),
            "Account preparation must not recreate a purged run directory"
        );
    }

    #[tokio::test]
    async fn a_retry_that_expires_during_preparation_cannot_register() {
        let (_root, service, _chat_id, run_id, workspace) = fixture().await;
        let _prepared = service
            .store
            .change_run(
                &run_id,
                crate::runs::RunChanges {
                    retry: Some(Some(
                        serde_json::from_value(json!({
                            "attempt": 1,
                            "limit": 5,
                            "nextAttemptAt": now() - 1,
                            "deadlineAt": now() + 60_000,
                            "cause": {"kind":"timeout","message":"temporary"},
                        }))
                        .unwrap(),
                    )),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let prepared = service.store.run(&run_id).await.unwrap();
        // Admission captured an eligible run, then waits behind asynchronous work.
        let active = service.worker.active.lock().await;
        let registration = service
            .worker
            .register(&service, &prepared, CancellationToken::new());
        tokio::pin!(registration);
        std::future::poll_fn(|cx| {
            assert!(registration.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        service
            .store
            .change_run(
                &run_id,
                crate::runs::RunChanges {
                    retry: Some({
                        let mut retry = service.store.run_state(&run_id).await.unwrap().retry;
                        retry.as_mut().unwrap().deadline_at = Some(now() - 1);
                        retry
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        drop(active);
        assert!(!registration.await.unwrap());
        let stopped = service.store.run(&run_id).await.unwrap();
        assert_eq!(stopped["status"], RunStatus::Failed);
        assert!(stopped["retry"]["nextAttemptAt"].is_null());
        assert!(service.worker.active.lock().await.is_empty());
        assert_eq!(
            tokio::fs::read_to_string(workspace.join("work.txt"))
                .await
                .unwrap(),
            "Private work"
        );
    }

    #[tokio::test]
    async fn deletion_committed_before_registration_prevents_stale_admission() {
        let (_root, service, chat_id, run_id, workspace) = fixture().await;
        let prepared = service.store.run(&run_id).await.unwrap();

        assert!(
            service
                .worker
                .register(&service, &prepared, CancellationToken::new())
                .await
                .unwrap()
        );
        assert!(
            !service
                .worker
                .register(&service, &prepared, CancellationToken::new())
                .await
                .unwrap()
        );
        service.worker.active.lock().await.remove(&run_id);

        let active = service.worker.active.lock().await;
        let registration = service
            .worker
            .register(&service, &prepared, CancellationToken::new());
        tokio::pin!(registration);
        std::future::poll_fn(|cx| {
            assert!(registration.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let deleting = service.clone();
        let deleting_id = chat_id.clone();
        let deletion = tokio::spawn(async move { deleting.chat_trash(&deleting_id, true).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let chat = service
                    .store
                    .conversation_boundary(&chat_id)
                    .await
                    .unwrap()
                    .unwrap();
                let run = service.store.run(&run_id).await.unwrap();
                if chat["lifecycle"] == "trash" && !run["cancelRequestedAt"].is_null() {
                    assert_eq!(run["status"], RunStatus::Queued);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(active);

        assert!(
            !registration.await.unwrap(),
            "A deleted conversation must not gain an execution"
        );
        deletion.await.unwrap().unwrap();
        assert!(service.worker.active.lock().await.is_empty());
        service.cleanup_conversations().await.unwrap();
        assert!(
            service
                .store
                .conversation_boundary(&chat_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(service.store.run(&run_id).await.unwrap_err().status, 404);
        assert!(
            service
                .store
                .checkpoint_boundary(&run_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!workspace.exists());
        assert!(
            !service
                .worker
                .register(&service, &prepared, CancellationToken::new())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn an_admission_prepared_before_merge_cannot_register_after_resume() {
        let (_root, service, chat_id, run_id, _workspace) = fixture().await;
        let source = service
            .agent_fixture(json!({ "name": "Source" }), None)
            .await
            .unwrap();
        let run_id_copy = run_id.clone();
        let source_copy = source.clone();
        let prepared = service
            .store
            .transaction(move |db| {
                crate::run_fixture::change_run_fixture(
                    db,
                    &run_id_copy,
                    &json!({
                        "snapshot": { "agent": source_copy },
                        "chatExecution": { "messageId": "original-message" },
                    }),
                )
            })
            .await
            .unwrap();
        service
            .store
            .change_run(
                &run_id,
                crate::runs::RunChanges {
                    status: Some(RunStatus::Cancelled),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        service
            .store
            .transaction({
                let chat_id = chat_id.clone();
                let source = source.clone();
                move |db| {
                    let mut chat = db.conversation_boundary(&chat_id)?.unwrap();
                    chat["agentId"] = source["id"].clone();
                    db.save_conversation_boundary(&chat)?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        service
            .merge_agents(serde_json::from_value(json!({
                "sourceId": source["id"], "targetId": crate::config::MAIN_AGENT_ID, "confirm": true,
            })).unwrap())
            .await
            .unwrap();
        let resumed = service.worker.resume(&service, &run_id).await.unwrap();
        assert!(
            !service
                .worker
                .register(&service, &prepared, CancellationToken::new())
                .await
                .unwrap()
        );
        assert!(service.worker.active.lock().await.is_empty());
        assert!(
            service
                .worker
                .register(&service, &resumed, CancellationToken::new())
                .await
                .unwrap()
        );
    }
}
