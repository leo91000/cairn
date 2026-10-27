//! Commands on an existing run are distinct from admitting a new mission run.
use crate::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunInput {
    pub run_id: RunId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunMissionInput {
    pub task_id: MissionId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelRunOutput {
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CleanupRunOutput {
    pub cleaned: bool,
}

/// Lifecycle fields have one SQL authority. Engine/node/chat payloads are adapted
/// separately at their domain boundaries, never merged into this state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunState {
    pub id: String,
    pub task_id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[schemars(
        with = "String",
        pattern("^(queued|running|waiting|succeeded|failed|cancelled|interrupted)$")
    )]
    pub status: RunStatus,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    #[ts(type = "number")]
    pub created_at: i64,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub error: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    #[ts(type = "number | null")]
    #[ts(optional = nullable)]
    pub workspace_cleaned_at: Option<i64>,
    #[serde(default)]
    pub recovery_pending: bool,
    #[serde(default)]
    pub resume_available: bool,
    #[serde(default)]
    pub resume_count: u32,
    #[serde(default)]
    #[ts(type = "number | null")]
    #[ts(optional = nullable)]
    pub cancel_requested_at: Option<i64>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub account_id: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub account_name: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub account_wait_reason: Option<String>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern("^(codex|claude|opencode)$"))]
    #[ts(optional = nullable)]
    pub account_required: Option<AgentProvider>,
    #[serde(default)]
    #[ts(type = "number | null")]
    #[ts(optional = nullable)]
    pub capacity_wait_until: Option<i64>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub retry: Option<Retry>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub requested_by: Option<RunRequester>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunRequester {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunSnapshot {
    pub task: RunTaskSnapshot,
    pub agent: Agent,
    pub project: Option<Project>,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub available_projects: Vec<Project>,
    #[serde(default)]
    pub skills: Vec<RunSkill>,
    #[serde(default)]
    pub delegation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunSkill {
    pub name: String,
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub bundle: Option<String>,
}

/// The full wire view shares its lifecycle fields with SQL. Payloads owned by
/// unconverted domains use explicit client type references at this boundary;
/// they do not define or decide run admission/transitions (A6-A13/A18).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    #[serde(flatten)]
    #[ts(flatten)]
    pub state: RunState,
    #[serde(default)]
    pub snapshot: Option<RunSnapshot>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub source_task_id: Option<String>,
    #[serde(default)]
    pub isolated: bool,
    #[serde(default)]
    pub workspaces: Vec<RunWorkspace>,
    #[serde(default)]
    pub task_name: String,
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern("^(codex|claude|opencode)$"))]
    #[ts(optional = nullable)]
    pub provider: Option<AgentProvider>,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub preview: Option<String>,
    #[serde(default)]
    #[ts(type = "Record<string, number> | null")]
    #[ts(optional = nullable)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "RunUsage"))]
    pub usage: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "ContextUsage"))]
    #[ts(optional = nullable)]
    #[ts(type = "import('../../chats').ContextUsage | null")]
    pub context_usage: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "ChatWakeup"))]
    #[ts(optional = nullable)]
    #[ts(type = "import('../../chats').ChatWakeup | null")]
    pub wakeup: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "ChatExecution"))]
    #[ts(optional = nullable)]
    #[ts(type = "import('../../chats').ChatExecution | null")]
    pub chat_execution: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "TaskOutcome"))]
    #[ts(optional = nullable)]
    #[ts(type = "import('../../contracts').TaskOutcome | null")]
    pub outcome: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "OutcomeDismissal"))]
    #[ts(type = "{ until: number | null } | null")]
    #[ts(optional = nullable)]
    pub dismissal: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "NodeResources"))]
    #[ts(optional = nullable)]
    #[ts(type = "import('../../nodes').NodeResources | null")]
    pub resources: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "NativeModeState"))]
    #[ts(optional = nullable)]
    #[ts(type = "import('../../nodes').NativeModeRequest | null")]
    pub native_mode: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "ExclusiveReservation"))]
    #[ts(optional = nullable)]
    #[ts(type = "import('../../nodes').ExclusiveReservation | null")]
    pub suspended_by_exclusive: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "NodeBackup"))]
    #[ts(optional = nullable)]
    #[ts(type = "import('../../run-boundaries').RunBackup | null")]
    pub backup: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(with = "Option<std::collections::BTreeMap<String, serde_json::Value>>", extend("x-kotlin-type" = "RunStorage"))]
    #[ts(optional = nullable)]
    #[ts(type = "import('../../run-boundaries').RunStorage | null")]
    pub storage: Option<serde_json::Value>,
    #[serde(default)]
    #[ts(type = "\"firecracker\" | \"native\" | null")]
    #[ts(optional = nullable)]
    pub execution_backend: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub node_id: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub node_state: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub pinned_node_id: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub preferred_node_id: Option<String>,
    #[serde(default)]
    #[ts(type = "number | null")]
    #[ts(optional = nullable)]
    pub restored_at: Option<i64>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub movement_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunWorkspace {
    pub project_id: String,
    pub path: String,
    #[serde(default)]
    pub revision: Option<String>,
    #[ts(type = "\"worktree\" | \"clone\" | \"copy\" | \"direct\"")]
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListRunsInput {
    #[serde(default = "run_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub limit: u16,
    #[serde(default, deserialize_with = "wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub offset: i64,
    #[serde(default)]
    #[schemars(
        with = "Option<String>",
        pattern("^(queued|running|waiting|succeeded|failed|cancelled|interrupted)$")
    )]
    pub status: Option<RunStatus>,
    #[serde(default)]
    pub task_id: Option<MissionId>,
}

