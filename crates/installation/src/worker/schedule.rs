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
    collections::{HashMap, HashSet, VecDeque},
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
        let provider_active = self.provider_active(&active).await;
        crate::chats::coding_agent::tick(s, &active).await?;
        // Parked missions must release any pre-crash execution independently
        // of their wakeup and of admission capacity for new processes.
        for state in s.store.read(|db| db.active_run_states()).await? {
            let run_id = state.id.as_str();
            if state.status == RunStatus::Waiting
                && state.recovery_pending
                && !active.contains(run_id)
            {
                let key = run_id.to_owned();
                if s.store
                    .read(move |db| crate::environment_expiration::pending(db, &key))
                    .await?
                {
                    continue;
                }
                let preparation = self.run_preparation_lock(run_id).await;
                let _preparation = preparation.lock().await;
                recover_or_wait(s, &s.store.run(run_id).await?).await?;
            }
        }
        crate::waits::tick_groups(s, &provider_active).await?;
        s.chat_tick(&provider_active).await?;
        crate::waits::tick_missions(s, &provider_active).await?;
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
                db.0.execute("DELETE FROM exclusive_reservations WHERE state IN ('released','failed','expired') AND created_at<?", [now() - 30 * DAY_MS])?;
                db.0.execute("DELETE FROM events WHERE created_at<? AND run_id IN (SELECT id FROM runs WHERE status NOT IN ('queued','running','waiting') AND trigger!='chat')", [now() - 30 * DAY_MS])?;
                // Timeline cards read their source event; a card whose source expired goes with it.
                db.0.execute("DELETE FROM timeline_entries WHERE run_id IN (SELECT id FROM runs WHERE status NOT IN ('queued','running','waiting') AND trigger!='chat') AND NOT EXISTS(SELECT 1 FROM events e WHERE e.id=timeline_entries.source)", [])?;
                db.0.execute("DELETE FROM timeline_state WHERE run_id IN (SELECT id FROM runs WHERE status NOT IN ('queued','running','waiting') AND trigger!='chat') AND NOT EXISTS(SELECT 1 FROM events e WHERE e.run_id=timeline_state.run_id)", [])?;
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
                .read(move |db| {
                    Ok(
                        crate::chats::coding_agent::pending_for_run(db, &pending_id)?
                            || crate::environment_expiration::pending(db, &pending_id)?,
                    )
                })
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
        let checkpoint = RunCheckpoint::load(&s.store, &run_id).await?;
        let launched = checkpoint.as_ref().is_some_and(RunCheckpoint::launched);
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
                // A full, offline or maintained node keeps its environment: it waits.
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
            {
                let mut active = worker.active.lock().await;
                worker
                    .deferred_cleanup_observers
                    .lock()
                    .await
                    .live_handoffs
                    .remove(&run_id);
                active.remove(&run_id);
            }
            worker.held.lock().await.remove(&run_id);
            worker.job_redactions.lock().await.remove(&run_id);
            worker.notify();
            worker.notify_retirement();
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
    // The next environment waits until the previous one is fenced and its local files retired.
    if record.state.status != RunStatus::Queued
        || record.state.cancel_requested_at.is_some()
        || crate::environment_expiration::admission_blocked(db, id)?
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
        if s.worker
            .deferred_cleanup_observers
            .lock()
            .await
            .has_handoff(&run_id)
        {
            // The exact fenced checkpoint is already owned by deferred
            // recovery. A later housekeeping error does not requeue it.
            break;
        }
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

const MAX_DEFERRED_CLEANUP_OBSERVERS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CleanupObservation {
    run_id: String,
    attempt: String,
}

#[derive(Default)]
pub(super) struct DeferredCleanupObservers {
    in_flight: HashSet<CleanupObservation>,
    waiting: VecDeque<CleanupObservation>,
    live_handoffs: HashMap<String, String>,
}

impl DeferredCleanupObservers {
    pub(super) fn accept_handoff(&mut self, run_id: &str, attempt: &str) {
        self.live_handoffs
            .insert(run_id.to_owned(), attempt.to_owned());
    }

    pub(super) fn has_handoff(&self, run_id: &str) -> bool {
        self.live_handoffs.contains_key(run_id)
    }

