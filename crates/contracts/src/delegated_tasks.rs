//! Cairn delegated conversations, distinct from engine-native subagents.
use crate::*;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegateTaskInput {
    #[schemars(length(min = 1, max = 50000))]
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schemars(length(min = 1, max = 90))]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_id: Option<AgentId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "project_choice"
    )]
    #[schemars(with = "Option<ProjectId>")]
    #[ts(optional = nullable,type="string | null")]
    pub project_id: Option<Option<ProjectId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>", pattern("^(codex|claude|opencode)$"))]
    #[ts(optional)]
    pub provider: Option<AgentProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 120), pattern(r"^[\w./:\[\]-]*$"))]
    #[ts(optional)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 40), pattern("^(?:[a-z][a-z0-9_-]*)?$"))]
    #[ts(optional)]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "uuid::Uuid")]
    #[ts(optional)]
    pub request_id: Option<String>,
}

fn project_choice<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<ProjectId>>, D::Error> {
    Option::<ProjectId>::deserialize(deserializer).map(Some)
}

impl DelegateTaskInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.task.trim().is_empty()
            || self.task.chars().count() > 50000
            || self
                .title
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.chars().count() > 90)
        {
            return Err(
                "Provide a self-contained task and a title of at most 90 characters.".into(),
            );
        }
        if let Some(id) = &self.request_id {
            uuid::Uuid::parse_str(id).map_err(|_| "Invalid delegation request identity.")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskStatusInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chat_id: Option<ConversationId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedTask {
    pub chat_id: String,
    pub title: String,
    pub agent_id: String,
    pub agent_name: String,
    pub project_id: Option<String>,
    #[schemars(with = "Option<String>", pattern("^(codex|claude|opencode)$"))]
    pub provider: Option<AgentProvider>,
    pub model: Option<String>,
    #[schemars(
        with = "String",
        pattern("^(idle|queued|running|waiting|succeeded|failed|cancelled|interrupted)$")
    )]
    pub status: ConversationStatus,
    #[schemars(with = "String", pattern("^(active|trash|purging)$"))]
    pub lifecycle: ConversationLifecycle,
    pub finished: bool,
    pub paused: bool,
    pub error: Option<String>,
    pub pending_questions: u32,
    #[schemars(with="Option<std::collections::BTreeMap<String,serde_json::Value>>",extend("x-kotlin-type"="TaskOutcome"))]
    #[ts(type = "import('../../contracts').TaskOutcome | null")]
    pub outcome: Option<serde_json::Value>,
    pub run_id: Option<String>,
    #[ts(type = "number | null")]
    pub finished_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub report: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct TaskStatusOutput {
    pub tasks: Vec<DelegatedTask>,
}
