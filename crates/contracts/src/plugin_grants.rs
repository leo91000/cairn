use crate::*;

/// A named contribution. Names are the manifest names, including conditions and sources.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "camelCase")]
pub enum PluginContributionKind {
    Tool,
    Command,
    Hook,
    RunCredentials,
    Instructions,
    Condition,
    ProjectSource,
    SkillSource,
}

impl PluginContributionKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Command => "command",
            Self::Hook => "hook",
            Self::RunCredentials => "runCredentials",
            Self::Instructions => "instructions",
            Self::Condition => "condition",
            Self::ProjectSource => "projectSource",
            Self::SkillSource => "skillSource",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginContributionGrant {
    pub kind: PluginContributionKind,
    /// Empty for instructions and runCredentials, which are single contributions.
    #[schemars(length(max = 200))]
    pub name: String,
    /// Empty means unrestricted. Each listed type restricts that type and descendants.
    #[schemars(length(max = 100))]
    pub resources: Vec<PluginResourceRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum PluginGrantAccess {
    Full,
    Selected {
        #[schemars(length(min = 1, max = 500))]
        contributions: Vec<PluginContributionGrant>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginInstanceGrant {
    pub instance_id: PluginInstanceId,
    pub access: PluginGrantAccess,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentPluginGrants {
    pub agent_id: AgentId,
    /// Shares the agent profile revision, so old profile editors cannot overwrite a change.
    pub revision: String,
    pub grants: Vec<PluginInstanceGrant>,
    /// At most one supplying instance per package. A full grant alone is insufficient.
    pub run_credential_instances: Vec<PluginInstanceId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceAgentPluginGrantsInput {
    pub agent_id: AgentId,
    pub revision: String,
    #[schemars(length(max = 100))]
    pub grants: Vec<PluginInstanceGrant>,
    #[schemars(length(max = 100))]
    pub run_credential_instances: Vec<PluginInstanceId>,
}
