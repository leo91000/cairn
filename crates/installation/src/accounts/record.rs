//! Private account state. Credentials remain in the encrypted vault.
use super::{PARALLEL_RUNS, lenient, usage::Usage};
use crate::{
    config::{id, now},
    error::Result,
    provider::Provider,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use cairn_contracts::AccountState;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub provider: Provider,
    pub name: String,
    pub enabled: bool,
    pub email: Option<String>,
    pub plan: Option<String>,
    /// A fingerprint of the signed-in identity, never shown to the user.
    pub identity: Option<String>,
    pub created_at: Option<i64>,
    pub checked_at: Option<i64>,
    pub state: AccountState,
    pub error: String,
    #[serde(deserialize_with = "lenient::usage")]
    pub usage: Option<Usage>,
    pub last_used_at: Option<i64>,
    pub exhausted: Option<Exhaustion>,
    pub max_concurrent_runs: Option<u64>,
    /// Why a Codex banked reset is pending or could not be redeemed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_error: Option<String>,
    pub priority: Option<u64>,
    pub model_provider: Option<String>,
    pub model_provider_name: Option<String>,
}

/// A run stopped on the account's usage limit for `model`, with the usage read at that time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Exhaustion {
    pub at: i64,
    pub model: String,
    #[serde(deserialize_with = "lenient::usage")]
    pub usage: Option<Usage>,
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
            priority: None,
            model_provider: None,
            model_provider_name: None,
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

    /// The user's position for this account, earlier first; unordered accounts follow by age.
    pub fn priority(&self) -> (u64, i64) {
        (
            self.priority.unwrap_or(u64::MAX),
            self.created_at.unwrap_or(0),
        )
    }

    /// Records a successful reading of the account's identity just now.
    pub fn mark_ready(&mut self) {
        self.state = AccountState::Ready;
        self.error = String::new();
        self.checked_at = Some(now());
    }
}

