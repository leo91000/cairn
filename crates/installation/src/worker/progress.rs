//! Writer-owned, bounded evidence of substantive work. Checkpoint persistence and
//! controller/lease observations never call `observe`.
use crate::{error::Result, recovery::ProcessIdentity, store::Db};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const MAX_ITEMS: usize = 128;

pub(crate) const SCHEMA: &str = "CREATE TABLE run_progress(run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE,state TEXT NOT NULL CHECK(json_valid(state) AND json_type(state)='object'));";

pub(crate) fn upgrade(connection: &mut rusqlite::Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 20 {
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "user_version", 21)?;
    } else if !(21..=crate::store::DATABASE_VERSION).contains(&version) {
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
    pub finalized_at: Option<i64>,
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
                self.process.as_ref().and_then(|p| i64::try_from(p.pid).ok()), self.process.as_ref().map(|p| &p.start), self.process.as_ref().map(|p| &p.boot)], |_| Ok(true))
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
            finalized_at: None,
            phase: Phase::Preparing,
            phase_at: at,
            complete: true,
            items: BTreeMap::new(),
            provider_started: false,
            persisted_at: at,
        }
    }

    pub(super) fn mark_incomplete(&mut self) -> bool {
        let changed = self.complete;
        self.complete = false;
        changed
    }

    pub fn confirm_finalized(&mut self, at: i64) {
        self.finalized_at.get_or_insert(at);
        if self.phase != Phase::Finalizing {
            self.phase = Phase::Finalizing;
            self.phase_at = at;
        }
    }

    /// Only canonical adapter frames from this worker's execution enter here.
    /// Unknown frames and replayed/unchanged snapshots cannot refresh the clock.
    pub fn observe(&mut self, event: &Value, at: i64) -> bool {
        let kind = event["type"].as_str().unwrap_or("");
        if kind == "runner.finalized" {
            if event["physicalFinalized"] == true
                && event["finalizationFailed"] != true
                && self
                    .runner_id
                    .as_deref()
                    .is_some_and(|id| event["attemptId"].as_str() == Some(id))
                && self.finalized_at.is_none()
            {
                self.confirm_finalized(at);
                return true;
            }
            return false;
        }
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
        if !self.complete {
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
        if kind == "turn.waiting" {
            // tasks[] names engine background tasks, not arbitrary open items.
            // Until every alias can be bound, an open tool cannot certify it.
            return self.mark_incomplete();
        }
        if !matches!(kind, "item.started" | "item.updated" | "item.completed") {
            return if observation_kind(kind) {
                false
            } else {
                self.mark_incomplete()
            };
        }
        let item = &event["item"];
        if item["details_truncated"] == true
            || item["replaces_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        {
            // Lossy previews can change when only counters cross the transport
            // limit. Identity folding also needs replay-safe alias ownership.
            // Neither may invent a new substantive clock or an eternal tool.
            return self.mark_incomplete();
        }
        let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) else {
            return self.mark_incomplete();
        };
        let item_type = item["type"].as_str().unwrap_or("");
        if !valid_item(item) {
            return self.mark_incomplete();
        }
        let detail = semantic_detail(item);
        // Event marker, elapsed time, retries and transport metadata are excluded.
        let fingerprint =
            digest(&json!({ "type": item_type, "status": item["status"], "detail": detail }));
        let key = digest(&json!([item["parent_id"], id]));
        if item_type == "todo_list"
            && item["items"].as_array().is_some_and(Vec::is_empty)
            && !self.items.contains_key(&key)
        {
            return false;
        }
        // Completing a spawn tool does not complete its still-running child.
        let completed = if item_type == "subagent" {
            matches!(
                item["status"].as_str(),
                Some("completed" | "failed" | "stopped")
            )
        } else {
            kind == "item.completed" || item["status"] == "completed" || item["status"] == "failed"
        };
        let tool = matches!(
            item_type,
            "command_execution" | "mcp_tool_call" | "web_search" | "subagent" | "collab_tool_call"
        );
        if tool
            && !completed
            && self
                .items
                .get(&key)
                .is_some_and(|previous| previous.completed)
        {
            // Reopening any completed tool needs a new execution identity. An
            // old in-progress replay cannot certify work or hide an open tool.
            return self.mark_incomplete();
        }
        if self.items.get(&key).is_some_and(|previous| {
            previous.fingerprint == fingerprint && (previous.completed || !completed)
        }) {
            return false;
        }
        if !self.items.contains_key(&key) && self.items.len() >= MAX_ITEMS {
            // Never evict then mistake a replay for a new item. Coverage remains
            // unknown until a fresh execution; memory and serialized state stay bounded.
            return self.mark_incomplete();
        }
        let completed = completed
            || self
                .items
                .get(&key)
                .is_some_and(|previous| previous.completed);
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

fn observation_kind(kind: &str) -> bool {
    matches!(
        kind,
        "diagnostic"
            | "output"
            | "status"
            | "thread.started"
            | "context.usage"
            | "engine.retry"
            | "engine.notice"
            | "provider.limit"
            | "error"
    )
}

fn known_kind(kind: &str) -> bool {
    observation_kind(kind)
        || matches!(
            kind,
            "turn.started"
                | "turn.completed"
                | "turn.failed"
                | "runner.failed"
                | "runner.finalized"
                | "turn.waiting"
                | "item.started"
                | "item.updated"
                | "item.completed"
        )
}

pub(super) fn known_frame(event: &Value) -> bool {
    let kind = event["type"].as_str().unwrap_or("");
    if !known_kind(kind) {
        return false;
    }
    if matches!(kind, "item.started" | "item.updated" | "item.completed") {
        let item = &event["item"];
        return valid_item(item);
    }
    true
}

// Validate every identity, discriminator and typed semantic field consumed by
// the fingerprint. Omitted/null tool-start outputs are valid; structured tool
// results are protocol-owned JSON and intentionally retain their full value.
fn valid_item(item: &Value) -> bool {
    let optional =
        |field: &str, check: fn(&Value) -> bool| item[field].is_null() || check(&item[field]);
    if !item.is_object()
        || !item["id"].as_str().is_some_and(|id| !id.is_empty())
        || !optional("parent_id", |v| v.as_str().is_some_and(|id| !id.is_empty()))
        || !optional("status", valid_status)
        || !optional("replaces_id", Value::is_string)
        || !optional("details_truncated", Value::is_boolean)
    {
        return false;
    }
    match item["type"].as_str().unwrap_or("") {
        "agent_message" | "reasoning" | "subagent_message" => item["text"].is_string(),
        "command_execution" => {
            optional("aggregated_output", Value::is_string)
                && optional("exit_code", |v| v.as_i64().is_some())
        }
        "mcp_tool_call" | "collab_tool_call" => true,
        "subagent" => {
            matches!(
                item["status"].as_str(),
                Some("in_progress" | "completed" | "failed" | "stopped")
            ) && optional("result", Value::is_string)
        }
        "todo_list" => item["items"].as_array().is_some_and(|items| {
            items.iter().all(|step| {
                step.is_object()
                    && step["text"].is_string()
                    && (step["status"].is_null() || valid_status(&step["status"]))
                    && (step["completed"].is_null() || step["completed"].is_boolean())
                    && (step["active_text"].is_null() || step["active_text"].is_string())
            })
        }),
        "file_change" => item["changes"].as_array().is_some_and(|changes| {
            changes.iter().all(|change| {
                change.is_object()
                    && change["path"].is_string()
                    && matches!(change["kind"].as_str(), Some("add" | "delete" | "update"))
                    && (change["diff"].is_null() || change["diff"].is_string())
            })
        }),
        "web_search" => {
            // Canonical adapters supply action; no result-element contract is
            // available. Do not guess semantic fields from arbitrary JSON.
            optional("action", valid_web_action) && item["results"].is_null()
        }
        _ => false,
    }
}

fn valid_status(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some("pending" | "in_progress" | "completed" | "failed" | "cancelled" | "stopped")
    )
}

