use crate::{
    config::now,
    error::Result,
    recovery::ProcessIdentity,
    run_status::RunStatus,
    store::{Db, Store},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{
    Mutex as SyncMutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::Mutex;

/// Absent (`None`), explicitly `null` (`Some(None)`) or set (`Some(Some(_))`).
type Nullable<T> = Option<Option<T>>;

/// The checkpoint identities needed to verify node storage ownership.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NodeOwnership {
    pub node_id: Option<String>,
    pub runner_id: Option<String>,
}

/// Restart state persisted once in `run_checkpoints`, serialized from this type.
///
/// Unconverted chat/node boundaries still serialize these named fields. Nullable
/// assignments retain null versus absent; no untyped decision fields round-trip.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunCheckpoint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launched: Option<bool>,
    /// Small durable row, loaded separately from the retained workspace.
    #[serde(skip)]
    pub progress: Option<super::progress::Progress>,
    /// Preserved when an engine session is replaced on the same workspace disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_initialized: Option<bool>,
    /// Explicit controller rejection of this attempt; absent is not proof of rejection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_rejected: Option<bool>,
    /// Output was truncated for this attempt; completion may be missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_interrupted: Option<bool>,
    /// A new explicit turn is queued but has not yet acquired an attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_pending: Option<bool>,
    /// Absolute deadline while fencing an interrupted attempt, retained across crashes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_deadline_ms: Option<i64>,
    /// `null` means the run has no time limit; absent means its budget is spent.
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub remaining_ms: Nullable<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub prepared: Nullable<Value>,
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub runner_id: Nullable<String>,
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub node_id: Nullable<String>,
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub runtime_id: Nullable<Value>,
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub process: Nullable<ProcessIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<bool>,
    /// The adapter confirmed failure, independently of controller transport/exit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_failed: Option<bool>,
    /// Controller result for runner_id, distinct from the provider turn and logs.
    /// Absent: an attempt may need observation. Null: observation was exhausted
    /// or a new turn invalidated the old result; recovery must only fence it.
    /// Neither absent nor null proves success.
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub terminal_code: Nullable<i32>,
    /// Durable reservations for reads of one controller attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_observation: Option<ControllerObservation>,
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub last_error: Nullable<String>,
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub retry_cause: Nullable<crate::run_retry::Cause>,
    /// A classified turn.failed durably recorded for this exact managed attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_failure_attempt: Option<String>,
    /// Controller-confirmed storage finalization failure for the same attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finalization_failure_attempt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_recoveries: Option<u64>,
    /// A recovery already charged to the budget, bound to the stopped attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_recovery_reservation: Option<String>,
    /// A paid move recovery that reached its destination, bound to the stopped attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_recovery_reservation: Option<String>,
    /// A provider retry already scheduled and charged to this attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_retry_reservation: Option<String>,
    /// Previously paid admission carried into an attempt until its start is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_paid_admission: Option<PaidAdmission>,
    /// A controller's capacity refusal delays admission without consuming a recovery attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity_retry_at: Option<i64>,
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub settled: Nullable<Settled>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fresh_session: Option<bool>,
    /// The node domain owns the resource negotiation at this boundary (A9/A10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<crate::nodes::Resources>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub administrative_pause_observation: Option<AdministrativePauseObservation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaidAdmission {
    pub attempt: String,
    pub controller: bool,
    #[serde(default)]
    pub capacity_refused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_cause: Option<crate::run_retry::Cause>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerObservation {
    pub attempt: String,
    pub requests: u8,
    #[serde(default)]
    pub in_flight: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdministrativePauseObservation {
    pub attempt: String,
    pub total_ms: u64,
}

/// Final run fields recorded before the previous execution was fenced. They are
/// applied to the run only once fencing confirms nothing is still running.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settled {
    pub status: RunStatus,
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub finished_at: Option<i64>,
}

impl Settled {
    pub fn failure(status: RunStatus, summary: &str) -> Self {
        Self {
            status,
            summary: Some(summary.to_owned()),
            error: Some(summary.to_owned()),
            finished_at: Some(now()),
        }
    }

    /// Captures the outcome already recorded by a typed lifecycle transition.
    pub fn from_state(run: &cairn_contracts::RunState) -> Self {
        Self {
            status: run.status,
            summary: Some(run.summary.clone()),
            error: None,
            finished_at: run.finished_at,
        }
    }

    pub fn run_changes(&self) -> crate::runs::RunChanges {
        crate::runs::RunChanges {
            status: Some(self.status),
            summary: self.summary.clone(),
            error: self.error.clone().map(Some),
            finished_at: Some(self.finished_at),
            account_wait_reason: Some(None),
            recovery_pending: Some(false),
            retry: (self.status == RunStatus::Cancelled).then_some(None),
            ..Default::default()
        }
    }
}

impl RunCheckpoint {
    pub fn reserve_observation(&mut self, attempt: &str) -> bool {
        if self.runner_id() != Some(attempt)
            || self.terminal_code.is_some()
            || self.settled().is_some()
        {
            return false;
        }
        let observation =
            self.controller_observation
                .get_or_insert_with(|| ControllerObservation {
                    attempt: attempt.to_owned(),
                    requests: 0,
                    in_flight: false,
                });
        if observation.attempt != attempt {
            *observation = ControllerObservation {
                attempt: attempt.to_owned(),
                requests: 0,
                in_flight: false,
            };
        }
        if observation.requests >= 3 || observation.in_flight {
            return false;
        }
        observation.requests += 1;
        observation.in_flight = true;
        true
    }

    pub fn release_observation(&mut self, attempt: &str) {
        if let Some(observation) = &mut self.controller_observation
            && observation.attempt == attempt
        {
            observation.in_flight = false;
        }
    }

    pub fn fresh(budget_ms: Option<i64>) -> Self {
        Self {
            launched: Some(false),
            remaining_ms: Some(budget_ms),
            ..Self::default()
        }
    }

    /// Placeholder saved when a run is recovered without any checkpoint.
    pub fn exhausted() -> Self {
        Self {
            remaining_ms: Some(Some(0)),
            ..Self::default()
        }
    }

    pub fn from_value(value: Value) -> Result<Self> {
        Ok(serde_json::from_value(value)?)
    }

    pub fn to_value(&self) -> Result<Value> {
        Ok(serde_json::to_value(self)?)
    }

    pub async fn load(store: &Store, run_id: &str) -> Result<Option<Self>> {
        let run_id = run_id.to_owned();
        store.read(move |db| Self::load_in(db, &run_id)).await
    }

    pub fn load_in(db: &Db<'_>, run_id: &str) -> Result<Option<Self>> {
        let encoded: Option<String> =
            db.0.prepare_cached("SELECT state FROM run_checkpoints WHERE run_id=?")?
                .query_row([run_id], |row| row.get(0))
                .optional()?;
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        let mut saved: Self = serde_json::from_str(&encoded)?;
        let progress: Option<String> =
            db.0.prepare_cached("SELECT state FROM run_progress WHERE run_id=?")?
                .query_row([run_id], |row| row.get(0))
                .optional()?;
        saved.progress = progress
            .map(|value| serde_json::from_str(&value))
            .transpose()?;
        Ok(Some(saved))
    }

    /// A display decision must not decode the retained workspace, tool sources,
    /// process identity and recovery state merely to inspect this flag.
    pub(crate) fn fresh_session_in(db: &Db<'_>, run_id: &str) -> Result<bool> {
        let value: Option<Option<bool>> =
            db.0.prepare_cached(
                "SELECT json_extract(state,'$.freshSession') FROM run_checkpoints WHERE run_id=?",
            )?
            .query_row([run_id], |row| row.get(0))
            .optional()?;
        Ok(value.flatten().unwrap_or(false))
    }

    pub async fn store(&self, store: &Store, run_id: &str) -> Result<()> {
        let (state, run_id) = (self.clone(), run_id.to_owned());
        store.write(move |db| state.store_in(db, &run_id)).await
    }

    pub fn store_in(&self, db: &Db<'_>, run_id: &str) -> Result<()> {
        db.0.prepare_cached(
            "INSERT INTO run_checkpoints(run_id,state) VALUES(?,?) ON CONFLICT(run_id) DO UPDATE SET state=excluded.state"
        )?.execute(params![run_id, serde_json::to_string(self)?])?;
        if let Some(progress) = &self.progress {
            progress.store_in(db, run_id)?;
        }
        Ok(())
    }

    pub fn launched(&self) -> bool {
        self.launched == Some(true)
    }

    pub fn completed(&self) -> bool {
        self.completed == Some(true)
    }

    pub fn provider_failed(&self) -> bool {
        self.provider_failed == Some(true)
    }

    pub fn terminal_code(&self) -> Option<i32> {
        self.terminal_code.flatten()
    }

    /// A healthy controller exit cannot prove that the provider finished its turn.
    pub fn output_error(&self) -> Option<&'static str> {
        if !self.managed() || self.completed() || self.provider_failed() {
            return None;
        }
        if self.confirmed_provider_failure().is_some() {
            return None;
        }
        if self.output_interrupted == Some(true) {
            return Some(
                "Execution output was interrupted before the agent's completion was recorded. The response may be incomplete; automatic replay was stopped.",
            );
        }
        (self.terminal_code() == Some(0)).then_some("Execution ended before the agent's completion was recorded. The response may be incomplete; automatic replay was stopped.")
    }

    /// A classified provider ending is authoritative only for its own attempt
    /// and a confirmed ordinary failure, never a controller/finalization fault.
    pub fn confirmed_provider_failure(&self) -> Option<&crate::run_retry::Cause> {
        if !self.managed()
            || self.completed()
            || self.terminal_code() != Some(1)
            || self.runner_id().is_none()
            || self.runner_id() != self.provider_failure_attempt.as_deref()
        {
            return None;
        }
        self.retry_cause.as_ref().and_then(Option::as_ref)
    }

    pub fn finalization_error(&self) -> Option<&str> {
        if self.runner_id().is_some()
            && self.runner_id() == self.finalization_failure_attempt.as_deref()
        {
            return Some(
                "The filesystem could not be flushed. Working files were preserved; automatic replay was stopped.",
            );
        }
        if !self.managed() || !self.completed() || self.terminal_code() == Some(0) {
            return None;
        }
        Some(if self.last_error().is_empty() {
            "Agent turn completed, but execution finalization was not confirmed healthy. Working files were preserved; automatic replay was stopped."
        } else {
            self.last_error()
        })
    }

    /// Commit the controller's typed finalization evidence with its status.
    pub fn record_controller_result(
        &mut self,
        db: &Db<'_>,
        id: &str,
        attempt: &str,
        result: Option<crate::runner::ControllerResult>,
    ) -> Result<()> {
        if self.runner_id() != Some(attempt) {
            return Err(crate::error::Error::conflict(
                "Execution attempt changed during observation.",
            ));
        }
        self.terminal_code = Some(result.map(|result| result.code));
        if result.is_some_and(|result| result.physical_finalized && !result.finalization_failed)
            && let Some(progress) = &mut self.progress
            && progress.runner_id.as_deref() == Some(attempt)
        {
            progress.confirm_finalized(now());
        }
        if result.is_some_and(|result| result.finalization_failed)
            && self.finalization_failure_attempt.as_deref() != Some(attempt)
        {
            self.finalization_failure_attempt = Some(attempt.to_owned());
            db.event(id, &cairn_contracts::EventKind::Error, "The filesystem could not be flushed. Working files were preserved; automatic replay was stopped.", None)?;
        }
        Ok(())
    }

    /// The prepared execution, when the workspace has been prepared.
    pub fn prepared(&self) -> Option<&Value> {
        self.prepared.as_ref()?.as_ref().filter(|p| p.is_object())
    }

    pub fn isolated(&self) -> bool {
        self.prepared().is_some_and(|p| p["isolated"] == true)
    }

    pub fn managed(&self) -> bool {
        self.prepared()
            .is_some_and(|p| matches!(p["backend"].as_str(), Some("firecracker" | "native")))
    }

    pub fn runner_id(&self) -> Option<&str> {
        self.runner_id.as_ref()?.as_deref()
    }

    pub fn process(&self) -> Option<&ProcessIdentity> {
        self.process.as_ref()?.as_ref()
    }

    pub fn last_error(&self) -> &str {
        self.last_error
            .as_ref()
            .and_then(Option::as_deref)
            .unwrap_or("")
    }

    pub fn last_message(&self) -> &str {
        self.last_message.as_deref().unwrap_or("")
    }

    pub fn controller_recoveries(&self) -> u64 {
        self.controller_recoveries.unwrap_or(0)
    }

    /// Transfer a paid authorization to a candidate attempt, without allowing
    /// it to be reused until an explicit capacity rejection proves no start.
    pub fn bind_paid_admission(&mut self, runner: &str, state: &cairn_contracts::RunState) {
        if self.runner_id() != Some(runner) {
            let controller =
                self.controller_recoveries() > 0 && self.has_controller_recovery_reservation();
            let provider_paid = self.turn_pending != Some(true)
                && self.has_provider_retry_reservation()
                && state.retry.as_ref().is_some_and(|retry| {
                    (retry.next_attempt_at.is_some()
                        || state.status == cairn_contracts::RunStatus::Running)
                        && self.retry_cause.as_ref().and_then(Option::as_ref) == Some(&retry.cause)
                });
            let provider_cause = if provider_paid {
                state.retry.as_ref().map(|retry| retry.cause.clone())
            } else {
                None
            };
            self.pending_paid_admission =
                (controller || provider_cause.is_some()).then(|| PaidAdmission {
                    attempt: runner.to_owned(),
                    controller,
                    capacity_refused: false,
                    provider_cause,
                });
        }
        self.runner_id = Some(Some(runner.to_owned()));
        self.provider_failure_attempt = None;
        self.finalization_failure_attempt = None;
        self.controller_recovery_reservation = None;
        self.move_recovery_reservation = None;
        self.provider_retry_reservation = None;
    }

    pub fn has_provider_retry_reservation(&self) -> bool {
        self.runner_id()
            .is_some_and(|id| self.provider_retry_reservation.as_deref() == Some(id))
    }

    pub fn has_controller_recovery_reservation(&self) -> bool {
        self.runner_id()
            .is_some_and(|id| self.controller_recovery_reservation.as_deref() == Some(id))
    }

    pub fn settled(&self) -> Option<&Settled> {
        self.settled.as_ref()?.as_ref()
    }

    /// Absolute deadline for a checkpoint resumed now.
    fn deadline(&self) -> Option<i64> {
        match self.remaining_ms {
            Some(None) => None,
            Some(Some(remaining)) => Some(now() + remaining),
            None => Some(now()),
        }
    }
}

