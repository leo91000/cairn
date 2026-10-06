use crate::{
    config::now,
    error::Result,
    recovery::ProcessIdentity,
    run_status::RunStatus,
    store::{Db, Store},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
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
/// A mistyped field reads as absent, like the `Value` accessors it replaces.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCheckpoint {
    #[serde(default, with = "lenient", skip_serializing_if = "Option::is_none")]
    pub launched: Option<bool>,
    /// `null` means the run has no time limit; absent means its budget is spent.
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub remaining_ms: Nullable<i64>,
    #[serde(default, with = "lenient", skip_serializing_if = "Option::is_none")]
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
    #[serde(default, with = "lenient", skip_serializing_if = "Option::is_none")]
    pub completed: Option<bool>,
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub last_error: Nullable<String>,
    #[serde(default, with = "nullable", skip_serializing_if = "Option::is_none")]
    pub retry_cause: Nullable<crate::run_retry::Cause>,
    #[serde(default, with = "lenient", skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    #[serde(default, with = "lenient", skip_serializing_if = "Option::is_none")]
    pub controller_recoveries: Option<u64>,
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

    pub fn run_patch(&self) -> Result<Value> {
        let mut patch = serde_json::to_value(self)?;
        patch["accountWaitReason"] = Value::Null;
        patch["recoveryPending"] = false.into();
        if self.status == RunStatus::Cancelled {
            patch["retry"] = Value::Null;
        }
        Ok(patch)
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

    pub fn firecracker(&self) -> bool {
        self.prepared()
            .is_some_and(|p| p["backend"] == "firecracker")
    }

    pub fn runner_id(&self) -> Option<&str> {
        self.runner_id.as_ref()?.as_deref()
    }

    pub fn node_id(&self) -> Option<&str> {
        self.node_id.as_ref()?.as_deref()
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
    pub deadline: Option<i64>,
}

impl Checkpoint {
    pub fn new(store: Store, id: String, state: RunCheckpoint) -> Self {
        Self {
            deadline: state.deadline(),
            state: Mutex::new(state),
            store,
            id,
        }
    }

    pub fn expired(&self) -> bool {
        self.deadline.is_some_and(|deadline| now() >= deadline)
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
        state.remaining_ms = Some(self.deadline.map(|deadline| (deadline - now()).max(0)));
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

/// Serde adapter reading a mistyped value as absent.
mod lenient {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
    use serde_json::Value;

    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: DeserializeOwned>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        Ok(T::deserialize(Value::deserialize(deserializer)?).ok())
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
        Ok(T::deserialize(value).ok().map(Some))
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
            "remainingMs": null,
            "process": null,
            "settled": null,
            "prepared": { "isolated": true, "backend": "firecracker" },
            "runnerId": "runner",
            "movedFrom": "node-a",
        });
        let checkpoint = RunCheckpoint::from_value(stored.clone()).unwrap();
        assert!(checkpoint.launched());
        assert_eq!(checkpoint.remaining_ms, Some(None));
        assert_eq!(checkpoint.deadline(), None);
        assert!(checkpoint.isolated() && checkpoint.firecracker());
        assert_eq!(checkpoint.runner_id(), Some("runner"));
        assert_eq!(checkpoint.to_value().unwrap(), stored);

        let empty = RunCheckpoint::from_value(json!({})).unwrap();
        assert_eq!(empty.to_value().unwrap(), json!({}));
        assert!(empty.deadline().is_some());
    }

    #[test]
    fn mistyped_fields_read_as_absent() {
        let checkpoint =
            RunCheckpoint::from_value(json!({ "launched": "yes", "remainingMs": 1.5 })).unwrap();
        assert!(!checkpoint.launched());
        assert_eq!(checkpoint.remaining_ms, None);
    }
}