fn web_action_fields(action: &Value) -> Option<&'static [&'static str]> {
    Some(match action["type"].as_str()? {
        "search" => &["type", "query", "queries"],
        "open_page" => &["type", "url"],
        "find_in_page" => &["type", "url", "pattern"],
        _ => return None,
    })
}

fn valid_web_action(action: &Value) -> bool {
    let Some(fields) = web_action_fields(action) else {
        return false;
    };
    if !action.is_object() {
        return false;
    }
    let search = action["type"] == "search";
    for field in fields {
        let value = &action[*field];
        if search && matches!(*field, "query" | "queries") && value.is_null() {
            continue;
        }
        let valid = if *field == "queries" {
            value
                .as_array()
                .is_some_and(|queries| queries.iter().all(Value::is_string))
        } else {
            value.is_string()
        };
        if !valid {
            return false;
        }
    }
    !search || !action["query"].is_null() || !action["queries"].is_null()
}

fn semantic_detail(item: &Value) -> Map<String, Value> {
    let project = |value: &Value, fields: &[&str]| -> Value {
        fields
            .iter()
            .map(|field| ((*field).to_owned(), value[*field].clone()))
            .collect::<Map<_, _>>()
            .into()
    };
    let mut detail = Map::new();
    for field in semantic_fields(item["type"].as_str().unwrap_or("")).unwrap_or_default() {
        let value = match *field {
            "items" => Value::Array(
                item["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|step| project(step, &["text", "status", "completed", "active_text"]))
                    .collect(),
            ),
            "changes" => Value::Array(
                item["changes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|change| project(change, &["path", "kind", "diff"]))
                    .collect(),
            ),
            "action" if !item["action"].is_null() => {
                project(&item["action"], web_action_fields(&item["action"]).unwrap())
            }
            _ => item[*field].clone(),
        };
        detail.insert((*field).to_owned(), value);
    }
    detail
}

fn semantic_fields(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "agent_message" | "reasoning" | "subagent_message" => &["text"],
        "command_execution" => &["aggregated_output", "exit_code"],
        "mcp_tool_call" | "collab_tool_call" => &["result", "error"],
        "todo_list" => &["items"],
        "subagent" => &["result"],
        "file_change" => &["changes"],
        "web_search" => &["action"],
        _ => return None,
    })
}