fn run_limit() -> u16 {
    20
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetRunInput {
    pub run_id: RunId,
    #[serde(default, deserialize_with = "wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub after: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadRunContentInput {
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1_i64, max = 9007199254740991_i64))]
    #[ts(type = "number | null")]
    pub event_id: Option<i64>,
    #[serde(default, deserialize_with = "wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub offset: i64,
    #[serde(default)]
    #[schemars(pattern("^[0-9a-f]{64}$"))]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunHistoryOutput {
    /// Existing byte-bounded preview; full metadata is read through read_run_content.
    pub run: RunMetadataPage,
    /// Engine/event payloads are owned by A7, not the run-transition conversion.
    #[ts(type = "Record<string, unknown>[]")]
    pub events: Vec<serde_json::Value>,
    #[ts(type = "number")]
    pub next_after: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunContentOutput {
    pub run_id: String,
    #[ts(type = "number | null")]
    pub event_id: Option<i64>,
    pub encoding: String,
    pub format: String,
    #[ts(type = "number")]
    pub offset: u64,
    #[ts(type = "number")]
    pub total_bytes: u64,
    pub sha256: String,
    pub data: String,
    #[ts(type = "number | null")]
    pub next_offset: Option<u64>,
}

/// A byte-bounded metadata page is either complete or an explicit preview. The
/// full record remains available through read_run_content in either case.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RunMetadataPage {
    Complete {
        run: Box<Run>,
    },
    Truncated {
        preview: RunPreview,
        #[ts(type = "number")]
        total_bytes: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunPreview {
    pub id: String,
    pub task_id: String,
    pub project_id: Option<String>,
    #[schemars(
        with = "String",
        pattern("^(queued|running|waiting|succeeded|failed|cancelled|interrupted)$")
    )]
    pub status: RunStatus,
    pub trigger: String,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    #[ts(type = "number | null")]
    pub finished_at: Option<i64>,
}

impl ListRunsInput {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=100).contains(&self.limit) || self.offset < 0 {
            return Err("Invalid run page.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_corpus_preserves_defaults_and_rejects_invalid_commands() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/run-contracts.json"))
                .unwrap();
        for value in corpus["commands"].as_array().unwrap() {
            assert!(serde_json::from_value::<RunInput>(value.clone()).is_ok());
        }
        for value in corpus["invalidCommands"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<RunInput>(value.clone()).is_err(),
                "{value}"
            );
        }
        for value in corpus["lists"].as_array().unwrap() {
            serde_json::from_value::<ListRunsInput>(value.clone())
                .unwrap()
                .validate()
                .unwrap();
        }
        for value in corpus["invalidLists"].as_array().unwrap() {
            assert!(
                serde_json::from_value::<ListRunsInput>(value.clone())
                    .map_or(true, |input| input.validate().is_err()),
                "{value}"
            );
        }
        for value in corpus["states"].as_array().unwrap() {
            let state: RunState = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                serde_json::from_value::<RunState>(serde_json::to_value(&state).unwrap()).unwrap(),
                state
            );
        }
        for value in corpus["retries"].as_array().unwrap() {
            let retry: Retry = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                serde_json::from_value::<Retry>(serde_json::to_value(&retry).unwrap()).unwrap(),
                retry
            );
        }
    }
}
