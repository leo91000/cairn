//! The coding agent that executes an agent's work.
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Codex,
    Claude,
    Opencode,
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::Codex, Provider::Claude, Provider::Opencode];

    /// Current agent and run records explicitly select their coding engine.
    pub fn of_agent(agent: &Value) -> Result<Self> {
        match agent.get("provider") {
            None => Err(Error::bad("Missing coding agent in saved state.")),
            Some(Value::String(value)) => Self::parse(value),
            Some(_) => Err(Error::bad("Invalid coding agent.")),
        }
    }

    pub fn of_run(run: &Value) -> Result<Self> {
        Self::of_agent(&run["snapshot"]["agent"])
    }

    pub fn of_account(account: &Value) -> Result<Self> {
        Self::of_agent(account)
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "opencode" => Ok(Self::Opencode),
            _ => Err(Error::bad("Choose a supported coding agent.")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Opencode => "opencode",
        }
    }

    /// Whether the coding agent can compact its native context on request. OpenCode runs
    /// with automatic compaction disabled and exposes no compaction control to Cairn.
    pub fn compacts_context(self) -> bool {
        matches!(self, Self::Codex | Self::Claude)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
            Self::Opencode => "OpenCode",
        }
    }
}

impl From<Provider> for cairn_contracts::AgentProvider {
    fn from(value: Provider) -> Self {
        match value {
            Provider::Codex => Self::Codex,
            Provider::Claude => Self::Claude,
            Provider::Opencode => Self::Opencode,
        }
    }
}

impl From<cairn_contracts::AgentProvider> for Provider {
    fn from(value: cairn_contracts::AgentProvider) -> Self {
        match value {
            cairn_contracts::AgentProvider::Codex => Self::Codex,
            cairn_contracts::AgentProvider::Claude => Self::Claude,
            cairn_contracts::AgentProvider::Opencode => Self::Opencode,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_require_an_explicit_provider() {
        assert!(Provider::of_agent(&json!({})).is_err());
        assert_eq!(
            Provider::of_agent(&json!({ "provider": "claude" })).unwrap(),
            Provider::Claude
        );
        assert_eq!(
            Provider::of_run(&json!({ "snapshot": { "agent": { "provider": "codex" } } })).unwrap(),
            Provider::Codex
        );
        assert!(Provider::parse("gemini").is_err());
        assert_eq!(serde_json::to_value(Provider::Claude).unwrap(), "claude");
    }

    #[test]
    fn explicit_unknown_or_malformed_providers_never_fall_back_to_codex() {
        for value in [json!("unsupported"), json!(""), json!(null), json!(42)] {
            assert!(Provider::of_agent(&json!({ "provider": value })).is_err());
            assert!(Provider::of_account(&json!({ "provider": value })).is_err());
            assert!(
                Provider::of_run(&json!({
                    "snapshot": { "agent": { "provider": value } }
                }))
                .is_err()
            );
        }
    }
}