    fn admit(
        &mut self,
        candidates: Vec<(CleanupObservation, RunCheckpoint)>,
    ) -> Vec<(CleanupObservation, RunCheckpoint)> {
        let eligible: HashSet<_> = candidates.iter().map(|(key, _)| key).collect();
        self.waiting
            .retain(|key| eligible.contains(key) && !self.in_flight.contains(key));

        let mut waiting: HashSet<_> = self.waiting.iter().cloned().collect();
        for (key, _) in &candidates {
            if !self.in_flight.contains(key) && waiting.insert(key.clone()) {
                self.waiting.push_back(key.clone());
            }
        }

        let slots = MAX_DEFERRED_CLEANUP_OBSERVERS.saturating_sub(self.in_flight.len());
        let mut candidates: HashMap<_, _> = candidates.into_iter().collect();
        let mut admitted = Vec::new();
        for key in self.waiting.drain(..self.waiting.len().min(slots)) {
            if let Some(saved) = candidates.remove(&key) {
                self.in_flight.insert(key.clone());
                admitted.push((key, saved));
            }
        }
        admitted
    }

    #[cfg(test)]
    pub(super) fn is_observing(&self) -> bool {
        !self.in_flight.is_empty()
    }
}

/// Observe and settle durable fences independently of ordinary recovery RPCs.
pub(super) async fn observe_native_cleanup(s: &Arc<Service>) -> Result<()> {
    let (mut ready, mut completed) = s
        .store
        .read(|db| {
            let mut ready = Vec::new();
            let mut completed = Vec::new();
            for run_id in db.native_cleanup_recovery_ids()? {
                let Some(saved) = RunCheckpoint::load_in(db, &run_id)? else {
                    continue;
                };
                let Some(pending) = saved.pending_native_cleanup() else {
                    continue;
                };
                let expiring = crate::environment_expiration::pending(db, &run_id)?;
                let Some(state) = db.run_state(&run_id)? else {
                    continue;
                };
                if pending.awaiting_retirement() && state.status != RunStatus::Queued {
                    let Some(chat) = db.conversation_for_run(&run_id)? else {
                        continue;
                    };
                    if !db.has_pending_messages(&chat.id)? {
                        continue;
                    }
                }
                let key = CleanupObservation {
                    run_id,
                    attempt: pending.attempt.clone(),
                };
                if state.status == RunStatus::Succeeded
                    && (expiring
                        || state.cancel_requested_at.is_some()
                        || !pending.needs_observation())
                {
                    completed.push((key, saved));
                } else if !expiring
                    && (pending.needs_observation() || pending.awaiting_retirement())
                {
                    ready.push((key, saved));
                }
            }
            Ok((ready, completed))
        })
        .await?;
    let admitted = {
        // A live worker owns the checkpoint until its last write. Admission
        // still sees the worker as active while it clears its local resources.
        let active = s.worker.active.lock().await;
        let mut observers = s.worker.deferred_cleanup_observers.lock().await;
        let eligible = |key: &CleanupObservation| {
            !active.contains_key(&key.run_id)
                || observers.live_handoffs.get(&key.run_id) == Some(&key.attempt)
        };
        ready.retain(|(key, _)| eligible(key));
        completed.retain(|(key, _)| eligible(key));
        observers.admit(ready)
    };
    for (key, saved) in admitted {
        let service = s.clone();
        s.worker.spawn_recovery(async move {
            let result = recovery::observe_pending_native_cleanup(&service, &key.run_id, saved).await;
            service.worker.deferred_cleanup_observers.lock().await.in_flight.remove(&key);
            if let Err(error) = result {
                tracing::warn!(run_id = key.run_id, message = %error.message, "Native cleanup observation failed");
            }
        });
    }
    for (key, expected) in completed {
        if let Err(error) = settle_native_cleanup(s, key.run_id.clone(), expected).await {
            tracing::warn!(run_id = key.run_id, message = %error.message, "Native cleanup settlement failed");
        }
    }
    Ok(())
}

/// Publish the outcome and revoke access without waiting for account I/O.
/// Ordinary launch recovery clears old provider homes before they are reused.
async fn settle_native_cleanup(s: &Service, id: String, expected: RunCheckpoint) -> Result<()> {
    use crate::recovery::DeferredCleanupState;

    let settled = s
        .store
        .transaction(move |db| {
            let Some(mut current) = RunCheckpoint::load_in(db, &id)? else {
                return Ok(false);
            };
            if !recovery::same_execution(Some(&current), Some(&expected))
                || current.pending_native_cleanup() != expected.pending_native_cleanup()
            {
                return Ok(false);
            }
            if !db
                .run_state(&id)?
                .is_some_and(|run| run.recovery_pending && run.status == RunStatus::Succeeded)
            {
                return Ok(false);
            }
            if let Some(pending) = current.pending_native_cleanup()
                && pending.state == DeferredCleanupState::Observing
                && (!pending.needs_observation()
                    || crate::environment_expiration::pending(db, &id)?)
            {
                let mut pending = pending.clone();
                pending.state = DeferredCleanupState::Unconfirmed;
                current.pending_native_cleanup = Some(pending);
            }
            db.revoke_mcp_run(&id)?;
            db.remove_account_lease(&id)?;
            finish_recovery(db, &id, Some(&current))?;
            Ok(true)
        })
        .await?;
    if settled {
        s.worker.notify();
    }
    Ok(())
}

