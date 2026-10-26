use crate::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[schemars(inline)]
pub struct AgentId(pub Uuid);

impl std::fmt::Display for AgentId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentProvider {
    #[default]
    Codex,
    Claude,
    Opencode,
}

/// Relational permission view: null is the explicit wildcard, distinct from an
/// empty selection. These generated wire models confer no actor authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessPolicy {
    #[serde(default = "firecracker")]
    #[schemars(pattern("^(firecracker|native)$"))]
    #[ts(type = "\"firecracker\" | \"native\"")]
    pub backend: String,
    #[serde(default = "local_nodes")]
    #[schemars(with = "Option<Vec<Uuid>>", length(max = 100))]
    pub nodes: Option<Vec<String>>,
    #[serde(default)]
    #[schemars(with = "Option<Vec<Uuid>>", length(max = 100))]
    pub projects: Option<Vec<String>>,
    #[serde(default)]
    #[schemars(length(max = 100), inner(length(max = 160)))]
    pub skills: Option<Vec<String>>,
    #[serde(default)]
    #[schemars(with = "Option<Vec<Uuid>>", length(max = 100))]
    pub mcps: Option<Vec<String>>,
    #[schemars(extend(
        "propertyNames" = schemars::json_schema!({
            "type": "string",
            "format": "uuid",
        }),
        "additionalProperties" = schemars::json_schema!({
            "type": "array",
            "maxItems": 500,
            "items": {
                "type": "string",
                "minLength": 1,
                "maxLength": 200,
            },
        })
    ))]
    pub mcp_tools: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub installation: bool,
    #[serde(default)]
    pub delegation: bool,
    #[serde(default = "yolo")]
    #[schemars(pattern("^(yolo|workspace-write|read-only)$"))]
    #[ts(type = "\"yolo\" | \"workspace-write\" | \"read-only\"")]
    pub sandbox: String,
}

fn firecracker() -> String {
    "firecracker".into()
}

fn yolo() -> String {
    "yolo".into()
}

fn local_nodes() -> Option<Vec<String>> {
    Some(vec!["00000000-0000-4000-8000-000000000002".into()])
}

