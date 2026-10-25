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

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
            Self::Opencode => "OpenCode",
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