/// Redaction/preview changes are representation changes, never work evidence.
/// Compare borrowed fields before hashing; no unredacted data is retained.
pub(super) fn same_semantics(source: &Value, stored: &Value) -> bool {
    if source["type"] != stored["type"] {
        return false;
    }
    if !source["type"]
        .as_str()
        .is_some_and(|kind| kind.starts_with("item."))
    {
        return true;
    }
    let (source, stored) = (&source["item"], &stored["item"]);
    for field in [
        "id",
        "type",
        "status",
        "parent_id",
        "replaces_id",
        "details_truncated",
    ] {
        if source[field] != stored[field] {
            return false;
        }
    }
    let Some(fields) = semantic_fields(source["type"].as_str().unwrap_or("")) else {
        return true; // observe() explicitly marks unsupported types incomplete.
    };
    fields.iter().all(|field| source[*field] == stored[*field])
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
        for kind in ["diagnostic", "status", "engine.retry", "context.usage"] {
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
    fn delegated_work_uses_semantics_not_spawn_completion_or_heartbeat_counters() {
        let mut progress = progress();
        let mut child = json!({"type":"item.started","item":{"id":"child","type":"subagent","status":"in_progress","prompt":"private"}});
        assert!(progress.observe(&child, 2));
        assert_eq!(progress.phase, Phase::Tool);
        child["type"] = "item.completed".into();
        child["item"]["background"] = true.into();
        child["item"]["duration_ms"] = 1000.into();
        child["item"]["tokens"] = 123.into();
        child["item"]["tool_uses"] = 7.into();
        child["item"]["activity"] = "Generated activity notice".into();
        assert!(!progress.observe(&child, 3));
        assert_eq!(progress.phase, Phase::Tool);
        assert_eq!(progress.advanced_at, Some(2));
        child["type"] = "item.updated".into();
        child["item"]["status"] = "completed".into();
        child["item"]["result"] = "Actual report".into();
        assert!(progress.observe(&child, 5));
        assert_eq!(progress.phase, Phase::Provider);
        child["item"]["status"] = "in_progress".into();
        assert!(progress.observe(&child, 6));
        assert!(
            !progress.complete,
            "reused child IDs need a child-run identity"
        );
        assert_eq!(progress.advanced_at, Some(5));
    }

    #[test]
    fn unsupported_items_and_unbound_waits_make_coverage_unknown_without_new_progress() {
        for event in [
            json!({}),
            json!({"type":"future.provider_wait"}),
            json!({"type":"item.delta","item":{"id":"future","type":"agent_message","text":"unsupported kind"}}),
            json!({"type":"item.started","item":{"id":"future","type":"new_provider_item"}}),
            json!({"type":"item.updated","item":{"type":"agent_message","text":"missing ID"}}),
            json!({"type":"turn.waiting"}),
        ] {
            let mut progress = progress();
            progress.observe(&json!({"type":"turn.started"}), 2);
            assert!(progress.observe(&event, 3));
            assert!(!progress.complete);
            assert_eq!(progress.advanced_at, Some(2));
            assert!(!progress.observe(&event, 4));
            assert!(progress.observe(&json!({"type":"turn.completed"}), 5));
            assert_eq!(progress.phase, Phase::Finalizing);
        }
    }

    #[test]
    fn guest_and_manager_previews_cannot_turn_a_counter_change_into_progress() {
        let mut event = json!({"type":"item.updated","item":{"id":"child","type":"subagent","status":"in_progress","result":"r".repeat(5000),"prompt":"","tokens":9}});
        let padding = crate::run_output::ACTIVITY_DETAIL_LIMIT - event.to_string().len();
        event["item"]["prompt"] = "p".repeat(padding).into();
        assert_eq!(
            event.to_string().len(),
            crate::run_output::ACTIVITY_DETAIL_LIMIT
        );
        let initial: Value =
            serde_json::from_slice(&crate::run_output::event_line(&event)).unwrap();
        assert_ne!(initial["item"]["details_truncated"], true);
        event["item"]["tokens"] = 10.into();
        assert_eq!(
            event.to_string().len(),
            crate::run_output::ACTIVITY_DETAIL_LIMIT + 1
        );
        for compacted in [
            serde_json::from_slice::<Value>(&crate::run_output::event_line(&event)).unwrap(),
            crate::run_output::compact_activity(&event, &[]),
        ] {
            assert_eq!(compacted["item"]["details_truncated"], true);
            let mut progress = progress();
            assert!(progress.observe(&initial, 2));
            assert!(progress.observe(&compacted, 3));
            assert!(!progress.complete);
            assert_eq!(progress.advanced_at, Some(2));
        }
    }

    #[test]
    fn provisional_child_folding_becomes_unknown_without_replay_safe_alias_ownership() {
        let mut progress = progress();
        progress.observe(&json!({"type":"item.started","item":{"id":"provisional","type":"subagent","status":"in_progress"}}),2);
        assert!(progress.observe(&json!({"type":"item.completed","item":{"id":"spawn","replaces_id":"provisional","type":"subagent","status":"in_progress"}}),3));
        assert!(!progress.complete);
        assert_eq!(progress.advanced_at, Some(2));
        assert!(!progress.observe(&json!({"type":"item.updated","item":{"id":"spawn","type":"subagent","status":"completed","result":"report"}}),4));
    }

    #[test]
    fn a_background_wait_cannot_borrow_an_unrelated_open_tool() {
        let mut progress = progress();
        progress.observe(&json!({"type":"item.started","item":{"id":"child-a","type":"subagent","status":"in_progress"}}),2);
        assert!(progress.observe(
            &json!({"type":"turn.waiting","tasks":[{"id":"background-b"}]}),
            3
        ));
        assert!(!progress.complete);
        assert_eq!(progress.advanced_at, Some(2));
    }

    #[test]
    fn plan_snapshots_and_child_namespaces_are_semantic_and_bounded() {
        let mut progress = progress();
        let mut plan = json!({"type":"item.updated","item":{"id":"plan","type":"todo_list","items":[{"text":"private plan","status":"pending"}]}});
        assert!(progress.observe(&plan, 2));
        plan["item"]["duration_ms"] = 100.into();
        assert!(!progress.observe(&plan, 3));
        plan["item"]["items"][0]["status"] = "completed".into();
        assert!(progress.observe(&plan, 4));
        for parent in [Value::Null, Value::String("child".into())] {
            assert!(progress.observe(&json!({"type":"item.completed","item":{"id":"same-id","parent_id":parent,"type":"subagent_message","text":"Actual result"}}), 5));
        }
        assert_eq!(progress.items.len(), 3);
        assert!(
            !serde_json::to_string(&progress)
                .unwrap()
                .contains("private plan")
        );
    }

    #[test]
    fn malformed_consumed_fields_cannot_advance_or_rekey_progress() {
        let valid = json!({"id":"stable","type":"agent_message","text":"stable"});
        let mut cases = vec![
            json!({"id":"stable","type":"agent_message"}),
            json!({"id":"stable","type":"command_execution","aggregated_output":{ "duration_ms":2 }}),
            json!({"id":"stable","type":"command_execution","exit_code":"0"}),
            json!({"id":"stable","type":"todo_list","items":[{"text":{ "duration_ms":2 }}]}),
            json!({"id":"stable","type":"todo_list","items":[{"text":"step","completed":2}]}),
            json!({"id":"stable","type":"subagent","status":"in_progress","result":{ "duration_ms":2 }}),
            json!({"id":"stable","type":"file_change","changes":[{"path":3,"kind":"update"}]}),
            json!({"id":"stable","type":"web_search","action":{"type":"search","queries":[2]}}),
            json!({"id":"stable","type":"web_search","results":{ "duration_ms":2 }}),
        ];
        for field in [
            "text",
            "status",
            "parent_id",
            "replaces_id",
            "details_truncated",
        ] {
            for value in [Value::Null, json!({"duration_ms":2}), json!([]), json!(2)] {
                // Null is canonical absence for optional discriminators.
                if value.is_null() && field != "text" {
                    continue;
                }
                let mut item = valid.clone();
                item[field] = value;
                cases.push(item);
            }
        }
        for item in cases {
            let mut progress = progress();
            progress.observe(&json!({"type":"item.updated","item":valid}), 2);
            let event = json!({"type":"item.updated","item":item});
            assert!(!known_frame(&event), "{event}");
            assert!(progress.observe(&event, 3), "{event}");
            assert!(!progress.complete, "{event}");
            assert_eq!(progress.advanced_at, Some(2));
            assert_eq!(progress.items.len(), 1);
        }
    }

    #[test]
    fn canonical_tool_starts_structured_results_and_nested_metadata_are_supported() {
        for item in [
            json!({"id":"command","type":"command_execution","status":"in_progress"}),
            json!({"id":"command","type":"command_execution","aggregated_output":null,"exit_code":null}),
            json!({"id":"tool","type":"mcp_tool_call","result":{"content":[{"type":"text","text":"result"}],"structured":{"value":42}}}),
            json!({"id":"collab","type":"collab_tool_call","result":{"agent":{"status":"completed"}},"error":null}),
        ] {
            let mut progress = progress();
            let event = json!({"type":"item.started","item":item});
            assert!(known_frame(&event));
            assert!(progress.observe(&event, 2));
            assert!(progress.complete);
        }
        for item in [
            json!({"id":"plan","type":"todo_list","items":[{"text":"step","status":"pending"}]}),
            json!({"id":"files","type":"file_change","changes":[{"path":"a","kind":"update","diff":"content"}]}),
            json!({"id":"web","type":"web_search","action":{"type":"search","query":"work"}}),
        ] {
            let mut progress = progress();
            let mut event = json!({"type":"item.updated","item":item});
            assert!(progress.observe(&event, 2));
            match event["item"]["type"].as_str().unwrap() {
                "todo_list" => event["item"]["items"][0]["duration_ms"] = 3.into(),
                "file_change" => event["item"]["changes"][0]["duration_ms"] = 3.into(),
                _ => event["item"]["action"]["duration_ms"] = 3.into(),
            }
            assert!(!progress.observe(&event, 3));
            assert_eq!(progress.advanced_at, Some(2));
            assert!(progress.complete);
        }
    }

    #[test]
    fn web_variants_validate_exactly_the_fields_they_fingerprint() {
        for action in [
            json!({"type":"search","query":"unchanged","queries":["unchanged"]}),
            json!({"type":"open_page","url":"https://example.test"}),
            json!({"type":"find_in_page","url":"https://example.test","pattern":"unchanged"}),
        ] {
            let fields = web_action_fields(&action).unwrap();
            for field in ["query", "queries", "url", "pattern"] {
                for value in [json!({"duration_ms":3}), json!(3), json!(false), json!([3])] {
                    let mut progress = progress();
                    let mut event = json!({"type":"item.updated","item":{"id":"web","type":"web_search","action":action}});
                    assert!(progress.observe(&event, 2));
                    event["item"]["action"][field] = value;
                    if fields.contains(&field) {
                        assert!(!known_frame(&event), "{event}");
                        assert!(progress.observe(&event, 3));
                        assert!(!progress.complete);
                    } else {
                        // Values outside this variant are metadata, not evidence.
                        assert!(known_frame(&event), "{event}");
                        assert!(!progress.observe(&event, 3));
                        assert!(progress.complete);
                    }
                    assert_eq!(progress.advanced_at, Some(2));
                }
            }
        }
    }

    #[test]
    fn unsupported_web_results_and_every_reopened_tool_lose_coverage() {
        for results in [
            json!([{"duration_ms":1}]),
            json!([1]),
            json!([]),
            json!({"text":"unbound"}),
        ] {
            let mut progress = progress();
            let mut event = json!({"type":"item.updated","item":{"id":"web","type":"web_search","action":{"type":"search","query":"unchanged"}}});
            assert!(progress.observe(&event, 2));
            event["item"]["results"] = results;
            assert!(!known_frame(&event));
            assert!(progress.observe(&event, 3));
            assert!(!progress.complete);
            assert_eq!(progress.advanced_at, Some(2));
        }
        for kind in [
            "command_execution",
            "mcp_tool_call",
            "collab_tool_call",
            "web_search",
            "subagent",
        ] {
            let mut progress = progress();
            let mut event = json!({"type":"item.completed","item":{"id":"stable","type":kind,"status":"completed"}});
            assert!(progress.observe(&event, 2));
            event["type"] = "item.updated".into();
            event["item"]["status"] = "in_progress".into();
            assert!(known_frame(&event));
            assert!(progress.observe(&event, 3), "{event}");
            assert!(!progress.complete);
            assert_eq!(progress.advanced_at, Some(2));
            event["item"]["status"] = "completed".into();
            assert!(!progress.observe(&event, 4));
            assert!(!progress.complete);
            assert_eq!(progress.advanced_at, Some(2));
        }
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
        db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE runs(id TEXT PRIMARY KEY); INSERT INTO runs VALUES('retained'); PRAGMA user_version=20;").unwrap();
        upgrade(&mut db).unwrap();
        upgrade(&mut db).unwrap();
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            21
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
        db.execute_batch("CREATE TABLE run_progress(existing TEXT); PRAGMA user_version=20;")
            .unwrap();
        assert!(upgrade(&mut db).is_err());
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            20
        );
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[test]
    fn actual_full_schemas_seventeen_nineteen_twenty_and_twenty_one_use_the_safe_collector() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("manager.db");
        let mut connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(include_str!("../../tests/fixtures/personal-state-v17.sql"))
            .unwrap();
        connection.execute("INSERT INTO runs(id,task_id,status,created_at,recovery_pending,resume_available,resume_count,boundary) VALUES('progress-compat','progress-task','running',1,0,1,0,?)", [json!({"missionTurnId":"turn"}).to_string()]).unwrap();
        connection
            .execute(
                "INSERT INTO run_checkpoints(run_id,state) VALUES('progress-compat',?)",
                [json!({"runnerId":"attempt","completed":false}).to_string()],
            )
            .unwrap();
        let helper = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/run-progress.mjs")
            .canonicalize()
            .unwrap();
        let script = format!(
            "import {{DatabaseSync}} from 'node:sqlite';const m=await import({});const db=new DatabaseSync(process.argv[1],{{readOnly:true}});const row=m.decodeRunProgress(m.prepareRunProgress(db).get('progress-compat'));if(m.observeRunProgress(row,Number(process.argv[3])).status!==process.argv[2])process.exit(1);db.close();",
            serde_json::to_string(&helper.to_string_lossy()).unwrap()
        );
        let check_at = |expected: &str, at: i64| {
            let result = std::process::Command::new("node")
                .args(["--input-type=module", "-e", &script])
                .arg(&database)
                .arg(expected)
                .arg(at.to_string())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        };
        let check = |expected: &str| check_at(expected, 3);
        check("unknown");
        crate::reads::migration::upgrade(&mut connection).unwrap();
        crate::runs::projections::upgrade(&mut connection).unwrap();
        assert_eq!(
            connection
                .pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            19
        );
        check("unknown");
        // The released version-20 contract adds hooks, before progress exists.
        connection
            .execute_batch(crate::plugins::hooks::SCHEMA)
            .unwrap();
        connection.pragma_update(None, "user_version", 20).unwrap();
        check("unknown");
        upgrade(&mut connection).unwrap();
        check("unknown");
        let mut progress = Progress::launched(
            "execution".into(),
            Some("attempt".into()),
            None,
            Some("turn".into()),
            1,
        );
        progress.observe(&json!({"type":"turn.started"}), 2);
        progress
            .store_in(&Db(&connection), "progress-compat")
            .unwrap();
        check("good");
        progress.observe(&json!({"type":"item.completed","item":{"id":"child","type":"subagent","status":"in_progress"}}),2);
        progress
            .store_in(&Db(&connection), "progress-compat")
            .unwrap();
        check_at("expected", 2_000_003);
        progress.observe(
            &json!({"type":"item.updated","item":{"id":"unknown","type":"future_activity"}}),
            3,
        );
        progress
            .store_in(&Db(&connection), "progress-compat")
            .unwrap();
        check_at("unknown", 2_000_003);
    }
}
