use crate::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[schemars(inline)]
pub struct MissionId(pub Uuid);

impl std::fmt::Display for MissionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MissionSettings {
    #[schemars(length(min = 1, max = 100))]
    pub name: String,
    #[schemars(length(min = 1, max = 50000))]
    pub prompt: String,
    pub agent_id: AgentId,
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    #[serde(default)]
    #[schemars(length(max = 100), inner(length(max = 160)))]
    pub skills: Option<Vec<String>>,
    #[serde(default)]
    #[schemars(length(max = 10), inner(length(max = 30)))]
    pub tags: Vec<String>,
    #[serde(default)]
    #[schemars(length(max = 100))]
    pub cron: Option<String>,
    #[serde(default = "timezone")]
    #[schemars(length(min = 1, max = 100))]
    pub timezone: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default = "yes")]
    pub worktree: bool,
}

fn timezone() -> String {
    "Europe/Paris".into()
}

fn yes() -> bool {
    true
}

impl MissionSettings {
    pub fn validate(&mut self) -> Result<(), &'static str> {
        self.name = self.name.trim().into();
        self.prompt = self.prompt.trim().into();
        if self.name.is_empty() || self.name.chars().count() > 100 {
            return Err("Enter a mission name of 1 to 100 characters.");
        }
        if self.prompt.is_empty() || self.prompt.chars().count() > 50000 {
            return Err("Enter a prompt of 1 to 50000 characters.");
        }
        if self
            .skills
            .as_ref()
            .is_some_and(|s| s.len() > 100 || s.iter().any(|s| s.chars().count() > 160))
        {
            return Err("Select at most 100 skills of at most 160 characters.");
        }
        if self.tags.len() > 10 || self.tags.iter().any(|s| s.chars().count() > 30) {
            return Err("Enter at most 10 tags of at most 30 characters.");
        }
        if self.cron.as_ref().is_some_and(|c| c.chars().count() > 100)
            || self.timezone.is_empty()
            || self.timezone.chars().count() > 100
        {
            return Err("Enter a valid cron expression and timezone.");
        }
        Ok(())
    }

    pub fn scheduled(&self) -> bool {
        self.enabled && !self.archived && self.cron.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateMissionInput {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "creation"
    )]
    #[schemars(with = "MissionId")]
    #[ts(optional)]
    pub creation_id: Option<MissionId>,
    pub settings: MissionSettings,
}

fn creation<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<MissionId>, D::Error> {
    let id = MissionId::deserialize(d)?;
    if id.0.get_version() != Some(uuid::Version::Random) {
        return Err(serde::de::Error::custom("creationId must be a random UUID"));
    }
    Ok(Some(id))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaceMissionInput {
    pub task_id: MissionId,
    pub revision: String,
    pub settings: MissionSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MissionInput {
    pub task_id: MissionId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MissionStateInput {
    pub task_id: MissionId,
    pub revision: String,
    pub enabled: bool,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchedulePreviewInput {
    #[schemars(length(min = 1, max = 100))]
    pub cron: String,
    #[schemars(length(min = 1, max = 100))]
    pub timezone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchedulePreviewOutput {
    #[schemars(inner(range(min = 0_i64, max = 9007199254740991_i64)))]
    #[ts(type = "number[]")]
    pub occurrences: Vec<i64>,
}

// Provenance retained at the native-node boundary until that domain is converted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MissionPluginGrant {
    pub connection_id: String,
    pub revision: String,
    pub author_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Mission {
    pub id: MissionId,
    #[serde(flatten)]
    #[ts(flatten)]
    pub settings: MissionSettings,
    pub revision: String,
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[serde(deserialize_with = "wire_integer")]
    #[ts(type = "number")]
    pub created_at: i64,
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[ts(type = "number | null")]
    pub next_run: Option<i64>,
    pub author_id: Option<String>,
    pub author_access_id: Option<String>,
    pub author_removed: bool,
    pub schedule_wait_reason: Option<String>,
    pub schedule_failure: Option<ScheduleFailure>,
    // Typed provenance at the boundary of the unconverted native-node domain (A10).
    pub native_principals: Vec<String>,
    pub native_authority_nodes: Vec<String>,
    pub native_parent_run_id: Option<String>,
    pub native_parent_access: Option<String>,
    pub native_provenance_unknown: bool,
    pub native_plugin_grants: Option<Vec<MissionPluginGrant>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleFailure {
    pub message: String,
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[serde(deserialize_with = "wire_integer")]
    #[ts(type = "number")]
    pub scheduled_for: i64,
    #[schemars(range(min = 0_i64, max = 9007199254740991_i64))]
    #[serde(deserialize_with = "wire_integer")]
    #[ts(type = "number")]
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListMissionsInput {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListMissionsOutput {
    pub tasks: Vec<Mission>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteMissionOutput {
    pub deleted: bool,
}

/// Presentation of task settings in a run snapshot. Chats synthesize the same typed
/// settings without creating a mission or a scheduled commitment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunTaskSnapshot {
    pub id: MissionId,
    #[serde(flatten)]
    #[ts(flatten)]
    pub settings: MissionSettings,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mission_contracts_qualify_defaults_null_selections_and_bounds() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mission-contracts.json"
        ))
        .unwrap();
        for value in corpus["creates"].as_array().unwrap() {
            let mut input: CreateMissionInput = serde_json::from_value(value.clone()).unwrap();
            input.settings.validate().unwrap();
            assert_eq!(
                serde_json::from_value::<CreateMissionInput>(serde_json::to_value(&input).unwrap())
                    .unwrap(),
                input
            );
        }
        for value in corpus["invalidCreates"]
            .as_array()
            .unwrap()
            .iter()
            .chain(corpus["serverInvalid"].as_array().unwrap())
        {
            assert!(
                serde_json::from_value::<CreateMissionInput>(value.clone())
                    .map_or(true, |mut input| input.settings.validate().is_err()),
                "{value}"
            );
        }
    }
}
