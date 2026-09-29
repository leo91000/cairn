//! The coding agent (Codex or Claude Code) that executes an agent's work.
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Codex,
    Claude,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Codex, Provider::Claude];

    /// Agents saved before Claude Code support have no provider and run on Codex.
    pub fn of_agent(agent: &Value) -> Self {
        if agent["provider"] == "claude" {
            Self::Claude
        } else {
            Self::Codex
        }
    }

    pub fn of_run(run: &Value) -> Self {
        Self::of_agent(&run["snapshot"]["agent"])
    }

    pub fn of_account(account: &Value) -> Self {
        Self::of_agent(account)
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            _ => Err(Error::bad("Choose Codex or Claude Code.")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agents_without_a_provider_run_on_codex() {
        assert_eq!(Provider::of_agent(&json!({})), Provider::Codex);
        assert_eq!(
            Provider::of_agent(&json!({"provider":"claude"})),
            Provider::Claude
        );
        assert_eq!(
            Provider::of_run(&json!({"snapshot":{"agent":{"provider":"codex"}}})),
            Provider::Codex
        );
        assert!(Provider::parse("gemini").is_err());
        assert_eq!(serde_json::to_value(Provider::Claude).unwrap(), "claude");
    }
}