/// A lost controller must not delay unrelated sends while completed chats fence.
pub(super) async fn recover_completed_environments(s: &Arc<Service>) -> Result<()> {
    for id in s.store.read(|db| db.completed_recovery_ids()).await? {
        if s.shutdown.is_cancelled() {
            break;
        }
        let preparation = s.worker.run_preparation_lock(&id).await;
        let _preparation = preparation.lock().await;
        if s.worker.active.lock().await.contains_key(&id) {
            continue;
        }
        let key = id.clone();
        if s.store
            .read(move |db| crate::environment_expiration::pending(db, &key))
            .await?
        {
            continue;
        }
        let run = s.store.run(&id).await?;
        if run["recoveryPending"] != true || RunStatus::of(&run) != Some(RunStatus::Succeeded) {
            continue;
        }
        if RunCheckpoint::load(&s.store, &id)
            .await?
            .is_some_and(|saved| saved.pending_native_cleanup().is_some())
        {
            continue;
        }
        recover_or_wait(s, &run).await?;
        s.worker.notify();
    }
    Ok(())
}

// A provider retry is paid by Retry::next, not by the controller recovery
// budget. Bind that authorization to the attempt and its confirmed cause.
pub(super) fn provider_retry_paid(saved: &RunCheckpoint, state: &RunState) -> bool {
    state.retry.as_ref().is_some_and(|retry| {
        retry.next_attempt_at.is_some()
            && (saved.turn_pending != Some(true) || (!saved.managed() && retry.cause.startup))
            && (!saved.managed() || saved.has_provider_retry_reservation())
            && saved.retry_cause.as_ref().and_then(Option::as_ref) == Some(&retry.cause)
    })
}

