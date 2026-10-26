use crate::{
    config::now,
    error::Result,
    recovery::ProcessIdentity,
    run_status::RunStatus,
    store::{Db, Store},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::{
    Mutex as SyncMutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::Mutex;

/// Absent (`None`), explicitly `null` (`Some(None)`) or set (`Some(Some(_))`).
type Nullable<T> = Option<Option<T>>;

pub fn key(run_id: &str) -> String {
    format!("run-checkpoint:{run_id}")
}

/// Restart state persisted under `run-checkpoint:{id}`.
///
/// Other modules read and patch the same document as JSON, so unknown fields
/// round-trip through `extra` and nullable fields keep `null` distinct from absent.
/// Present fields must have their current type.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    #[serde(flatten)]
    pub extra: Map<String, Value>,
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

    /// Captures the outcome already recorded on the run document.
    pub fn from_run(run: &Value) -> Option<Self> {
        Some(Self {
            status: RunStatus::of(run)?,
            summary: run["summary"].as_str().map(str::to_owned),
            error: None,
            finished_at: run["finishedAt"].as_i64(),
        })
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
        store
            .kv(&key(run_id))
            .await?
            .map(Self::from_value)
            .transpose()
    }

    pub fn load_in(db: &Db<'_>, run_id: &str) -> Result<Option<Self>> {
        db.kv(&key(run_id))?.map(Self::from_value).transpose()
    }

    pub async fn store(&self, store: &Store, run_id: &str) -> Result<()> {
        store.set(&key(run_id), self.to_value()?, None).await
    }

    pub fn store_in(&self, db: &Db<'_>, run_id: &str) -> Result<()> {
        db.set(&key(run_id), &self.to_value()?, None)
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
            .extra
            .get("administrativePauseObservation")
            .cloned()
            .unwrap_or_default();
        Self {
            deadline_base: state.deadline(),
            administrative_pause_ms: AtomicU64::new(0),
            pause_observation: SyncMutex::new((
                previous["attempt"].as_str().unwrap_or("").into(),
                previous["totalMs"].as_u64().unwrap_or(0),
            )),
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
            state.extra.insert(
                "administrativePauseObservation".into(),
                serde_json::json!({ "attempt": previous.0, "totalMs": previous.1 }),
            );
            state.remaining_ms = Some(self.deadline().map(|deadline| (deadline - now()).max(0)));
        }
        state.store(&self.store, &self.id).await
    }

    pub async fn persist(&self) -> Result<()> {
        self.update(|_| {}).await
    }

    /// Changes the in-memory state only; the next update persists it.
    pub async fn remember(&self, change: impl FnOnce(&mut RunCheckpoint)) {
        change(&mut *self.state.lock().await);
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
    fn round_trips_null_absent_and_unknown_fields() {
        let stored = json!({
            "launched": true,
            "workspaceInitialized": true,
            "remainingMs": null,
            "process": null,
            "settled": null,
            "prepared": { "isolated": true, "backend": "firecracker" },
            "runnerId": "runner",
            "movedFrom": "node-a",
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
        assert!(RunCheckpoint::from_value(json!({ "launched": "yes" })).is_err());
        assert!(RunCheckpoint::from_value(json!({ "remainingMs": 1.5 })).is_err());
    }

    #[tokio::test]
    async fn pause_credit_is_monotonic_per_attempt_and_survives_checkpoint_recovery() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
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
