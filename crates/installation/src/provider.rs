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
    Antigravity,
}

impl Provider {
    pub const ALL: [Provider; 4] = [
        Provider::Codex,
        Provider::Claude,
        Provider::Opencode,
        Provider::Antigravity,
    ];

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
            "antigravity" => Ok(Self::Antigravity),
            _ => Err(Error::bad("Choose a supported coding agent.")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Opencode => "opencode",
            Self::Antigravity => "antigravity",
        }
    }

    /// Whether the coding agent can compact its native context on request. OpenCode runs
    /// with automatic compaction disabled; neither it nor Antigravity exposes a
    /// compaction control to Cairn.
    pub fn compacts_context(self) -> bool {
        matches!(self, Self::Codex | Self::Claude)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
            Self::Opencode => "OpenCode",
            Self::Antigravity => "Antigravity",
        }
    }
}

/// Tables whose CHECK constraints list every coding agent.
const PROVIDER_TABLES: &[&str] = &[
    "agents",
    "coding_agent_accounts",
    "account_leases",
    "coding_agent_configurations",
    "runs",
    "chat_messages",
    "conversation_engine_switches",
];

/// Version 36 to 37: Antigravity joins the coding agents allowed by stored CHECK
/// constraints. SQLite cannot alter a CHECK in place, and rebuilding these tables
/// (runs among them) would rewrite every row. A CHECK does not change the on-disk
/// format, so SQLite documents editing its schema text with `writable_schema` as safe;
/// the schema version change makes every connection reload it.
pub(crate) fn upgrade_storage(db: &mut rusqlite::Connection) -> Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 36 {
        // SQLite refuses to prepare a schema update before this pragma is set.
        tx.pragma_update(None, "writable_schema", true)?;
        let mut update = tx.prepare(
            "UPDATE sqlite_schema SET sql=replace(replace(sql,?1,?2),?3,?4) WHERE type='table' AND name=?5",
        )?;
        for table in PROVIDER_TABLES {
            update.execute(rusqlite::params![
                "IN ('codex','claude','opencode')",
                "IN ('codex','claude','opencode','antigravity')",
                "IN ('','codex','claude','opencode')",
                "IN ('','codex','claude','opencode','antigravity')",
                table,
            ])?;
        }
        drop(update);
        let schema: i64 = tx.pragma_query_value(None, "schema_version", |row| row.get(0))?;
        tx.pragma_update(None, "schema_version", schema + 1)?;
        tx.pragma_update(None, "writable_schema", false)?;
        for table in PROVIDER_TABLES {
            let sql: String = tx.query_row(
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?",
                [table],
                |row| row.get(0),
            )?;
            if !sql.contains("'antigravity'") {
                return Err(Error::bad(
                    "Unexpected coding agent constraints during the Antigravity storage upgrade.",
                ));
            }
        }
        tx.pragma_update(None, "user_version", 37)?;
    } else if !(37..=crate::store::DATABASE_VERSION).contains(&version) {
        return Err(Error::conflict(
            "Unexpected version during the Antigravity storage upgrade.",
        ));
    }
    tx.commit()?;
    let check: String = db.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(Error::bad(
            "The database failed its integrity check after the Antigravity storage upgrade.",
        ));
    }
    Ok(())
}

impl From<Provider> for cairn_contracts::AgentProvider {
    fn from(value: Provider) -> Self {
        match value {
            Provider::Codex => Self::Codex,
            Provider::Claude => Self::Claude,
            Provider::Opencode => Self::Opencode,
            Provider::Antigravity => Self::Antigravity,
        }
    }
}

impl From<cairn_contracts::AgentProvider> for Provider {
    fn from(value: cairn_contracts::AgentProvider) -> Self {
        match value {
            cairn_contracts::AgentProvider::Codex => Self::Codex,
            cairn_contracts::AgentProvider::Claude => Self::Claude,
            cairn_contracts::AgentProvider::Opencode => Self::Opencode,
            cairn_contracts::AgentProvider::Antigravity => Self::Antigravity,
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
        assert_eq!(
            Provider::parse("antigravity").unwrap(),
            Provider::Antigravity
        );
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