fn controller_budget_exhausted(
    saved: &RunCheckpoint,
    state: &RunState,
    native_mode_interruption: bool,
) -> bool {
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
    let exception = saved
        .prepared()
        .is_some_and(|prepared| prepared["backend"] == "native")
        && native_mode_interruption;
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
            || (saved.provider_failed()
                && !provider_retry_paid(saved, state)
                && saved.confirmed_provider_failure().is_none())
            || (saved.managed() && saved.completed() && saved.terminal_code() == Some(0))
            || saved.settled().is_some()
            || controller_budget_exhausted(saved, state, run["nativeModeInterruption"].is_string())
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
    if initial_state.cancel_requested_at.is_none()
        && checkpoint
            .as_ref()
            .and_then(RunCheckpoint::pending_native_cleanup)
            .is_some_and(recovery::PendingNativeCleanup::awaiting_retirement)
    {
        return Err(Error::unavailable(
            "Waiting for the previous native environment to finish cleanup.",
        ));
    }
    let parked = matches!(
        initial_state.status,
        RunStatus::Waiting | RunStatus::Succeeded
    ) || (initial_state.trigger != "chat"
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
        && (saved.cleanup_observation_pending()
            || (saved.terminal_code.is_none()
                && !matches!(saved.remaining_ms, Some(Some(i64::MIN..=0)))))
        && let Some(attempt) = saved.runner_id().map(str::to_owned)
    {
        let cancel = CancellationToken::new();
        let node = saved.node_id.clone().flatten();
        let observation = recovery::terminal_code(
            s,
            &attempt,
            node.as_deref(),
            &cancel,
            recovery_observation_limit(recovery_deadline),
            saved.cleanup_observation_pending(),
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
                                        current.record_controller_observation(
                                            db, &id, &attempt, code,
                                        )?;
                                    }
                                }
                                recovery::ObservationAction::PublishCleanup(result) => {
                                    if state.cancel_requested_at.is_none()
                                        && !shutdown.is_cancelled()
                                    {
                                        current.publish_cleanup_observation(
                                            db, &id, &attempt, result,
                                        )?;
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
        );
        tokio::pin!(observation);
        let cancellation = async {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if s.store
                    .run_state(run_id)
                    .await?
                    .cancel_requested_at
                    .is_some()
                {
                    return Ok::<_, Error>(());
                }
            }
        };
        tokio::select! {
            result = &mut observation => result?,
            result = cancellation => {
                cancel.cancel();
                observation.await?;
                result?;
            }
        }
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
    if !parked
        && !cannot_replay
        && !crate::nodes::moves::node_ready_for_attempt(s, run, expected_checkpoint.as_ref())
            .await?
    {
        return Ok(false);
    }
    let fenced = recovery::fence_attempt(s, run, expected_checkpoint.as_ref()).await;
    if matches!(
        fenced,
        Ok(recovery::FenceDisposition::DeferredNativeCleanup)
    ) {
        // The durable observer owns settlement. Ordinary recovery must not
        // clear credentials or publish a wait reason after that handoff.
        return Ok(false);
    }
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
    if cannot_replay
        && !cannot_replay_after_fence
        && !parked
        && !crate::nodes::moves::node_ready_for_attempt(s, run, expected_fenced.as_ref()).await?
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

fn recovery_observation_limit(deadline: Option<i64>) -> Duration {
    deadline.map_or(Duration::from_secs(5), |deadline| {
        Duration::from_millis(deadline.saturating_sub(now()).clamp(0, 5000) as u64)
    })
}

/// Settle the retiring native turn before detachment replaces its checkpoint.
pub(crate) fn finish_native_cleanup_before_detach(
    db: &Db<'_>,
    run_id: &str,
    saved: &RunCheckpoint,
) -> Result<bool> {
    use crate::recovery::DeferredCleanupState;

    let Some(run) = db
        .run_state(run_id)?
        .filter(|run| run.status == RunStatus::Succeeded && run.recovery_pending)
    else {
        return Ok(false);
    };
    if let Some(pending) = saved.pending_native_cleanup() {
        if pending.awaiting_retirement() {
            return Ok(false);
        }
        let mut saved = saved.clone();
        if pending.state == DeferredCleanupState::Observing {
            let mut pending = pending.clone();
            pending.state = DeferredCleanupState::Unconfirmed;
            saved.pending_native_cleanup = Some(pending);
        }
        finish_recovery(db, run_id, Some(&saved))?;
        return Ok(true);
    }
    if run.trigger != "chat" || !RunCheckpoint::finishing_native_turn_in(db, run_id, true)? {
        return Ok(false);
    }

    let finished_at = saved
        .settled()
        .and_then(|turn| turn.finished_at)
        .or(run.finished_at);
    if run.cancel_requested_at.is_some() {
        cancel_recovery(db, run_id, &run, finished_at)?;
        return Ok(true);
    }
    let failure = saved
        .runner_id()
        .filter(|attempt| saved.finalization_failure_attempt.as_deref() == Some(*attempt))
        .and_then(|_| saved.finalization_error());
    if failure.is_none() && saved.native_flush_confirmed() {
        finish_recovery(db, run_id, Some(saved))?;
        return Ok(true);
    }
    crate::chats::continuation::cancel_for_run(db, run_id)?;
    let mut outcome = Settled::failure(
        RunStatus::Failed,
        failure.unwrap_or(
            "Native disk cleanup could not be confirmed before the environment expired.",
        ),
    );
    outcome.finished_at = finished_at.or(outcome.finished_at);
    db.change_run(run_id, outcome.run_changes())?;
    Ok(true)
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
    let observed_control_failure = if let Some(saved) = checkpoint {
        super::held::take_observed_control_failure(db, id, saved)?
    } else {
        None
    };
    let cleanup_finished_at = checkpoint
        .filter(|saved| saved.pending_native_cleanup().is_some() || saved.native_cleanup_failure())
        .and_then(RunCheckpoint::settled)
        .filter(|settled| settled.status == RunStatus::Succeeded)
        .and_then(|settled| settled.finished_at);
    let cleanup_failure = |message| {
        let mut failure = Settled::failure(RunStatus::Failed, message);
        // Messages queued after the completed turn still belong to the next turn.
        failure.finished_at = cleanup_finished_at.or(failure.finished_at);
        failure.run_changes()
    };

    let mut published = None;
    if let Some(saved) = checkpoint
        && let Some(pending) = saved.pending_native_cleanup()
    {
        use crate::recovery::DeferredCleanupState;

        if pending.awaiting_retirement() && current.cancel_requested_at.is_none() {
            return Ok(false);
        }
        let mut saved = saved.clone();
        if current.cancel_requested_at.is_none()
            && observed_control_failure.is_none()
            && current.status == RunStatus::Succeeded
            && current.recovery_pending
        {
            match pending.state {
                DeferredCleanupState::Observing => return Ok(false),
                DeferredCleanupState::Unconfirmed => saved.record_controller_result(
                    db,
                    id,
                    &pending.attempt,
                    Some(pending.clone().failure()),
                )?,
                DeferredCleanupState::Retired | DeferredCleanupState::AwaitingRetirement => {}
            }
        }
        // Retain the fence across turn boundaries while this disk still needs
        // retirement. A later turn can wait for proof without replaying this one.
        saved.pending_native_cleanup =
            (pending.state != DeferredCleanupState::Retired).then(|| {
                let mut pending = pending.clone();
                pending.state = DeferredCleanupState::AwaitingRetirement;
                pending
            });
        saved.store_in(db, id)?;
        published = Some(saved);
    }
    let checkpoint = published.as_ref().or(checkpoint);
    if current.cancel_requested_at.is_some() {
        let completed_at = (current.status == RunStatus::Succeeded && current.recovery_pending)
            .then_some(current.finished_at)
            .flatten();
        cancel_recovery(db, id, current, cleanup_finished_at.or(completed_at))?;
        return Ok(false);
    }
    if let Some(checkpoint) = checkpoint {
        // A controller can report a failed physical close only during DELETE,
        // after the provider outcome was tentatively settled. Preserve explicit
        // user cancellation above, but never let that older outcome hide it.
        if checkpoint.runner_id().is_some()
            && checkpoint.runner_id() == checkpoint.finalization_failure_attempt.as_deref()
            && let Some(error) = checkpoint.finalization_error()
        {
            crate::chats::continuation::cancel_for_run(db, id)?;
            db.change_run(id, cleanup_failure(error))?;
            return Ok(false);
        }
        if let Some(message) = observed_control_failure
            && checkpoint.settled().is_none_or(|outcome| {
                outcome.status == RunStatus::Succeeded
                    || outcome.error.as_deref() == Some(super::held::DEADLINE_MESSAGE)
            })
        {
            crate::chats::continuation::cancel_for_run(db, id)?;
            db.event(id, &EventKind::Error, &message, None)?;
            db.change_run(id, cleanup_failure(&message))?;
            return Ok(false);
        }
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
        if checkpoint.provider_failed() && super::retry::expire_in(db, id)? {
            return Ok(false);
        }
        let provider_paid = provider_retry_paid(checkpoint, current)
            || RunCheckpoint::load_in(db, id)?
                .as_ref()
                .is_some_and(|persisted| {
                    recovery::same_execution(Some(persisted), Some(checkpoint))
                        && provider_retry_paid(persisted, current)
                });
        if let Some(cause) = checkpoint.confirmed_provider_failure()
            && !provider_paid
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
        if checkpoint.provider_failed() && !provider_paid {
            let message = if checkpoint.last_error().is_empty() {
                "Agent turn failed."
            } else {
                checkpoint.last_error()
            };
            let mut patch = Settled::failure(RunStatus::Failed, message).run_changes();
            patch.wakeup = Some(None);
            if let Some(mut retry) = current.retry.clone()
                && retry.next_attempt_at.take().is_some()
            {
                patch.retry = Some(Some(retry));
            }
            db.change_run(id, patch)?;
            return Ok(false);
        }
        if (checkpoint.terminal_code() == Some(0) && !checkpoint.provider_failed())
            || checkpoint.completed()
        {
            let summary = match checkpoint.last_message() {
                "" => {
                    "Conversation completed before worker restart. See Activity for the recorded result."
                }
                message => message,
            };
            let waiting = current.trigger != "chat" && record.wakeup.is_some();
            let fresh_wait = record.boundary["environmentState"] == "expired"
                && record.boundary["sessionHandoff"].is_string();
            if waiting && current.session_id.is_none() && !fresh_wait {
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
        if !provider_paid
            && controller_budget_exhausted(
                checkpoint,
                current,
                record.placement.native_mode_interruption.is_some(),
            )
        {
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
        && !RunCheckpoint::load_in(db, id)?
            .as_ref()
            .is_some_and(|persisted| {
                recovery::same_execution(Some(persisted), Some(saved))
                    && provider_retry_paid(persisted, current)
            })
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
fn cancel_recovery(db: &Db<'_>, id: &str, run: &RunState, finished_at: Option<i64>) -> Result<()> {
    db.change_run(
        id,
        crate::runs::RunChanges {
            status: Some(RunStatus::Cancelled),
            finished_at: Some(Some(finished_at.unwrap_or_else(now))),
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
            let preserves_success =
                current.status == RunStatus::Succeeded && current.recovery_pending;
            let mut patch = crate::runs::RunChanges {
                status: (!preserves_success).then_some(RunStatus::Queued),
                recovery_pending: Some(true),
                finished_at: (!preserves_success).then_some(None),
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
use crate::attempt_fixture::*;

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