/// Effects of one redacted engine frame. A streamed message changes only its
/// last-message field; it never clones the prepared workspace per token.
pub struct EventChanges {
    pub run: Option<crate::runs::RunChanges>,
    pub failure: Option<(String, Option<crate::run_retry::Cause>, bool)>,
    pub completed: bool,
    pub last_message: Option<String>,
}

/// Live checkpoint of the run this worker is executing.
pub struct Checkpoint {
    store: Store,
    pub id: String,
    state: Mutex<RunCheckpoint>,
    deadline_base: Option<i64>,
    administrative_pause_ms: AtomicU64,
    pause_observation: SyncMutex<(String, u64)>,
}

impl Checkpoint {
    pub fn new(store: Store, id: String, state: RunCheckpoint) -> Self {
        let previous = state
            .administrative_pause_observation
            .clone()
            .unwrap_or_default();
        Self {
            deadline_base: state.deadline(),
            administrative_pause_ms: AtomicU64::new(0),
            pause_observation: SyncMutex::new((previous.attempt, previous.total_ms)),
            state: Mutex::new(state),
            store,
            id,
        }
    }

    pub fn expired(&self) -> bool {
        self.deadline().is_some_and(|deadline| now() >= deadline)
    }

    pub fn deadline(&self) -> Option<i64> {
        self.deadline_base.map(|deadline| {
            deadline.saturating_add(
                self.administrative_pause_ms
                    .load(Ordering::SeqCst)
                    .min(i64::MAX as u64) as i64,
            )
        })
    }

