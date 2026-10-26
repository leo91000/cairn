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
    pub ok: bool,
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
    pub error: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub workspace_cleaned_at: Option<i64>,
    #[serde(default)]
    pub recovery_pending: bool,
    #[serde(default)]
    pub resume_available: bool,
    #[serde(default)]
    pub resume_count: u32,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub cancel_requested_at: Option<i64>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub account_name: Option<String>,
    #[serde(default)]
    pub account_wait_reason: Option<String>,
    #[serde(default)]
    pub account_required: Option<AgentProvider>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub capacity_wait_until: Option<i64>,
    #[serde(default)]
    pub retry: Option<Retry>,
    #[serde(default)]
    pub requested_by: Option<RunRequester>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunRequester {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task_id: Option<String>,
}