impl Default for AccessPolicy {
    fn default() -> Self {
        Self {
            backend: firecracker(),
            nodes: local_nodes(),
            projects: None,
            skills: None,
            mcps: None,
            mcp_tools: BTreeMap::new(),
            installation: false,
            delegation: false,
            sandbox: yolo(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSettings {
    #[schemars(length(min = 1, max = 100))]
    pub name: String,
    #[serde(default)]
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
    #[serde(default)]
    #[schemars(length(max = 500))]
    pub description: String,
    #[serde(default)]
    #[schemars(length(max = 100))]
    pub model: String,
    #[serde(default = "high")]
    #[schemars(length(max = 40), pattern("^(?:[a-z][a-z0-9_-]*)?$"))]
    pub reasoning: String,
    #[serde(default)]
    #[schemars(length(max = 40), pattern("^(?:[a-z][a-z0-9_-]*)?$"))]
    pub speed: String,
    #[serde(default)]
    #[schemars(length(max = 20000))]
    pub instructions: String,
    #[serde(default)]
    #[schemars(range(min = 0, max = 720))]
    pub timeout_minutes: u16,
    pub access: AccessPolicy,
    #[schemars(with = "Vec<Uuid>", length(max = 100))]
    pub exclusive_nodes: Vec<String>,
    #[schemars(with = "Vec<Uuid>", length(max = 100))]
    pub plugins: Vec<String>,
}

fn high() -> String {
    "high".into()
}

impl AgentSettings {
    pub fn validate(&mut self) -> Result<(), &'static str> {
        self.name = self.name.trim().into();
        if self.name.is_empty() || self.name.chars().count() > 100 {
            return Err("Enter an agent name of 1 to 100 characters.");
        }
        if self.description.chars().count() > 500
            || self.model.chars().count() > 100
            || self.instructions.chars().count() > 20000
        {
            return Err("Agent text exceeds its maximum length.");
        }
        if self.timeout_minutes > 720 {
            return Err("Choose a time limit between 0 and 720 minutes.");
        }
        for (value, message) in [
            (&self.reasoning, "Enter a valid reasoning effort."),
            (&self.speed, "Enter a valid speed."),
        ] {
            let valid = value.is_empty()
                || value.bytes().enumerate().all(|(i, c)| {
                    c.is_ascii_lowercase()
                        || (i > 0 && (c.is_ascii_digit() || c == b'_' || c == b'-'))
                });
            if value.len() > 40 || !valid {
                return Err(message);
            }
        }
        if !matches!(self.access.backend.as_str(), "native" | "firecracker")
            || !matches!(
                self.access.sandbox.as_str(),
                "yolo" | "workspace-write" | "read-only"
            )
        {
            return Err("Choose a supported execution mode.");
        }
        for ids in [&self.access.nodes, &self.access.projects, &self.access.mcps]
            .into_iter()
            .flatten()
            .chain([&self.exclusive_nodes, &self.plugins])
        {
            if ids.len() > 100 || ids.iter().any(|id| Uuid::parse_str(id).is_err()) {
                return Err("Select at most 100 valid resource IDs.");
            }
        }
        if self.access.skills.as_ref().is_some_and(|skills| {
            skills.len() > 100 || skills.iter().any(|s| s.chars().count() > 160)
        }) {
            return Err("Select at most 100 valid skills.");
        }
        for (id, tools) in &self.access.mcp_tools {
            if Uuid::parse_str(id).is_err()
                || tools.len() > 500
                || tools
                    .iter()
                    .any(|s| s.is_empty() || s.chars().count() > 200)
            {
                return Err("Select valid MCP tools.");
            }
        }
        for selection in [
            &mut self.access.projects,
            &mut self.access.skills,
            &mut self.access.nodes,
            &mut self.access.mcps,
        ]
        .into_iter()
        .flatten()
        {
            let mut seen = std::collections::HashSet::new();
            selection.retain(|id| seen.insert(id.clone()));
        }
        for tools in self.access.mcp_tools.values_mut() {
            let mut seen = std::collections::HashSet::new();
            tools.retain(|tool| seen.insert(tool.clone()));
        }
        let mut seen_plugins = std::collections::HashSet::new();
        self.plugins.retain(|id| seen_plugins.insert(id.clone()));
        self.exclusive_nodes.retain(|node| {
            self.access.backend != "native"
                && self
                    .access
                    .nodes
                    .as_ref()
                    .is_none_or(|nodes| nodes.contains(node))
        });
        let mut seen_exclusive_nodes = std::collections::HashSet::new();
        self.exclusive_nodes
            .retain(|id| seen_exclusive_nodes.insert(id.clone()));
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentAvatar {
    #[schemars(pattern("^(generating|ready|failed)$"))]
    #[ts(type = "\"generating\" | \"ready\" | \"failed\"")]
    pub status: String,
    pub revision: String,
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: AgentId,
    #[serde(flatten)]
    #[ts(flatten)]
    pub settings: AgentSettings,
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub avatar: Option<AgentAvatar>,
    #[serde(deserialize_with = "wire_integer")]
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[ts(type = "number")]
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentReview {
    #[serde(default)]
    #[schemars(with = "Vec<Uuid>", length(max = 100))]
    pub native_node_reviews: Vec<String>,
    #[serde(default)]
    #[schemars(with = "Vec<Uuid>", length(max = 100))]
    pub reapprove_delegations: Vec<String>,
}

impl AgentReview {
    pub fn validate(&self) -> Result<(), &'static str> {
        for ids in [&self.native_node_reviews, &self.reapprove_delegations] {
            if ids.len() > 100 || ids.iter().any(|id| Uuid::parse_str(id).is_err()) {
                return Err("Review at most 100 valid resource IDs.");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAgentInput {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "agent_creation_id"
    )]
    #[schemars(with = "AgentId")]
    #[ts(optional)]
    pub creation_id: Option<AgentId>,
    pub settings: AgentSettings,
    pub review: AgentReview,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceAgentInput {
    pub agent_id: AgentId,
    pub revision: String,
    pub settings: AgentSettings,
    pub review: AgentReview,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentInput {
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListAgentsInput {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListAgentsOutput {
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteAgentOutput {
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AvatarConfiguration {
    pub configured: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetAgentAvatarInput {
    pub agent_id: AgentId,
    /// Base64 PNG, JPEG or WebP, at most 5 MiB before normalization.
    #[schemars(length(max = 6990508))]
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentPreviewRuns {
    pub queued: u32,
    pub running: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentDelegationEffect {
    pub id: String,
    pub plugin_id: String,
    pub name: String,
    pub enabled: bool,
    pub suspended: bool,
    pub blocked: Option<String>,
    pub already_suspended: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentPreview {
    pub runs: AgentPreviewRuns,
    pub delegations: Vec<AgentDelegationEffect>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MergeAgentsInput {
    pub source_id: AgentId,
    pub target_id: AgentId,
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentTransfers {
    pub tasks: u32,
    pub chats: u32,
    pub runs: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MergeAgentsOutput {
    pub source_id: AgentId,
    pub target_id: AgentId,
    pub transferred: AgentTransfers,
    pub deleted: bool,
}

fn agent_creation_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<AgentId>, D::Error> {
    let id = AgentId::deserialize(deserializer)?;
    if id.0.get_version() != Some(uuid::Version::Random) {
        return Err(serde::de::Error::custom("creationId must be a random UUID"));
    }
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_corpus_shares_defaults_and_rejects_invalid_profiles() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/agent-contracts.json"))
                .unwrap();
        for value in corpus["creates"].as_array().unwrap() {
            let mut input: CreateAgentInput = serde_json::from_value(value.clone()).unwrap();
            input.settings.validate().unwrap();
            input.review.validate().unwrap();
            assert_eq!(
                serde_json::from_value::<CreateAgentInput>(serde_json::to_value(&input).unwrap())
                    .unwrap(),
                input
            );
        }
        for value in corpus["invalidCreates"].as_array().unwrap() {
            assert!(
                match serde_json::from_value::<CreateAgentInput>(value.clone()) {
                    Err(_) => true,
                    Ok(mut input) =>
                        input.settings.validate().is_err() || input.review.validate().is_err(),
                },
                "{value}"
            );
        }
    }
}