    // A deadline including pause credit and its cumulative counter must be
    // captured together. account_pause holds this same lock while changing both.
    fn budget_snapshot(&self) -> (Option<i64>, AdministrativePauseObservation) {
        let previous = self.pause_observation.lock().unwrap();
        let observation = AdministrativePauseObservation {
            attempt: previous.0.clone(),
            total_ms: previous.1,
        };
        (self.deadline(), observation)
    }

    pub fn account_pause(&self, attempt: &str, total_ms: u64) {
        let mut previous = self.pause_observation.lock().unwrap();
        if previous.0 != attempt {
            *previous = (attempt.into(), 0);
        }
        if total_ms > previous.1 {
            self.administrative_pause_ms
                .fetch_add(total_ms - previous.1, Ordering::SeqCst);
            previous.1 = total_ms;
        }
    }

    pub async fn refresh_pause(&self, service: &crate::service::Service) -> Result<()> {
        let state = self.snapshot().await;
        if state.managed()
            && let Some(attempt) = state.runner_id()
        {
            let total =
                crate::nodes::exclusive::administrative_pause_ms(service, &self.id, attempt)
                    .await?;
            self.account_pause(attempt, total);
        }
        Ok(())
    }

    pub async fn snapshot(&self) -> RunCheckpoint {
        self.state.lock().await.clone()
    }

