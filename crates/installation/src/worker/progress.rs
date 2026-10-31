//! Writer-owned, bounded evidence of substantive work. Checkpoint persistence and
//! controller/lease observations never call `observe`.
use crate::{error::Result, recovery::ProcessIdentity, store::Db};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const MAX_ITEMS: usize = 128;

pub(crate) const SCHEMA: &str = "CREATE TABLE run_progress(run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE,state TEXT NOT NULL CHECK(json_valid(state) AND json_type(state)='object'));";

pub(crate) fn upgrade(connection: &mut rusqlite::Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 19 {
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "user_version", 20)?;
    } else if version != 20 {
        return Err(crate::error::Error::bad(
            "Unexpected installation database version during progress upgrade.",
        ));
    }
    tx.commit()?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Progress {
    pub version: u8,
    pub execution_id: String,
    pub runner_id: Option<String>,
    pub process: Option<ProcessIdentity>,
    pub turn_id: Option<String>,
    pub started_at: i64,
    pub advanced_at: Option<i64>,
    pub phase: Phase,
    pub phase_at: i64,
    pub complete: bool,
    // Digests stay installation-private; monitoring projects only IDs/times/phase.
    items: BTreeMap<String, ItemProgress>,
    provider_started: bool,
    pub persisted_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ItemProgress {
    fingerprint: String,
    completed: bool,
    tool: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Provider,
    Tool,
    Finalizing,
}

impl Progress {
    pub fn bound_in(&self, db: &Db<'_>, run_id: &str) -> Result<bool> {
        Ok(db.0.prepare_cached("SELECT 1 FROM runs r JOIN run_checkpoints k ON k.run_id=r.id
            WHERE r.id=? AND coalesce(json_extract(k.state,'$.turnPending'),0)!=1
            AND coalesce(json_extract(r.boundary,'$.chatExecution.messageId'),json_extract(r.boundary,'$.missionTurnId')) IS ?
            AND ((? IS NOT NULL AND json_extract(k.state,'$.runnerId') IS ?)
              OR (? IS NULL AND json_extract(k.state,'$.runnerId') IS NULL
                AND json_extract(k.state,'$.process.pid') IS ?
                AND json_extract(k.state,'$.process.start') IS ?
                AND json_extract(k.state,'$.process.boot') IS ?))")?
            .query_row(params![run_id, self.turn_id, self.runner_id, self.runner_id, self.runner_id,
                self.process.as_ref().map(|p| p.pid), self.process.as_ref().map(|p| &p.start), self.process.as_ref().map(|p| &p.boot)], |_| Ok(true))
            .optional()?.unwrap_or(false))
    }

    pub fn store_in(&self, db: &Db<'_>, run_id: &str) -> Result<()> {
        db.0.prepare_cached("INSERT INTO run_progress(run_id,state) VALUES(?,?) ON CONFLICT(run_id) DO UPDATE SET state=excluded.state")?
            .execute(params![run_id, serde_json::to_string(self)?])?;
        Ok(())
    }

    pub fn launched(
        execution_id: String,
        runner_id: Option<String>,
        process: Option<ProcessIdentity>,
        turn_id: Option<String>,
        at: i64,
    ) -> Self {
        Self {
            version: 1,
            execution_id,
            runner_id,
            process,
            turn_id,
            started_at: at,
            advanced_at: None,
            phase: Phase::Preparing,
            phase_at: at,
            complete: true,
            items: BTreeMap::new(),
            provider_started: false,
            persisted_at: at,
        }
    }

    /// Only canonical adapter frames from this worker's execution enter here.
    /// Unknown frames and replayed/unchanged snapshots cannot refresh the clock.
    pub fn observe(&mut self, event: &Value, at: i64) -> bool {
        let kind = event["type"].as_str().unwrap_or("");
        if matches!(kind, "turn.completed" | "turn.failed" | "runner.failed") {
            if self.phase == Phase::Finalizing {
                return false;
            }
            self.phase = Phase::Finalizing;
            self.phase_at = at;
            return true;
        }
        // Provider ending is sticky for this execution. A late item/turn marker
        // cannot make controller/storage finalization appear healthy again.
        if self.phase == Phase::Finalizing {
            return false;
        }
        if kind == "turn.started" {
            if self.provider_started {
                return false;
            }
            self.provider_started = true;
            self.phase = Phase::Provider;
            self.phase_at = at;
            self.advanced_at = Some(at);
            return true;
        }
        if !matches!(kind, "item.started" | "item.updated" | "item.completed") {
            return false;
        }
        let item = &event["item"];
        let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) else {
            return false;
        };
        let item_type = item["type"].as_str().unwrap_or("");
        let detail = match item_type {
            "agent_message" | "reasoning" => json!({ "text": item["text"] }),
            "command_execution" => json!({
                "output": item["aggregated_output"],
                "exitCode": item["exit_code"],
            }),
            "mcp_tool_call" => json!({ "result": item["result"], "error": item["error"] }),
            "file_change" => json!({ "changes": item["changes"] }),
            "web_search" => json!({ "action": item["action"], "results": item["results"] }),
            _ => return false,
        };
        // Event marker, elapsed time, retries and transport metadata are excluded.
        let fingerprint =
            digest(&json!({ "type": item_type, "status": item["status"], "detail": detail }));
        let key = digest(&Value::String(id.to_owned()));
        let completed =
            kind == "item.completed" || item["status"] == "completed" || item["status"] == "failed";
        if self.items.get(&key).is_some_and(|previous| {
            previous.fingerprint == fingerprint && (previous.completed || !completed)
        }) {
            return false;
        }
        if !self.items.contains_key(&key) && self.items.len() >= MAX_ITEMS {
            // Never evict then mistake a replay for a new item. Coverage remains
            // unknown until a fresh execution; memory and serialized state stay bounded.
            let changed = self.complete;
            self.complete = false;
            return changed;
        }
        let completed = completed
            || self
                .items
                .get(&key)
                .is_some_and(|previous| previous.completed);
        let tool = matches!(
            item_type,
            "command_execution" | "mcp_tool_call" | "web_search"
        );
        self.items.insert(
            key,
            ItemProgress {
                fingerprint,
                completed,
                tool,
            },
        );
        self.advanced_at = Some(at);
        let phase = if self.items.values().any(|item| item.tool && !item.completed) {
            Phase::Tool
        } else {
            Phase::Provider
        };
        if self.phase != phase {
            self.phase = phase;
            self.phase_at = at;
        }
        true
    }
}

fn digest(value: &Value) -> String {
    hex::encode(Sha256::digest(value.to_string().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress() -> Progress {
        Progress::launched(
            "execution".into(),
            Some("runner".into()),
            None,
            Some("turn".into()),
            1,
        )
    }

    #[test]
    fn diagnostic_heartbeat_and_unchanged_snapshots_do_not_certify_work() {
        let mut progress = progress();
        for kind in [
            "diagnostic",
            "status",
            "engine.retry",
            "turn.waiting",
            "context.usage",
        ] {
            assert!(!progress.observe(&json!({ "type": kind }), 10));
        }
        assert_eq!(progress.advanced_at, None);
        let mut event = json!({ "type": "item.started", "item": { "id": "tool", "type": "command_execution", "status": "in_progress", "aggregated_output": "" } });
        assert!(progress.observe(&event, 20));
        event["type"] = "item.updated".into();
        event["item"]["duration_ms"] = 5000.into();
        assert!(!progress.observe(&event, 30));
        event["item"]["aggregated_output"] = "substantive".into();
        assert!(progress.observe(&event, 40));
        assert_eq!(progress.advanced_at, Some(40));
        assert!(progress.observe(&json!({ "type": "turn.completed" }), 50));
        assert!(!progress.observe(&event, 60));
        assert_eq!(progress.phase, Phase::Finalizing);
        assert_eq!(progress.phase_at, 50);
        assert_eq!(progress.advanced_at, Some(40));
    }

    #[test]
    fn completing_one_parallel_tool_does_not_hide_the_other_quiet_tool() {
        let mut progress = progress();
        for id in ["one", "two"] {
            progress.observe(
                &json!({"type":"item.started","item":{"id":id,"type":"command_execution"}}),
                2,
            );
        }
        progress.observe(
            &json!({"type":"item.completed","item":{"id":"one","type":"command_execution"}}),
            3,
        );
        assert_eq!(progress.phase, Phase::Tool);
        progress.observe(
            &json!({"type":"item.completed","item":{"id":"two","type":"command_execution"}}),
            4,
        );
        assert_eq!(progress.phase, Phase::Provider);
        assert!(!progress.observe(
            &json!({"type":"item.completed","item":{"id":"two","type":"command_execution"}}),
            5
        ));
    }

    #[test]
    fn bounded_item_budget_becomes_unknown_instead_of_forgetting_replays() {
        let mut progress = progress();
        for n in 0..10000 {
            progress.observe(&json!({ "type": "item.started", "item": { "id": format!("item-{n}"), "type": "agent_message", "text": "private" } }), n + 2);
        }
        assert!(!progress.complete);
        assert_eq!(progress.items.len(), MAX_ITEMS);
        assert!(serde_json::to_string(&progress).unwrap().len() < 24576);
        assert!(
            !serde_json::to_string(&progress)
                .unwrap()
                .contains("private")
        );
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn upgrade_is_atomic_idempotent_preserves_rows_and_cascades() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE runs(id TEXT PRIMARY KEY); INSERT INTO runs VALUES('retained'); PRAGMA user_version=19;").unwrap();
        upgrade(&mut db).unwrap();
        upgrade(&mut db).unwrap();
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            20
        );
        db.execute("INSERT INTO run_progress VALUES('retained','{}')", [])
            .unwrap();
        db.execute("DELETE FROM runs WHERE id='retained'", [])
            .unwrap();
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT count(*) FROM run_progress", [], |row| row.get(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn failed_upgrade_does_not_advance_version() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE run_progress(existing TEXT); PRAGMA user_version=19;")
            .unwrap();
        assert!(upgrade(&mut db).is_err());
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            19
        );
    }
}
