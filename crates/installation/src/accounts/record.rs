//! The persisted account record, as stored under [`super::KIND`].
use super::{PARALLEL_RUNS, lenient, usage::Usage};
use crate::{
    config::{id, now},
    error::Result,
    provider::Provider,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountState {
    /// Added, but never signed in.
    Pending,
    Ready,
    /// Needs to be reconnected.
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    #[serde(default = "codex", deserialize_with = "lenient::provider")]
    pub provider: Provider,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    pub email: Option<String>,
    pub plan: Option<String>,
    /// A fingerprint of the signed-in identity, never shown to the user.
    pub identity: Option<String>,
    pub created_at: Option<i64>,
    pub checked_at: Option<i64>,
    pub state: AccountState,
    #[serde(default)]
    pub error: String,
    #[serde(default, deserialize_with = "lenient::usage")]
    pub usage: Option<Usage>,
    pub last_used_at: Option<i64>,
    pub exhausted: Option<Exhaustion>,
    pub max_concurrent_runs: Option<u64>,
    /// Why a Codex banked reset is pending or could not be redeemed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_error: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A run stopped on the account's usage limit for `model`, with the usage read at that time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Exhaustion {
    #[serde(default)]
    pub at: i64,
    #[serde(default)]
    pub model: String,
    #[serde(default, deserialize_with = "lenient::usage")]
    pub usage: Option<Usage>,
}

/// Records saved before Claude Code support have no provider and belong to Codex.
fn codex() -> Provider {
    Provider::Codex
}

impl Account {
    /// A new account, before its first sign-in or import.
    pub fn new(provider: Provider, name: &str, state: AccountState) -> Self {
        Self {
            id: id(),
            provider,
            name: name.into(),
            enabled: true,
            email: None,
            plan: None,
            identity: None,
            created_at: Some(now()),
            checked_at: None,
            state,
            error: String::new(),
            usage: None,
            last_used_at: None,
            exhausted: None,
            max_concurrent_runs: Some(PARALLEL_RUNS),
            reset_error: None,
            extra: Map::new(),
        }
    }

    pub fn from_value(value: Value) -> Result<Self> {
        Ok(serde_json::from_value(value)?)
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn is_ready(&self) -> bool {
        self.state == AccountState::Ready
    }

    /// Signed in and not paused.
    pub fn is_active(&self) -> bool {
        self.is_ready() && self.enabled
    }

    pub fn usage(&self) -> Usage {
        self.usage.clone().unwrap_or_default()
    }

    pub fn parallel_runs(&self) -> usize {
        self.max_concurrent_runs.unwrap_or(PARALLEL_RUNS) as usize
    }

    /// The model a run was exhausted on, or `fallback` when the account is not exhausted.
    pub fn exhausted_model<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.exhausted
            .as_ref()
            .map_or(fallback, |e| e.model.as_str())
    }

    /// Records a successful reading of the account's identity just now.
    pub fn mark_ready(&mut self) {
        self.state = AccountState::Ready;
        self.error = String::new();
        self.checked_at = Some(now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_accounts_keep_their_stored_shape() {
        let account = Account::new(Provider::Claude, "Work", AccountState::Pending).to_value();
        let mut keys = account
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            [
                "checkedAt",
                "createdAt",
                "email",
                "enabled",
                "error",
                "exhausted",
                "id",
                "identity",
                "lastUsedAt",
                "maxConcurrentRuns",
                "name",
                "plan",
                "provider",
                "state",
                "usage",
            ]
        );
        assert_eq!(account["provider"], "claude");
        assert_eq!(account["state"], "pending");
        assert_eq!(account["maxConcurrentRuns"], 4);
    }

    #[test]
    fn unknown_fields_round_trip() {
        let value = json!({ "id": "a", "state": "ready", "future": { "x": 1 } });
        let account = Account::from_value(value).unwrap();
        assert_eq!(account.provider, Provider::Codex);
        assert_eq!(account.to_value()["future"], json!({ "x": 1 }));
    }
}