    pub async fn read<T>(&self, read: impl FnOnce(&RunCheckpoint) -> T) -> T {
        read(&*self.state.lock().await)
    }

    /// Applies a checkpoint change only after its durable write succeeds.
    pub async fn update(&self, change: impl FnOnce(&mut RunCheckpoint)) -> Result<()> {
        self.update_and_publish(change, |_| Ok(())).await
    }

    /// Publishes a launcher checkpoint and its corresponding run changes/events
    /// together. Neither live readers nor this worker see a partial transition.
    pub async fn update_and_publish(
        &self,
        change: impl FnOnce(&mut RunCheckpoint),
        publish: impl FnOnce(&mut Db<'_>) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.update_and_publish_if(
            move |saved| {
                change(saved);
                true
            },
            publish,
        )
        .await
    }

    /// Commits an event only when its checkpoint transition is new.
    pub async fn update_and_publish_if(
        &self,
        change: impl FnOnce(&mut RunCheckpoint) -> bool,
        publish: impl FnOnce(&mut Db<'_>) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        let mut state = self.state.lock().await;
        let mut saved = state.clone();
        if !change(&mut saved) {
            return Ok(());
        }
        let (deadline, observation) = self.budget_snapshot();
        saved.administrative_pause_observation = Some(observation);
        let id = self.id.clone();
        let saved = self
            .store
            .transaction(move |db| {
                saved.remaining_ms =
                    Some(deadline.map(|deadline| deadline.saturating_sub(now()).max(0)));
                saved.store_in(db, &id)?;
                publish(db)?;
                Ok(saved)
            })
            .await?;
        *state = saved;
        Ok(())
    }

    /// Decide a run transition and its checkpoint under the same store transaction.
    pub async fn decide<T: Send + 'static>(
        &self,
        decision: impl FnOnce(&mut RunCheckpoint, &mut Db<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let mut state = self.state.lock().await;
        let expected = state.clone();
        let fallback = expected.clone();
        let id = self.id.clone();
        let (deadline, observation) = self.budget_snapshot();
        let (saved, value) = self
            .store
            .transaction(move |db| {
                let current = RunCheckpoint::load_in(db, &id)?;
                if current.is_none()
                    && (expected.runner_id().is_some() || expected.process().is_some())
                {
                    return Err(crate::error::Error::conflict(
                        "Execution checkpoint disappeared during settlement.",
                    ));
                }
                let mut saved = current.unwrap_or(fallback);
                if !crate::recovery::same_execution(Some(&saved), Some(&expected)) {
                    return Err(crate::error::Error::conflict(
                        "Execution attempt changed during settlement.",
                    ));
                }
                saved.remaining_ms =
                    Some(deadline.map(|deadline| deadline.saturating_sub(now()).max(0)));
                saved.administrative_pause_observation = Some(observation);
                let value = decision(&mut saved, db)?;
                saved.store_in(db, &id)?;
                Ok((saved, value))
            })
            .await?;
        *state = saved;
        Ok(value)
    }

    /// Publishes engine state, its checkpoint and the event in one commit. A
    /// streamed text snapshot only changes memory; it does not rewrite the
    /// checkpoint per token. Memory advances only after successful publication.
    pub async fn record_event(
        &self,
        kind: cairn_contracts::EventKind,
        text: String,
        payload: Value,
        changes: EventChanges,
    ) -> Result<()> {
        let mut state = self.state.lock().await;
        let failure = changes.failure.filter(|(_, _, provider)| {
            state.last_error().is_empty() || (*provider && !state.provider_failed())
        });
        let mut progress = state.progress.clone();
        let progress_changed = progress
            .as_mut()
            .is_some_and(|progress| progress.observe(&payload, now()));
        let publish_progress = progress_changed
            && progress.as_ref().is_some_and(|next| {
                state.progress.as_ref().is_none_or(|old| {
                    next.phase != old.phase
                        || next.complete != old.complete
                        || next.finalized_at != old.finalized_at
                        || now().saturating_sub(old.persisted_at) >= 1000
                })
            });
        // Streamed snapshots retain bounded digests in memory. Publish the small
        // progress row at most once a second (phase/coverage immediately), without
        // cloning or serializing the prepared workspace for every token.
        if publish_progress && let Some(progress) = &mut progress {
            progress.persisted_at = now();
        }
        let persist = changes.completed || failure.is_some();
        let mut last_message = changes.last_message;
        let mut saved = persist.then(|| state.clone());
        if let Some(saved) = &mut saved {
            saved.progress = progress.clone();
            if let Some((message, cause, provider_failure)) = failure {
                if provider_failure && cause.is_some() {
                    saved.provider_failure_attempt = saved.runner_id().map(str::to_owned);
                }
                saved.last_error = Some(Some(message));
                saved.retry_cause = Some(cause);
                if provider_failure {
                    saved.provider_failed = Some(true);
                    saved.completed = Some(false);
                    // The provider ending does not prove controller finalization;
                    // leave the attempt observable if the worker crashes here.
                    saved.terminal_code = None;
                }
            }
            if let Some(message) = last_message.take() {
                saved.last_message = Some(message);
            }
            if changes.completed {
                saved.completed = Some(true);
                saved.provider_failed = None;
            }
        }
        let deadline = if let Some(saved) = &mut saved {
            let (deadline, observation) = self.budget_snapshot();
            saved.administrative_pause_observation = Some(observation);
            deadline
        } else {
            None
        };
        let id = self.id.clone();
        let expected_progress = state.progress.clone();
        let published_progress = publish_progress.then(|| progress.clone()).flatten();
        let saved = self
            .store
            .transaction(move |db| {
                if let Some(expected) = &expected_progress
                    && !expected.bound_in(db, &id)?
                {
                    return Err(crate::error::Error::conflict(
                        "Execution changed while recording progress.",
                    ));
                }
                if let Some(progress) = &published_progress {
                    progress.store_in(db, &id)?;
                }
                if let Some(saved) = &mut saved {
                    saved.remaining_ms =
                        Some(deadline.map(|deadline| deadline.saturating_sub(now()).max(0)));
                }
                if let Some(changes) = changes.run {
                    db.change_run(&id, changes)?;
                }
                if let Some(saved) = &saved {
                    saved.store_in(db, &id)?;
                }
                db.event(&id, &kind, &text, Some(&payload))?;
                Ok(saved)
            })
            .await?;
        if let Some(saved) = saved {
            *state = saved;
        } else {
            state.progress = progress;
            if let Some(message) = last_message {
                state.last_message = Some(message);
            }
        }
        Ok(())
    }

    pub async fn persist(&self) -> Result<()> {
        self.update(|_| {}).await
    }
}

/// Serde adapter keeping an explicit `null` distinct from an absent field.
mod nullable {
    use super::Nullable;
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
    use serde_json::Value;

    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Nullable<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .as_ref()
            .and_then(Option::as_ref)
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: DeserializeOwned>(
        deserializer: D,
    ) -> Result<Nullable<T>, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if value.is_null() {
            return Ok(Some(None));
        }
        T::deserialize(value)
            .map(|value| Some(Some(value)))
            .map_err(serde::de::Error::custom)
    }
}

