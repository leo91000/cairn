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

/// Restart state persisted once in `run_checkpoints`, serialized from this type.
///
/// Unconverted chat/node boundaries still serialize these named fields. Nullable
/// assignments retain null versus absent; no untyped decision fields round-trip.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunCheckpoint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launched: Option<bool>,
    /// Preserved when an engine session is replaced on the same workspace disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_initialized: Option<bool>,
    /// Explicit controller rejection of this attempt; absent is not proof of rejection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_rejected: Option<bool>,
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
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub last_error: Nullable<String>,
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub retry_cause: Nullable<crate::run_retry::Cause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_recoveries: Option<u64>,
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
        encoded
            .map(|data| serde_json::from_str(&data).map_err(Into::into))
            .transpose()
    }

    pub async fn store(&self, store: &Store, run_id: &str) -> Result<()> {
        let (state, run_id) = (self.clone(), run_id.to_owned());
        store.write(move |db| state.store_in(db, &run_id)).await
    }

    pub fn store_in(&self, db: &Db<'_>, run_id: &str) -> Result<()> {
        db.0.prepare_cached(
            "INSERT INTO run_checkpoints(run_id,state) VALUES(?,?) ON CONFLICT(run_id) DO UPDATE SET state=excluded.state"
        )?.execute(params![run_id, serde_json::to_string(self)?])?;
        Ok(())
    }

    pub fn launched(&self) -> bool {
        self.launched == Some(true)
    }

    pub fn completed(&self) -> bool {
        self.completed == Some(true)
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

    /// Applies `change`, refreshes the remaining budget and persists the checkpoint.
    pub async fn update(&self, change: impl FnOnce(&mut RunCheckpoint)) -> Result<()> {
        let mut state = self.state.lock().await;
        change(&mut state);
        {
            let previous = self.pause_observation.lock().unwrap();
            state.administrative_pause_observation = Some(AdministrativePauseObservation {
                attempt: previous.0.clone(),
                total_ms: previous.1,
            });
            state.remaining_ms = Some(self.deadline().map(|deadline| (deadline - now()).max(0)));
        }
        state.store(&self.store, &self.id).await
    }

    /// Publishes engine state, its checkpoint and the event in one commit. A
    /// streamed text snapshot only changes memory; it does not rewrite the
    /// checkpoint per token. Memory advances only after successful publication.
    pub async fn record_event(
        &self,
        kind: cairn_contracts::EventKind,
        text: String,
        payload: Value,
        change: impl FnOnce(&mut RunCheckpoint) -> (Option<crate::runs::RunChanges>, bool),
    ) -> Result<()> {
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        let (changes, persist) = change(&mut next);
        if persist {
            let previous = self.pause_observation.lock().unwrap();
            next.administrative_pause_observation = Some(AdministrativePauseObservation {
                attempt: previous.0.clone(),
                total_ms: previous.1,
            });
            next.remaining_ms = Some(self.deadline().map(|deadline| (deadline - now()).max(0)));
        }
        let (id, saved) = (self.id.clone(), persist.then(|| next.clone()));
        self.store
            .transaction(move |db| {
                if let Some(changes) = changes {
                    db.change_run(&id, changes)?;
                }
                if let Some(saved) = saved {
                    saved.store_in(db, &id)?;
                }
                db.event(&id, &kind, &text, Some(&payload))
            })
            .await?;
        *state = next;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    async fn pause_credit_is_monotonic_per_attempt_and_survives_checkpoint_recovery() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        use crate::run_fixture::DbRunFixture;
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

// Temporary JSON adapters are owned by the unconverted node/chat domains. They
// cannot persist arbitrary checkpoint fields or create a second source of truth.
impl Db<'_> {
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
