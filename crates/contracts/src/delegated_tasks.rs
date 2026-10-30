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
    /// Omission inherits the requester project. An explicit choice can clear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project: Option<DelegationProject>,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DelegationProject {
    Inherit,
    None,
    Selected { project_id: ProjectId },
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
        if self.model.as_ref().is_some_and(|model| {
            model.chars().count() > 120
                || !model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_./:[]-".contains(c))
        }) || self.reasoning.as_ref().is_some_and(|reasoning| {
            !reasoning.is_empty()
                && (reasoning.chars().count() > 40
                    || !reasoning.starts_with(|c: char| c.is_ascii_lowercase())
                    || !reasoning
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_-".contains(c)))
        }) {
            return Err("Invalid model or reasoning choice.".into());
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskCancelInput {
    pub chat_id: ConversationId,
}
