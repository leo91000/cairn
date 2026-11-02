//! Native configuration documents, shared by the form, text editor and tools.
use crate::*;

/// Safe display defaults. These are inherited preferences, never profile overrides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct CodingAgentPreferences {
    pub model: String,
    pub reasoning: String,
    pub speed: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodingAgentConfigurationInput {
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodingAgentSettingChange {
    /// JSON pointer, including for TOML documents.
    #[schemars(length(min = 1, max = 500))]
    pub path: String,
    /// A JSON value; null removes the setting and restores the inherited value.
    #[schemars(length(max = 200000))]
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditCodingAgentConfigurationInput {
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    #[schemars(length(max = 200))]
    pub revision: String,
    #[schemars(length(max = 200000))]
    pub document: String,
    #[serde(default)]
    #[schemars(length(max = 200))]
    pub changes: Vec<CodingAgentSettingChange>,
    /// Explicitly restore inheritance even when its value equals a customization.
    #[serde(default)]
    #[schemars(length(max = 1000), inner(length(max = 500)))]
    pub inherit_paths: Vec<String>,
    #[serde(default)]
    pub reset: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodingAgentSetting {
    pub path: String,
    pub label: String,
    pub description: String,
    pub group: String,
    /// text, integer, boolean or select.
    pub kind: String,
    pub choices: Vec<String>,
    /// Serialized JSON, or null when the engine chooses its own default.
    pub value: Option<String>,
    pub inherited: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedCodingAgentSetting {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodingAgentConfiguration {
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
    pub agent_id: Option<AgentId>,
    pub agent_name: Option<String>,
    pub revision: String,
    pub filename: String,
    pub document: String,
    pub inherited_document: String,
    pub customized_paths: Vec<String>,
    pub settings: Vec<CodingAgentSetting>,
    pub managed: Vec<ManagedCodingAgentSetting>,
    pub notices: Vec<String>,
}

/// Frozen per message. Contains editable settings only, never credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeConfigurationSnapshot {
    #[schemars(with = "String", pattern("^(codex|claude|opencode)$"))]
    pub provider: AgentProvider,
    pub document: String,
    pub revision: String,
    #[serde(default)]
    pub configured_paths: Vec<String>,
}