// Temporary JSON adapters are owned by the unconverted node/chat domains. They
// cannot persist arbitrary checkpoint fields or create a second source of truth.
impl Db<'_> {
    /// Storage eligibility never needs the prepared workspace or tool sources.
    /// Decode the projected JSON so arrays, objects and numbers remain invalid
    /// identities, just as they are in the full checkpoint contract.
    pub(crate) fn checkpoint_node_ownership(&self, run_id: &str) -> Result<Option<NodeOwnership>> {
        Ok(self
            .json_rows_as(
                "SELECT json_object(
                    'nodeId',json(state -> '$.nodeId'),
                    'runnerId',json(state -> '$.runnerId')
                 ) FROM run_checkpoints WHERE run_id=?",
                [run_id],
            )?
            .pop())
    }

    pub fn checkpoint_boundary(&self, run_id: &str) -> Result<Option<Value>> {
        RunCheckpoint::load_in(self, run_id)?
            .map(|state| state.to_value())
            .transpose()
    }

    pub fn save_checkpoint_boundary(&self, run_id: &str, value: &Value) -> Result<()> {
        RunCheckpoint::from_value(value.clone())?.store_in(self, run_id)
    }

    pub fn delete_checkpoint(&self, run_id: &str) -> Result<()> {
        self.0
            .execute("DELETE FROM run_checkpoints WHERE run_id=?", [run_id])?;
        Ok(())
    }
}

impl Store {
    pub async fn checkpoint_boundary(&self, run_id: &str) -> Result<Option<Value>> {
        let run_id = run_id.to_owned();
        self.read(move |db| db.checkpoint_boundary(&run_id)).await
    }

    pub async fn save_checkpoint_boundary(&self, run_id: &str, value: Value) -> Result<()> {
        RunCheckpoint::from_value(value)?.store(self, run_id).await
    }

    pub async fn delete_checkpoint(&self, run_id: &str) -> Result<()> {
        let run_id = run_id.to_owned();
        self.write(move |db| db.delete_checkpoint(&run_id)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_fixture::DbRunFixture;
    use serde_json::json;

    #[tokio::test]
    async fn node_ownership_reads_nullable_ids_without_prepared_execution_data() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.transaction(|db| {
            for (id, state) in [
                ("absent", json!({})),
                ("null", json!({ "nodeId": null, "runnerId": null })),
                ("set", json!({ "nodeId": "node", "runnerId": "attempt", "prepared": { "instructions": "private".repeat(16384) } })),
            ] {
                db.add_run_fixture(&json!({ "id": id, "taskId": id, "status": "succeeded" }), None)?;
                db.save_checkpoint_boundary(id, &state)?;
                let compact = db.checkpoint_node_ownership(id)?.unwrap();
                assert_eq!(compact.node_id.as_deref(), state["nodeId"].as_str());
                assert_eq!(compact.runner_id.as_deref(), state["runnerId"].as_str());
            }
            assert!(db.checkpoint_node_ownership("missing")?.is_none());
            for state in [
                json!({ "nodeId": 42 }), json!({ "runnerId": 42 }),
                json!({ "nodeId": {} }), json!({ "runnerId": [] }),
                json!({ "nodeId": true }), json!({ "runnerId": 1.5 }),
            ] {
                db.0.execute("UPDATE run_checkpoints SET state=? WHERE run_id='set'", [state.to_string()])?;
                assert!(db.checkpoint_node_ownership("set").is_err());
            }
            Ok(())
        }).await.unwrap();
    }

    #[test]
    fn round_trips_named_checkpoint_fields_and_preserves_null_versus_absent() {
        let stored = json!({
            "launched": true,
            "workspaceInitialized": true,
            "remainingMs": null,
            "process": null,
            "settled": null,
            "prepared": { "isolated": true, "backend": "firecracker" },
            "runnerId": "runner",
            "freshSession": true,
            "administrativePauseObservation": {"attempt":"attempt", "totalMs":123},
        });
        let checkpoint = RunCheckpoint::from_value(stored.clone()).unwrap();
        assert!(checkpoint.launched());
        assert_eq!(checkpoint.workspace_initialized, Some(true));
        assert_eq!(checkpoint.remaining_ms, Some(None));
        assert_eq!(checkpoint.deadline(), None);
        assert!(checkpoint.isolated() && checkpoint.managed());
        assert_eq!(checkpoint.runner_id(), Some("runner"));
        assert_eq!(checkpoint.to_value().unwrap(), stored);

        let empty = RunCheckpoint::from_value(json!({})).unwrap();
        assert_eq!(empty.to_value().unwrap(), json!({}));
        assert!(empty.deadline().is_some());
    }

    #[test]
    fn mistyped_fields_are_rejected() {
        assert!(RunCheckpoint::from_value(json!({ "unknownDecision": true })).is_err());
        assert!(RunCheckpoint::from_value(json!({ "launched": "yes" })).is_err());
        assert!(RunCheckpoint::from_value(json!({ "remainingMs": 1.5 })).is_err());
    }

    #[tokio::test]
    async fn checkpoint_budget_includes_time_waiting_for_the_store() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({ "id": "deadline-fixture", "taskId": "deadline", "status": "running" }),
                    None,
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let checkpoint = std::sync::Arc::new(Checkpoint::new(
            store.clone(),
            "deadline-fixture".into(),
            RunCheckpoint::fresh(Some(100)),
        ));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocking_store = store.clone();
        let blocker = tokio::spawn(async move {
            blocking_store
                .transaction(move |_| {
                    let _ = entered_tx.send(());
                    release_rx.recv().unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
        entered_rx.await.unwrap();
        let writer = checkpoint.clone();
        let update = tokio::spawn(async move {
            writer
                .update(|saved| saved.terminal_code = Some(Some(0)))
                .await
                .unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        update.await.unwrap();
        let saved = RunCheckpoint::load(&store, "deadline-fixture")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.terminal_code(), Some(0));
        assert_eq!(saved.remaining_ms, Some(Some(0)));
        assert!(checkpoint.expired());
        store
            .transaction(move |db| {
                assert!(!super::super::schedule::finish_recovery(
                    db,
                    "deadline-fixture",
                    Some(&saved)
                )?);
                let state = db.run_state("deadline-fixture")?.unwrap();
                assert_eq!(state.status, RunStatus::Failed);
                assert_eq!(state.error.as_deref(), Some("Run exceeded its time limit."));
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pause_credit_is_monotonic_per_attempt_and_survives_checkpoint_recovery() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({ "id": "budget-fixture", "taskId": "budget", "status": "running" }),
                    None,
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let checkpoint = Checkpoint::new(
            store.clone(),
            "budget-fixture".into(),
            RunCheckpoint::fresh(Some(60_000)),
        );
        let original = checkpoint.deadline().unwrap();
        checkpoint.account_pause("attempt-a", 30_000);
        checkpoint.account_pause("attempt-a", 29_000);
        checkpoint.account_pause("attempt-a", 30_000);
        assert_eq!(checkpoint.deadline(), Some(original + 30_000));
        checkpoint.account_pause("attempt-b", 10_000);
        assert_eq!(checkpoint.deadline(), Some(original + 40_000));
        checkpoint.persist().await.unwrap();
        let saved = RunCheckpoint::load(&store, "budget-fixture")
            .await
            .unwrap()
            .unwrap();
        let recovered = Checkpoint::new(store, "budget-fixture".into(), saved);
        let before = recovered.deadline();
        recovered.account_pause("attempt-b", 10_000);
        assert_eq!(
            recovered.deadline(),
            before,
            "already persisted credit is not counted twice"
        );
        recovered.account_pause("attempt-b", 12_000);
        assert_eq!(
            recovered.deadline(),
            before.map(|deadline| deadline + 2_000)
        );
    }

    #[tokio::test]
    async fn paid_admission_is_bound_to_one_candidate_without_authorizing_an_uncertain_start() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({ "id": "paid-binding", "taskId": "task", "status": "running" }),
                    None,
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let mut state = store.run_state("paid-binding").await.unwrap();
        let cause = crate::run_retry::Cause::classify(
            &json!({ "code": "server_overloaded", "message": "busy" }),
        )
        .unwrap();
        state.retry = Some(crate::run_retry::Retry::next(None, cause.clone(), now()).unwrap());
        state.retry.as_mut().unwrap().next_attempt_at = None;
        for provider in [false, true] {
            let mut saved = RunCheckpoint::from_value(json!({
                "prepared": { "backend": "native", "isolated": true }, "runnerId": "old",
                "controllerRecoveries": 3,
                "controllerRecoveryReservation": if provider { None } else { Some("old") },
                "providerRetryReservation": if provider { Some("old") } else { None },
            }))
            .unwrap();
            if provider {
                saved.retry_cause = Some(Some(cause.clone()));
            }
            saved.bind_paid_admission("candidate", &state);
            assert!(!saved.has_controller_recovery_reservation());
            assert!(!saved.has_provider_retry_reservation());
            let pending = saved.pending_paid_admission.as_ref().unwrap();
            assert_eq!(pending.attempt, "candidate");
            assert!(!pending.capacity_refused);
            assert_eq!(pending.controller, !provider);
            assert_eq!(pending.provider_cause.is_some(), provider);
            saved.bind_paid_admission("candidate", &state);
            assert!(saved.pending_paid_admission.is_some());
            saved.bind_paid_admission("replacement", &state);
            assert!(
                saved.pending_paid_admission.is_none(),
                "an uncertain candidate cannot fund another attempt"
            );
        }
        let mut stale = RunCheckpoint::from_value(json!({
            "runnerId": "replacement", "providerRetryReservation": "old", "controllerRecoveries": 3,
        }))
        .unwrap();
        stale.retry_cause = Some(Some(cause));
        stale.bind_paid_admission("candidate", &state);
        assert!(stale.pending_paid_admission.is_none());
    }

    #[tokio::test]
    async fn decision_persists_the_pause_counter_that_extended_its_budget() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({ "id": "pause-decision", "taskId": "pause", "status": "running" }),
                    None,
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let checkpoint = Checkpoint::new(
            store.clone(),
            "pause-decision".into(),
            RunCheckpoint::fresh(Some(60_000)),
        );
        checkpoint.account_pause("attempt-a", 30_000);
        checkpoint.decide(|_, _| Ok(())).await.unwrap();
        let saved = RunCheckpoint::load(&store, "pause-decision")
            .await
            .unwrap()
            .unwrap();
        let observation = saved.administrative_pause_observation.as_ref().unwrap();
        assert_eq!(
            (observation.attempt.as_str(), observation.total_ms),
            ("attempt-a", 30_000)
        );
        let recovered = Checkpoint::new(store, "pause-decision".into(), saved);
        let deadline = recovered.deadline().unwrap();
        recovered.account_pause("attempt-a", 30_000);
        recovered.account_pause("attempt-a", 29_000);
        assert_eq!(recovered.deadline(), Some(deadline));
        recovered.account_pause("attempt-a", 32_000);
        assert_eq!(recovered.deadline(), Some(deadline + 2_000));
    }

    #[tokio::test]
    async fn no_op_run_transition_keeps_checkpoint_evidence_events_and_node_ownership() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.transaction(|db| {
            db.add_run_fixture(&json!({ "id": "ownership", "taskId": "task", "status": "queued", "recoveryPending": true, "nodeId": "node-a" }), None)?;
            Ok(())
        }).await.unwrap();
        let saved = RunCheckpoint::from_value(json!({
            "prepared": { "backend": "native", "isolated": true },
            "runnerId": "attempt-a", "nodeId": "node-a", "remainingMs": null,
        }))
        .unwrap();
        saved.store(&store, "ownership").await.unwrap();
        let checkpoint = Checkpoint::new(store.clone(), "ownership".into(), saved);
        let changes = store.subscribe();
        checkpoint
            .decide(|saved, db| {
                db.change_run(
                    "ownership",
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        recovery_pending: Some(true),
                        boundary: Some(json!({ "nodeId": "node-a" })),
                        ..Default::default()
                    },
                )?;
                assert!(saved.reserve_observation("attempt-a"));
                Ok(())
            })
            .await
            .unwrap();
        assert!(changes.has_changed().unwrap());
        let reserved = RunCheckpoint::load(&store, "ownership")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            reserved.controller_observation.as_ref().unwrap().requests,
            1
        );
        assert!(reserved.controller_observation.as_ref().unwrap().in_flight);
        checkpoint
            .decide(|saved, db| {
                db.change_run(
                    "ownership",
                    crate::runs::RunChanges {
                        status: Some(RunStatus::Queued),
                        ..Default::default()
                    },
                )?;
                saved.release_observation("attempt-a");
                saved.record_controller_result(
                    db,
                    "ownership",
                    "attempt-a",
                    Some(crate::runner::ControllerResult {
                        code: 70,
                        finalization_failed: true,
                        physical_finalized: false,
                    }),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        store
            .read(|db| {
                let saved = RunCheckpoint::load_in(db, "ownership")?.unwrap();
                assert_eq!(saved.terminal_code(), Some(70));
                assert_eq!(
                    saved.finalization_failure_attempt.as_deref(),
                    Some("attempt-a")
                );
                assert!(!saved.controller_observation.unwrap().in_flight);
                let ownership = db.checkpoint_node_ownership("ownership")?.unwrap();
                assert_eq!(
                    (ownership.node_id.as_deref(), ownership.runner_id.as_deref()),
                    (Some("node-a"), Some("attempt-a"))
                );
                let events = db.events_fixture("ownership", 0, 100)?;
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event["text"]
                            .as_str()
                            .is_some_and(|text| text.contains("filesystem could not be flushed")))
                        .count(),
                    1
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn launcher_publication_rolls_back_checkpoint_state_and_event_together() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.add_run_fixture(
                    &json!({ "id": "run", "taskId": "task", "status": "queued" }),
                    None,
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let mut saved = RunCheckpoint::fresh(None);
        saved.capacity_retry_at = Some(123);
        saved.store(&store, "run").await.unwrap();
        let checkpoint = Checkpoint::new(store.clone(), "run".into(), saved);
        let mut changes = store.subscribe();
        for reject in [true, false] {
            store.write(move |db| {
                db.0.execute_batch(if reject {
                    "CREATE TRIGGER reject_launch_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'event rejected'); END;"
                } else {
                    "DROP TRIGGER reject_launch_event;"
                })?;
                Ok(())
            }).await.unwrap();
            let result = checkpoint
                .update_and_publish(
                    |saved| saved.capacity_retry_at = None,
                    |db| {
                        db.change_run(
                            "run",
                            crate::runs::RunChanges {
                                status: Some(RunStatus::Running),
                                ..Default::default()
                            },
                        )?;
                        db.event(
                            "run",
                            &cairn_contracts::EventKind::Status,
                            "Preparing workspace",
                            Some(&json!({ "attempt": true })),
                        )
                    },
                )
                .await;
            assert_eq!(result.is_err(), reject);
            assert_eq!(changes.has_changed().unwrap(), !reject);
            let expected_retry = reject.then_some(123);
            assert_eq!(
                checkpoint.read(|saved| saved.capacity_retry_at).await,
                expected_retry
            );
            assert_eq!(
                RunCheckpoint::load(&store, "run")
                    .await
                    .unwrap()
                    .unwrap()
                    .capacity_retry_at,
                expected_retry
            );
            assert_eq!(
                store.run_state("run").await.unwrap().status,
                if reject {
                    RunStatus::Queued
                } else {
                    RunStatus::Running
                }
            );
            assert_eq!(
                store
                    .read(|db| db.event_page("run", 0, 100))
                    .await
                    .unwrap()
                    .len(),
                usize::from(!reject)
            );
            changes.borrow_and_update();
        }
    }

    #[test]
    fn administrative_pause_never_creates_a_deadline_for_an_unlimited_run() {
        let root = tempfile::tempdir().unwrap();
        let checkpoint = Checkpoint::new(
            Store::open(root.path()).unwrap(),
            "unlimited-fixture".into(),
            RunCheckpoint::fresh(None),
        );
        checkpoint.account_pause("attempt", 30_000);
        assert_eq!(checkpoint.deadline(), None);
        assert!(!checkpoint.expired());
    }
}
